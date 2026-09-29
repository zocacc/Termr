use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::inventory::{Inventory, InventoryStore};

#[derive(Debug)]
pub enum UiEvent {
    Key(KeyEvent),
    Resize { width: u16, height: u16 },
    Tick,
    InventoryReloaded(Result<Inventory, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    None,
    Quit,
    ReloadInventory,
}

#[derive(Debug)]
pub struct App {
    inventory: InventoryStore,
    status: String,
    terminal_size: (u16, u16),
    operation_in_progress: bool,
}

impl App {
    pub fn new(inventory: InventoryStore) -> Self {
        Self {
            inventory,
            status: "Ready".to_owned(),
            terminal_size: (0, 0),
            operation_in_progress: false,
        }
    }

    pub fn update(&mut self, event: UiEvent) -> AppAction {
        match event {
            UiEvent::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => AppAction::Quit,
                KeyCode::Char('R') if !self.operation_in_progress => {
                    self.operation_in_progress = true;
                    self.status = "Reloading inventory...".to_owned();
                    AppAction::ReloadInventory
                }
                _ => AppAction::None,
            },
            UiEvent::Resize { width, height } => {
                self.terminal_size = (width, height);
                AppAction::None
            }
            UiEvent::InventoryReloaded(result) => {
                self.operation_in_progress = false;
                match result {
                    Ok(candidate) => {
                        self.inventory = InventoryStore::new(candidate);
                        self.status = format!(
                            "Inventory reloaded: {} hosts",
                            self.inventory.current().hosts().len()
                        );
                    }
                    Err(error) => self.status = format!("Reload failed: {error}"),
                }
                AppAction::None
            }
            UiEvent::Key(_) | UiEvent::Tick => AppAction::None,
        }
    }

    pub fn inventory(&self) -> &InventoryStore {
        &self.inventory
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn terminal_size(&self) -> (u16, u16) {
        self.terminal_size
    }

    pub fn operation_in_progress(&self) -> bool {
        self.operation_in_progress
    }

    pub fn set_operation_in_progress(&mut self, in_progress: bool) {
        self.operation_in_progress = in_progress;
    }
}
