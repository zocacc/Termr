use std::path::Path;

use termr::inventory::{AuthMethod, Inventory, InventoryStore};

const VALID_INVENTORY: &str = r#"
hosts:
  - id: olt-lab
    name: OLT-LAB
    address: 192.168.1.10
    username: admin
    group: lab
    tags: [olt, gpon]
    auth_method: identity_file
    identity_file: ~/.ssh/id_ed25519
  - id: switch-core
    name: SW-CORE
    address: switch.example.net
    port: 2222
    username: operator
    auth_method: agent
"#;

#[test]
fn valid_yaml_normalizes_documented_defaults() {
    let inventory = Inventory::from_yaml(VALID_INVENTORY).unwrap();

    assert_eq!(inventory.hosts().len(), 2);
    assert_eq!(inventory.hosts()[0].port, 22);
    assert_eq!(inventory.hosts()[0].tags, ["olt", "gpon"]);
    assert_eq!(inventory.hosts()[0].auth_method, AuthMethod::IdentityFile);
    assert_eq!(inventory.hosts()[1].port, 2222);
    assert!(inventory.hosts()[1].tags.is_empty());
    assert_eq!(inventory.hosts()[1].auth_method, AuthMethod::Agent);
}

#[test]
fn duplicate_ids_and_names_are_reported_together() {
    let error = Inventory::from_yaml(
        r#"
hosts:
  - id: olt-lab
    name: OLT-LAB
    address: 192.168.1.10
    username: admin
    auth_method: agent
  - id: olt-lab
    name: OLT-LAB
    address: 192.168.1.11
    username: admin
    auth_method: agent
"#,
    )
    .unwrap_err();
    let message = error.to_string();

    assert!(message.contains("hosts[1].id"));
    assert!(message.contains("hosts[1].name"));
}

#[test]
fn authentication_and_required_field_errors_are_aggregated() {
    let error = Inventory::from_yaml(
        r#"
hosts:
  - id: ""
    name: ""
    address: ""
    username: ""
    auth_method: identity_file
  - id: agent-host
    name: agent-host
    address: localhost
    username: admin
    auth_method: agent
    identity_file: /tmp/should-not-be-used
"#,
    )
    .unwrap_err();
    let message = error.to_string();

    for field in [
        "hosts[0].id",
        "hosts[0].name",
        "hosts[0].address",
        "hosts[0].username",
        "hosts[0].identity_file",
        "hosts[1].identity_file",
    ] {
        assert!(message.contains(field), "missing {field} in {message}");
    }
}

#[test]
fn malformed_credentials_are_not_echoed_in_errors() {
    let secret = "do-not-leak-this-password";
    let error = Inventory::from_yaml(&format!(
        r#"
hosts:
  - id: server
    name: server
    address: localhost
    username: root
    auth_method: agent
    password: {secret}
"#
    ))
    .unwrap_err();

    assert!(!error.to_string().contains(secret));
}

#[test]
fn invalid_auth_method_does_not_echo_its_value() {
    let secret = "do-not-leak-auth-value";
    let error = Inventory::from_yaml(&format!(
        r#"
hosts:
  - id: server
    name: server
    address: localhost
    username: root
    auth_method: {secret}
"#
    ))
    .unwrap_err();
    let message = error.to_string();

    assert!(message.contains("hosts[0].auth_method"));
    assert!(!message.contains(secret));
}

#[test]
fn ids_are_not_restricted_beyond_being_non_empty_and_unique() {
    let inventory = Inventory::from_yaml(
        r#"
hosts:
  - id: site/core.edge
    name: core
    address: localhost
    username: root
    auth_method: agent
"#,
    )
    .unwrap();

    assert_eq!(inventory.hosts()[0].id, "site/core.edge");
}

#[test]
fn failed_reload_preserves_the_previous_inventory() {
    let initial = Inventory::from_yaml(VALID_INVENTORY).unwrap();
    let mut store = InventoryStore::new(initial);

    let error = store.reload_from_yaml("hosts: not-a-list").unwrap_err();

    assert!(error.to_string().contains("hosts"));
    assert_eq!(store.current().hosts().len(), 2);
    assert_eq!(store.current().hosts()[0].id, "olt-lab");
}

#[test]
fn inventory_loads_from_the_configured_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hosts.yaml");
    std::fs::write(&path, VALID_INVENTORY).unwrap();

    let store = InventoryStore::load(Path::new(&path)).unwrap();

    assert_eq!(store.current().hosts().len(), 2);
}
