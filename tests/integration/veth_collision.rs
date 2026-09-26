//! A foreign interface that already holds the name Enclave would use.
//!
//! A host veth is named from the workspace and the address it is given, so the name is
//! predictable. That makes a collision possible, and the dangerous half of it is not the
//! creation failing: the creation failure path removes the interface by name, so an
//! interface Enclave did not create is what would be deleted.

use std::process::Command;

use enclave::sandbox::{create_sandbox, start_sandbox, stop_sandbox, BootstrapMethod};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
};

use super::support::{prepare_cached_rootfs, root_only, state_dir, SandboxCleanup};

fn ip(args: &[&str]) -> std::process::Output {
    Command::new("ip")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run ip {args:?}: {error}"))
}

fn interface_exists(name: &str) -> bool {
    ip(&["link", "show", name]).status.success()
}

/// The name Enclave gives a workspace host interface.
///
/// The test builds it from the ids rather than reaching into the crate, because the
/// naming is a contract between the setup path and the cleanup path: a cleanup that
/// looked for a different name would leave the interface behind, and a setup that
/// produced a different name would collide with something else.
fn host_veth_name(octet: u8, workspace_id: &str) -> String {
    let hash = workspace_id.bytes().fold(0x811c9dc5u32, |hash, byte| {
        hash.wrapping_mul(0x01000193) ^ u32::from(byte)
    }) & 0x00ff_ffff;
    format!("veth-{octet}-{hash:06x}")
}

/// Starting a workspace must never delete an interface it did not create.
///
/// The foreign interface is a dummy with the exact name the start will want, so the
/// start has to fail rather than adopt it, and the interface has to still be there
/// afterwards with its marker address intact.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_foreign_interface_with_the_workspace_name_is_never_deleted() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-veth-collision");
    prepare_cached_rootfs(&state, "bookworm");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-veth-collision-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");

    // The first start is how the test learns which address the workspace is given: the
    // octet is the part of the interface name that comes from the allocation.
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("first start");
    let assigned = started
        .assigned_ip
        .clone()
        .expect("a started workspace holds an address");
    let octet = assigned
        .rsplit('.')
        .next()
        .and_then(|last| last.parse::<u8>().ok())
        .expect("the assigned address ends in an octet");
    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");

    let foreign = host_veth_name(octet, &workspace.id);
    // A dummy interface rather than a veth, so the name is held by something that is
    // unmistakably not a workspace pair, and a marker address that a deletion would take
    // with it.
    let created = ip(&["link", "add", &foreign, "type", "dummy"]);
    assert!(
        created.status.success(),
        "failed to seed the foreign interface {foreign}: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let marked = ip(&["addr", "add", "192.0.2.77/32", "dev", &foreign]);
    assert!(
        marked.status.success(),
        "failed to mark the foreign interface"
    );

    // The start has to refuse: the name is taken by something Enclave did not create, so
    // it cannot build its pair under it.
    let error = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect_err("a start whose interface name is taken must fail");
    let message = format!("{error:#}");
    assert!(
        message.contains(&foreign),
        "the failure must name the interface it could not create: {message}"
    );

    // And the foreign interface is untouched: still present, still holding its address.
    assert!(
        interface_exists(&foreign),
        "the failed start deleted the foreign interface {foreign}"
    );
    let addresses = ip(&["-o", "addr", "show", "dev", &foreign]);
    assert!(
        String::from_utf8_lossy(&addresses.stdout).contains("192.0.2.77"),
        "the failed start removed the foreign interface address: {}",
        String::from_utf8_lossy(&addresses.stdout)
    );

    // The workspace is usable once the name is free, which is what makes the refusal a
    // refusal rather than a break.
    let removed = ip(&["link", "del", &foreign]);
    assert!(
        removed.status.success(),
        "failed to remove the foreign interface"
    );
    let restarted = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("the workspace must start once the name is free");
    assert!(restarted.runtime_pid.is_some());

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    drop(cleanup);
}

/// A leftover of Enclave's own must not block a start.
///
/// A teardown that did not finish leaves the workspace interface attached to the bridge,
/// which is the state a crash leaves. The start removes it rather than failing, because
/// otherwise the operator's next command would fail until they removed an interface by
/// hand. The leftover here is built the way Enclave builds one, so it is
/// indistinguishable from a real one: a veth, named from the workspace, enslaved to the
/// bridge.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_leftover_workspace_interface_is_replaced_by_the_next_start() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-veth-leftover");
    prepare_cached_rootfs(&state, "bookworm");
    let mut cleanup = SandboxCleanup::new(state.clone());
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-veth-leftover-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");

    // The first start is how the test learns the workspace's address, and it also builds
    // the bridge, which is what makes the leftover below attachable to it.
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("first start");
    let assigned = started.assigned_ip.clone().expect("assigned address");
    let octet = assigned
        .rsplit('.')
        .next()
        .and_then(|last| last.parse::<u8>().ok())
        .expect("an address ending in an octet");
    let name = host_veth_name(octet, &workspace.id);
    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    assert!(!interface_exists(&name), "the stop left {name} behind");

    // A pair named the way Enclave names one, with the host end attached to the bridge.
    // The peer is given a name the test owns so the deletion of the pair cannot collide
    // with anything.
    let created = ip(&[
        "link",
        "add",
        &name,
        "type",
        "veth",
        "peer",
        "name",
        "leftover-peer",
    ]);
    assert!(
        created.status.success(),
        "failed to build the leftover pair: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let attached = ip(&["link", "set", &name, "master", "enclave0"]);
    assert!(
        attached.status.success(),
        "failed to attach the leftover to the bridge: {}",
        String::from_utf8_lossy(&attached.stderr)
    );
    // The fixture is on the bridge the rest of the host shares, so it is removed
    // however the test ends. A panic before the start below would otherwise leave an
    // interface holding an address that no record names, which nothing would release.
    let _fixture = LeftoverFixture { name: name.clone() };

    // The start replaces it instead of failing, and the workspace is usable.
    let restarted = start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("a leftover workspace interface must be replaced, not block the start");
    assert!(restarted.runtime_pid.is_some());
    assert!(
        interface_exists(&name),
        "the start did not build its own interface"
    );
    // The leftover was replaced rather than routed around: the pair the test built is
    // gone, which is also what proves the address was reused instead of a second one
    // being taken. Deleting the host end destroys both ends, so the peer is the evidence
    // that the interface now on the bridge is not the one the test built.
    assert!(
        !interface_exists("leftover-peer"),
        "the start left the fixture's pair in place; the leftover was not replaced"
    );
    assert_eq!(
        restarted.assigned_ip.as_deref(),
        Some(assigned.as_str()),
        "the workspace must keep its address rather than be given another one"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    drop(cleanup);
}

/// The leftover pair this test builds, removed however the test ends.
///
/// The pair is attached to the bridge Enclave shares with every other workspace on
/// the host, and a workspace veth on the bridge names an address that is in use. A run
/// that panics between building it and the start that replaces it would leave that
/// address held by an interface no record names, so the fixture takes itself away.
struct LeftoverFixture {
    name: String,
}

impl Drop for LeftoverFixture {
    fn drop(&mut self) {
        // Both ends, by name: deleting the host end destroys the pair, and naming the
        // peer as well covers a run that failed between creating the two.
        let _ = ip(&["link", "del", &self.name]);
        let _ = ip(&["link", "del", "leftover-peer"]);
    }
}
