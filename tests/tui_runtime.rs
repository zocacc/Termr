use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use termr::app::{App, AppAction, UiEvent};
use termr::inventory::{Inventory, InventoryStore};
use termr::tui::render;
use tokio::sync::mpsc;

fn app() -> App {
    let inventory = Inventory::from_yaml("hosts: []").unwrap();
    App::new(InventoryStore::new(inventory))
}

#[tokio::test]
async fn input_and_resize_remain_responsive_while_work_is_pending() {
    let mut app = app();
    app.update(UiEvent::Key(KeyEvent::new(
        KeyCode::Char('R'),
        KeyModifiers::NONE,
    )));
    let (sender, mut receiver) = mpsc::unbounded_channel();

    sender
        .send(UiEvent::Resize {
            width: 42,
            height: 12,
        })
        .unwrap();
    let event = tokio::time::timeout(Duration::from_millis(100), receiver.recv())
        .await
        .expect("resize event was blocked")
        .unwrap();
    let action = app.update(event);

    assert_eq!(action, AppAction::None);
    assert_eq!(app.terminal_size(), (42, 12));

    sender
        .send(UiEvent::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )))
        .unwrap();
    let event = tokio::time::timeout(Duration::from_millis(100), receiver.recv())
        .await
        .expect("input event was blocked")
        .unwrap();
    let action = app.update(event);

    assert_eq!(action, AppAction::Quit);
}

#[test]
fn rendering_is_safe_for_tiny_terminals() {
    for (width, height) in [(1, 1), (8, 2), (20, 3), (40, 8)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = app();

        terminal.draw(|frame| render(frame, &app)).unwrap();
    }
}

#[test]
fn small_terminal_has_an_explicit_fallback() {
    let backend = TestBackend::new(16, 2);
    let mut terminal = Terminal::new(backend).unwrap();
    let app = app();

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Termr"));
}
