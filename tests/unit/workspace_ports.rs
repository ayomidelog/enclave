use enclave::workspace::{
    merge_published_port_statuses, validate_published_ports, PublishedPortBinding,
    PublishedPortSpec, PublishedPortState, PublishedPortStatus,
};

#[test]
fn parse_published_port_spec_accepts_explicit_protocol() {
    let spec = PublishedPortSpec::parse("127.0.0.1:3001:3000/tcp").expect("parse spec");
    assert_eq!(spec.host_ip, "127.0.0.1");
    assert_eq!(spec.host_port, 3001);
    assert_eq!(spec.workspace_port, 3000);
    assert_eq!(spec.protocol, "tcp");
}

#[test]
fn parse_published_port_spec_defaults_protocol_to_tcp() {
    let spec = PublishedPortSpec::parse("127.0.0.1:3001:3000").expect("parse spec");
    assert_eq!(spec.protocol, "tcp");
}

#[test]
fn parse_published_port_spec_rejects_non_loopback_host() {
    let err = PublishedPortSpec::parse("0.0.0.0:3001:3000/tcp").expect_err("reject host ip");
    assert!(err.to_string().contains("only 127.0.0.1 is allowed"));
}

#[test]
fn parse_published_port_binding_rejects_unsupported_protocol() {
    let err = PublishedPortBinding::parse("127.0.0.1:3001/udp").expect_err("reject udp");
    assert!(err.to_string().contains("only tcp is supported"));
}

#[test]
fn validate_published_ports_rejects_duplicate_host_bindings() {
    let ports = vec![
        PublishedPortSpec::parse("127.0.0.1:3001:3000/tcp").unwrap(),
        PublishedPortSpec::parse("127.0.0.1:3001:4000/tcp").unwrap(),
    ];
    let err = validate_published_ports(&ports).expect_err("reject duplicate binding");
    assert!(err.to_string().contains("duplicate published host binding"));
}

#[test]
fn merge_published_port_statuses_prefers_runtime_statuses_and_keeps_configured_entries() {
    let declared = vec![
        PublishedPortSpec::parse("127.0.0.1:3001:3000/tcp").unwrap(),
        PublishedPortSpec::parse("127.0.0.1:3002:3000/tcp").unwrap(),
    ];
    let runtime = vec![PublishedPortStatus::active(&declared[0], "10.200.0.10")];

    let merged = merge_published_port_statuses(&declared, &runtime);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].state, PublishedPortState::Active);
    assert_eq!(merged[0].workspace_ip.as_deref(), Some("10.200.0.10"));
    assert_eq!(merged[1].state, PublishedPortState::Configured);
    assert!(merged[1].workspace_ip.is_none());
}
