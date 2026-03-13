use enclave::enclavefile::{parse_enclavefile, scaffold_enclavefile};

#[test]
fn parse_minimal_enclavefile() {
    let content = r#"
[sandbox]
name = "devbox"
suite = "bookworm"
"#;
    let ef = parse_enclavefile(content).unwrap();
    assert_eq!(ef.sandbox.name, "devbox");
    assert_eq!(ef.sandbox.suite, "bookworm");
    assert!(ef.sandbox.setup.is_empty());
    assert!(ef.workspace.is_empty());
}

#[test]
fn parse_full_enclavefile() {
    let content = r#"
[sandbox]
name = "devbox"
suite = "bookworm"
memory_mb = 4096
cpu_percent = 50
max_procs = 512

setup = [
  "apt install -y nodejs python3 cargo",
  "npm install -g typescript",
  "pip install flask numpy",
]

[workspace.api]
name = "api"
run = "node server.js"
path = "/tmp/project-api"
cpu_seconds = 60
cpu_percent = 25
memory_mb = 2048
max_procs = 256
max_open_files = 65535
ports = ["127.0.0.1:3001:3000/tcp"]

[workspace.builder]
name = "builder"
run = "cargo build --release"

[workspace.shell]
name = "shell"
"#;
    let ef = parse_enclavefile(content).unwrap();
    assert_eq!(ef.sandbox.name, "devbox");
    assert_eq!(ef.sandbox.suite, "bookworm");
    assert_eq!(ef.sandbox.memory_mb, Some(4096));
    assert_eq!(ef.sandbox.cpu_percent, Some(50.0));
    assert_eq!(ef.sandbox.max_procs, Some(512));
    assert_eq!(ef.sandbox.setup.len(), 3);
    assert_eq!(ef.workspace.len(), 3);

    let api = &ef.workspace["api"];
    assert_eq!(api.name, "api");
    assert_eq!(api.run.as_deref(), Some("node server.js"));
    assert_eq!(api.path.as_deref(), Some("/tmp/project-api"));
    assert_eq!(api.cpu_seconds, Some(60));
    assert_eq!(api.cpu_percent, Some(25.0));
    assert_eq!(api.memory_mb, Some(2048));
    assert_eq!(api.max_procs, Some(256));
    assert_eq!(api.max_open_files, Some(65535));
    assert_eq!(api.ports, vec!["127.0.0.1:3001:3000/tcp"]);
    assert!(api.auth.is_empty());

    let builder = &ef.workspace["builder"];
    assert_eq!(builder.name, "builder");
    assert_eq!(builder.run.as_deref(), Some("cargo build --release"));

    let shell = &ef.workspace["shell"];
    assert_eq!(shell.name, "shell");
    assert!(shell.run.is_none());
    assert!(shell.path.is_none());
}

#[test]
fn parse_enclavefile_default_suite() {
    let content = r#"
[sandbox]
name = "test"
"#;
    let ef = parse_enclavefile(content).unwrap();
    assert_eq!(ef.sandbox.suite, "bookworm");
}

#[test]
fn parse_enclavefile_rejects_empty_name() {
    let content = r#"
[sandbox]
name = ""
suite = "bookworm"
"#;
    assert!(parse_enclavefile(content).is_err());
}

#[test]
fn parse_enclavefile_rejects_empty_workspace_name() {
    let content = r#"
[sandbox]
name = "devbox"

[workspace.bad]
name = ""
"#;
    assert!(parse_enclavefile(content).is_err());
}

#[test]
fn parse_enclavefile_rejects_empty_workspace_path() {
    let content = r#"
[sandbox]
name = "devbox"

[workspace.bad]
name = "bad"
path = "   "
"#;
    assert!(parse_enclavefile(content).is_err());
}

#[test]
fn scaffold_enclavefile_is_valid_toml() {
    let content = scaffold_enclavefile();
    let ef = parse_enclavefile(&content).unwrap();
    assert_eq!(ef.sandbox.name, "devbox");
}

#[test]
fn parse_enclavefile_rejects_invalid_workspace_ports() {
    let content = r#"
[sandbox]
name = "devbox"

[workspace.web]
name = "web"
ports = ["0.0.0.0:3001:3000/tcp"]
"#;
    let err = parse_enclavefile(content).expect_err("invalid ports should fail");
    assert!(err.to_string().contains("contains invalid port"));
}

#[test]
fn parse_enclavefile_rejects_invalid_cpu_percent() {
    let content = r#"
[sandbox]
name = "devbox"
cpu_percent = 200
"#;
    let err = parse_enclavefile(content).expect_err("invalid cpu percent should fail");
    assert!(err.to_string().contains("cpu_percent"));
}
