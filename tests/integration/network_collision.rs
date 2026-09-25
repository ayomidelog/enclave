//! A host that already uses the Enclave subnet must be refused safely.
//!
//! The bridge address is fixed at 10.200.0.1/24, so a second interface holding an
//! address inside that subnet gives the kernel two connected routes for one prefix and
//! workspace traffic can leave through a network Enclave does not control, while every
//! workspace still reports clean anti-spoofing rules.
//!
//! The check that refuses this reads the host, so proving it needs a host that has the
//! conflict without putting one on the real machine. The test therefore runs itself
//! again under `unshare -n`: the second process is in a network namespace of its own,
//! where the address it adds and the bridge it would create exist only there and are
//! released when it exits. A running daemon is a different process and is untouched.
//!
//! It has to be a second process rather than a second thread, for two reasons. The
//! readiness of host networking is cached in a process-wide flag, and once any other
//! test has started a workspace the check is skipped in favour of an existing bridge.
//! And the check asks whether the bridge exists by reading /sys/class/net, which on
//! this host lists every interface on the machine whatever namespace the reader is in,
//! so a thread in a fresh namespace would still see the real bridge.

use std::process::Command;

use enclave::sandbox::{create_sandbox, start_sandbox, BootstrapMethod};
use enclave::workspace::{create_workspace, start_workspace, WorkspaceLimits};

use super::support::{prepare_cached_rootfs, root_only, state_dir, SandboxCleanup};

/// Set in the process that `unshare` put in a namespace of its own.
const CHILD_ENV: &str = "ENCLAVE_SUBNET_COLLISION_CHILD";
const TEST_PATH: &str =
    "integration::network_collision::a_host_already_using_the_enclave_subnet_refuses_the_start_cleanly";

const SUBNET_CIDR: &str = "10.200.0.0/24";
const COMPETING_ADDRESS: &str = "10.200.0.200/24";
const BRIDGE_NAME: &str = "enclave0";

fn ip(args: &[&str]) -> std::process::Output {
    Command::new("ip")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run ip {args:?}: {error}"))
}

/// Interfaces in the current network namespace whose name starts with a prefix.
fn interfaces_named(prefix: &str) -> Vec<String> {
    let listed = ip(&["-o", "link", "show"]);
    assert!(
        listed.status.success(),
        "failed to list interfaces: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(_, rest)| rest.split(':').next().unwrap_or_default().to_string())
        .filter(|name| name.starts_with(prefix))
        .collect()
}

fn workspace_record(state: &std::path::Path, workspace_id: &str) -> (String, Option<String>) {
    enclave::registry::with_registry(state, |registry| {
        let workspace = registry
            .sandboxes
            .values()
            .find_map(|sandbox| sandbox.workspaces.get(workspace_id))
            .expect("the test's workspace record is gone");
        Ok((
            workspace.status.as_str().to_string(),
            workspace.assigned_ip.clone(),
        ))
    })
    .expect("read the registry")
}

/// The body that runs inside the namespace that already uses the Enclave subnet.
fn refuse_on_a_colliding_host() {
    let state = state_dir("enclave-int-subnet-collision");
    prepare_cached_rootfs(&state, "bookworm");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-collision-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    assert_eq!(workspace_record(&state, &workspace.id).0, "stopped");

    // A second interface on the Enclave subnet, which is the collision the check
    // exists to catch. A loopback interface is enough: the check asks which
    // interfaces hold an address in the subnet, not what kind they are.
    let added = ip(&["addr", "add", COMPETING_ADDRESS, "dev", "lo"]);
    assert!(
        added.status.success(),
        "failed to add the competing address: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let raised = ip(&["link", "set", "lo", "up"]);
    assert!(raised.status.success(), "failed to raise lo");

    let error = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect_err("a start on a host already using the Enclave subnet must fail");
    let message = format!("{error:#}");
    assert!(
        message.contains(SUBNET_CIDR),
        "the refusal must name the subnet it refused: {message}"
    );
    assert!(
        message.contains("lo"),
        "the refusal must name the interface holding the address: {message}"
    );
    assert!(
        message.contains("10.200.0.200"),
        "the refusal must name the address that collides: {message}"
    );

    // Nothing was created. The bridge is named by the check, so a refusal that had
    // already built one would be visible here, and the workspace interface is named
    // after its id, so a refusal that had already attached it would be too.
    assert!(
        interfaces_named(BRIDGE_NAME).is_empty(),
        "the refusal created the bridge it refused for"
    );
    assert!(
        interfaces_named("veth-").is_empty(),
        "the refusal attached a workspace interface before refusing"
    );

    // And the refusal is safe: the workspace is back to a state a later start can
    // use, with the address it reserved given back, so the operator's next command is
    // the one they would have run anyway.
    let (status, assigned_ip) = workspace_record(&state, &workspace.id);
    assert_eq!(
        status, "stopped",
        "the refused start left the workspace {status} with the address {assigned_ip:?} held"
    );
    assert_eq!(
        assigned_ip, None,
        "the refused start did not give its reserved address back"
    );
}

/// Starting a workspace on a host that already uses the Enclave subnet is refused,
/// and the refusal leaves nothing behind.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_host_already_using_the_enclave_subnet_refuses_the_start_cleanly() {
    if !root_only() {
        return;
    }
    if std::env::var_os(CHILD_ENV).is_some() {
        refuse_on_a_colliding_host();
        return;
    }

    // The parent's only job is to run the check somewhere the conflict is real and
    // nowhere else. Everything it asserts is asserted in the child, whose output is
    // printed when it fails.
    let binary = std::env::current_exe().expect("locate the running test binary");
    let output = Command::new("unshare")
        .arg("-n")
        .arg(&binary)
        .args([
            "--ignored",
            "--exact",
            "--test-threads=1",
            "--nocapture",
            TEST_PATH,
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("run the check in a network namespace of its own");
    assert!(
        output.status.success(),
        "the check failed inside its network namespace:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
