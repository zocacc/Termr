use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termr::app::{App, InputMode, UiEvent};
use termr::inventory::{Inventory, InventoryStore};

const HOSTS: &str = r#"
hosts:
  - id: olt-lab
    name: OLT-LAB
    address: 192.168.1.10
    username: admin
    group: Lab
    tags: [OLT, GPON]
    auth_method: agent
  - id: switch-core
    name: SW-CORE
    address: core.example.net
    username: operator
    group: Core
    tags: [Switch, Backbone]
    auth_method: agent
  - id: server-one
    name: SERVER-01
    address: linux.example.net
    username: root
    tags: [Linux]
    auth_method: agent
"#;

fn app() -> App {
    App::new(InventoryStore::new(Inventory::from_yaml(HOSTS).unwrap()))
}

fn press(app: &mut App, code: KeyCode) {
    app.update(UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn search(app: &mut App, value: &str) {
    press(app, KeyCode::Char('/'));
    for character in value.chars() {
        press(app, KeyCode::Char(character));
    }
    press(app, KeyCode::Enter);
}

#[test]
fn search_is_case_insensitive_across_all_supported_fields() {
    for (query, expected) in [
        ("olt-lab", "olt-lab"),
        ("192.168", "olt-lab"),
        ("LAB", "olt-lab"),
        ("gPoN", "olt-lab"),
        ("CORE.EXAMPLE", "switch-core"),
    ] {
        let mut app = app();
        search(&mut app, query);

        assert_eq!(
            app.visible_hosts()
                .iter()
                .map(|host| host.id.as_str())
                .collect::<Vec<_>>(),
            [expected]
        );
    }
}

#[test]
fn arrow_and_jk_navigation_clamp_at_the_visible_boundaries() {
    let mut app = app();

    assert_eq!(app.focused_host().unwrap().id, "olt-lab");
    press(&mut app, KeyCode::Down);
    assert_eq!(app.focused_host().unwrap().id, "switch-core");
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.focused_host().unwrap().id, "server-one");
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Char('k'));
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.focused_host().unwrap().id, "olt-lab");
}

#[test]
fn focus_is_preserved_when_the_host_remains_visible() {
    let mut app = app();
    press(&mut app, KeyCode::Down);
    assert_eq!(app.focused_host().unwrap().id, "switch-core");

    search(&mut app, "core");

    assert_eq!(app.focused_host().unwrap().id, "switch-core");
}

#[test]
fn empty_results_are_explicit_and_clearing_restores_inventory() {
    let mut app = app();
    search(&mut app, "not-present");

    assert!(app.visible_hosts().is_empty());
    assert!(app.focused_host().is_none());

    press(&mut app, KeyCode::Char('c'));

    assert_eq!(app.visible_hosts().len(), 3);
    assert_eq!(app.search_query(), "");
    assert_eq!(app.input_mode(), InputMode::Normal);
}

#[test]
fn group_and_tag_filters_cycle_without_mutating_inventory() {
    let mut app = app();

    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.active_group(), Some("Core"));
    assert_eq!(app.visible_hosts()[0].id, "switch-core");

    press(&mut app, KeyCode::Char('c'));
    press(&mut app, KeyCode::Char('t'));
    assert_eq!(app.active_tag(), Some("Backbone"));
    assert_eq!(app.visible_hosts()[0].id, "switch-core");

    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.visible_hosts().len(), 3);
    assert_eq!(app.inventory().current().hosts().len(), 3);
}
