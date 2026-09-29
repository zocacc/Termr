use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termr::app::{App, ConnectionStatus, Overlay, UiEvent, help_text};
use termr::inventory::{Inventory, InventoryStore};

const HOSTS: &str = r#"
hosts:
  - id: olt-lab
    name: OLT-LAB
    address: 192.168.1.10
    username: admin
    group: lab
    tags: [olt]
    auth_method: agent
  - id: switch-core
    name: SW-CORE
    address: 192.168.1.20
    username: admin
    group: core
    tags: [switch]
    auth_method: agent
"#;

fn app() -> App {
    App::new(InventoryStore::new(Inventory::from_yaml(HOSTS).unwrap()))
}

fn press(app: &mut App, code: KeyCode) {
    app.update(UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

#[test]
fn selection_survives_filters_and_drops_removed_hosts_after_reload() {
    let mut app = app();
    press(&mut app, KeyCode::Char(' '));
    assert!(app.selected_host_ids().contains("olt-lab"));

    press(&mut app, KeyCode::Char('/'));
    for character in "core".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.selected_host_ids().contains("olt-lab"));

    let replacement = Inventory::from_yaml(
        r#"
hosts:
  - id: switch-core
    name: SW-CORE
    address: 192.168.1.20
    username: admin
    auth_method: agent
"#,
    )
    .unwrap();
    app.update(UiEvent::InventoryReloaded(Ok(replacement)));

    assert!(app.selected_host_ids().is_empty());
}

#[test]
fn connection_status_is_kept_per_host_and_unknown_updates_are_ignored() {
    let mut app = app();

    app.update(UiEvent::ConnectionStatusChanged {
        host_id: "olt-lab".to_owned(),
        status: ConnectionStatus::Connecting,
    });
    app.update(UiEvent::ConnectionStatusChanged {
        host_id: "missing".to_owned(),
        status: ConnectionStatus::Error,
    });

    assert_eq!(
        app.connection_status("olt-lab"),
        ConnectionStatus::Connecting
    );
    assert_eq!(
        app.connection_status("switch-core"),
        ConnectionStatus::Disconnected
    );
    assert_eq!(
        app.connection_status("missing"),
        ConnectionStatus::Disconnected
    );
}

#[test]
fn help_lists_every_active_shortcut_and_escape_closes_it() {
    let expected = ["j/k", "Space", "/", "g", "t", "c", "R", "?", "Esc", "q"];
    for shortcut in expected {
        assert!(help_text().contains(shortcut), "missing {shortcut}");
    }

    let mut app = app();
    press(&mut app, KeyCode::Char('?'));
    assert_eq!(app.overlay(), &Overlay::Help);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.overlay(), &Overlay::None);
}

#[test]
fn reload_failure_is_actionable_and_closable() {
    let mut app = app();
    let error = Inventory::from_yaml("hosts: not-a-list").unwrap_err();

    app.update(UiEvent::InventoryReloaded(Err(error)));

    let Overlay::Error { title, message } = app.overlay() else {
        panic!("expected error overlay");
    };
    assert_eq!(title, "Inventory reload failed");
    assert!(message.contains("hosts"));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.overlay(), &Overlay::None);
}

#[test]
fn ticks_advance_progress_feedback_during_background_work() {
    let mut app = app();
    app.set_operation_in_progress(true);
    let before = app.progress_indicator();

    app.update(UiEvent::Tick);

    assert_ne!(app.progress_indicator(), before);
}
