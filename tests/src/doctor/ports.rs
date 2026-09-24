use std::collections::BTreeMap;

use super::*;
use crate::workspace::{PublishedPortSpec, PublishedPortStatus};

/// One workspace's worth of served ports, built the way the publisher reports it.
fn serving(sandbox: &str, workspace: &str, ports: usize) -> PublishedPortOwner {
    let ports = (0..ports)
        .map(|index| {
            let spec =
                PublishedPortSpec::parse(&format!("127.0.0.1:{}:{}", 18000 + index, 8000 + index))
                    .expect("parse published port spec");
            PublishedPortStatus::active(&spec, "10.200.0.5")
        })
        .collect();
    PublishedPortOwner {
        sandbox_id: sandbox.to_string(),
        workspace_id: workspace.to_string(),
        ports,
    }
}

fn expected(entries: &[(&str, &str, ExpectedPorts)]) -> BTreeMap<(String, String), ExpectedPorts> {
    entries
        .iter()
        .map(|(sandbox, workspace, state)| ((sandbox.to_string(), workspace.to_string()), *state))
        .collect()
}

#[test]
fn a_listener_the_registry_expects_is_not_reported() {
    let details = port_inventory_details(
        &expected(&[("sb", "ws", ExpectedPorts::Running)]),
        &[serving("sb", "ws", 2)],
    );
    assert!(details.is_empty(), "{details:?}");
}

#[test]
fn a_listener_for_a_stopped_workspace_is_reported() {
    let details = port_inventory_details(
        &expected(&[("sb", "ws", ExpectedPorts::Stopped)]),
        &[serving("sb", "ws", 3)],
    );
    assert_eq!(details.len(), 1);
    assert!(
        details[0].contains("still listening while the workspace is not running"),
        "{details:?}"
    );
    assert!(details[0].contains("ws"), "{details:?}");
}

#[test]
fn a_listener_the_workspace_no_longer_declares_is_reported() {
    let details = port_inventory_details(
        &expected(&[("sb", "ws", ExpectedPorts::Undeclared)]),
        &[serving("sb", "ws", 1)],
    );
    assert_eq!(details.len(), 1);
    assert!(details[0].contains("no longer declares"), "{details:?}");
}

#[test]
fn a_listener_for_an_unregistered_workspace_is_reported() {
    let details = port_inventory_details(&expected(&[]), &[serving("sb", "gone", 1)]);
    assert_eq!(details.len(), 1);
    assert!(details[0].contains("not registered"), "{details:?}");
}

#[test]
fn only_the_unexpected_listeners_are_reported() {
    let details = port_inventory_details(
        &expected(&[
            ("sb", "live", ExpectedPorts::Running),
            ("sb", "idle", ExpectedPorts::Stopped),
        ]),
        &[
            serving("sb", "live", 2),
            serving("sb", "idle", 1),
            serving("sb", "gone", 4),
        ],
    );
    assert_eq!(details.len(), 2, "{details:?}");
    assert!(
        details.iter().all(|detail| !detail.contains("live")),
        "{details:?}"
    );
}
