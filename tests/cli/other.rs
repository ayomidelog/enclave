//! The commands that are not about one workspace: daemon, sandbox, rootfs, snapshots.

use super::*;

#[test]
fn doctor_repair_and_explicit_daemon_start_parse() {
    let cli = Cli::parse_from(["enclave", "--start-daemon", "doctor", "--repair"]);
    assert!(cli.start_daemon);
    let Commands::Doctor(args) = cli.command else {
        panic!("expected doctor command");
    };
    assert!(args.repair);
}

#[test]
fn workspace_resize_parses_target_disk_size() {
    let cli = Cli::parse_from([
        "enclave",
        "workspace",
        "resize",
        "sb",
        "ws",
        "--disk-mb",
        "2048",
    ]);
    let Commands::Workspace { command } = cli.command else {
        panic!("expected workspace command");
    };
    let WorkspaceCommands::Resize(args) = command else {
        panic!("expected workspace resize command");
    };
    assert_eq!(args.sandbox, "sb");
    assert_eq!(args.workspace, "ws");
    assert_eq!(args.disk_mb, 2048);
}

#[test]
fn workspace_resize_requires_disk_size() {
    assert!(Cli::try_parse_from(["enclave", "workspace", "resize", "sb", "ws"]).is_err());
}

#[test]
fn sandbox_create_parses_limit_flags() {
    let cli = Cli::parse_from([
        "enclave",
        "create",
        "sb",
        "--cpu-percent",
        "40",
        "--memory-mb",
        "8192",
        "--max-procs",
        "1024",
    ]);
    let Commands::Create(args) = cli.command else {
        panic!("expected create command");
    };
    assert_eq!(args.cpu_percent, Some(40.0));
    assert_eq!(args.memory_mb, Some(8192));
    assert_eq!(args.max_procs, Some(1024));
}

#[test]
fn rootfs_export_parses_suite_and_output() {
    let cli = Cli::parse_from([
        "enclave",
        "rootfs",
        "export",
        "--suite",
        "bookworm",
        "--output",
        "/tmp/bookworm-rootfs.tar.gz",
    ]);
    let Commands::Rootfs { command } = cli.command else {
        panic!("expected rootfs command");
    };
    let RootfsCommands::Export(args) = command else {
        panic!("expected rootfs export command");
    };
    assert_eq!(args.suite.as_deref(), Some("bookworm"));
    assert!(!args.base);
    assert_eq!(
        args.output,
        std::path::PathBuf::from("/tmp/bookworm-rootfs.tar.gz")
    );
}

#[test]
fn rootfs_fetch_parses_base_url_and_replace() {
    let cli = Cli::parse_from([
        "enclave",
        "rootfs",
        "fetch",
        "--base",
        "--replace",
        "https://example.com/rootfs.tar.gz",
    ]);
    let Commands::Rootfs { command } = cli.command else {
        panic!("expected rootfs command");
    };
    let RootfsCommands::Fetch(args) = command else {
        panic!("expected rootfs fetch command");
    };
    assert!(args.base);
    assert!(args.replace);
    assert_eq!(args.url, "https://example.com/rootfs.tar.gz");
}

#[test]
fn snapshot_export_parses_output() {
    let cli = Cli::parse_from([
        "enclave",
        "snapshot",
        "export",
        "sb",
        "ws",
        "snap1",
        "--output",
        "/tmp/snap1.tar.gz",
    ]);
    let Commands::Snapshot { command } = cli.command else {
        panic!("expected snapshot command");
    };
    let SnapshotCommands::Export(args) = command else {
        panic!("expected snapshot export command");
    };
    assert_eq!(args.sandbox, "sb");
    assert_eq!(args.workspace, "ws");
    assert_eq!(args.snapshot, "snap1");
    assert_eq!(args.output, std::path::PathBuf::from("/tmp/snap1.tar.gz"));
}

#[test]
fn snapshot_import_parses_name_and_replace() {
    let cli = Cli::parse_from([
        "enclave",
        "snapshot",
        "import",
        "sb",
        "ws",
        "--name",
        "snap2",
        "--replace",
        "/tmp/snap1.tar.gz",
    ]);
    let Commands::Snapshot { command } = cli.command else {
        panic!("expected snapshot command");
    };
    let SnapshotCommands::Import(args) = command else {
        panic!("expected snapshot import command");
    };
    assert_eq!(args.sandbox, "sb");
    assert_eq!(args.workspace, "ws");
    assert_eq!(args.name.as_deref(), Some("snap2"));
    assert!(args.replace);
    assert_eq!(args.archive, std::path::PathBuf::from("/tmp/snap1.tar.gz"));
}
