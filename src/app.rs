use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::inventory::{Host, Inventory, InventoryStore};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
}

#[derive(Debug)]
pub struct App {
    inventory: InventoryStore,
    status: String,
    terminal_size: (u16, u16),
    operation_in_progress: bool,
    focused_host_id: Option<String>,
    search_query: String,
    active_group: Option<String>,
    active_tag: Option<String>,
    input_mode: InputMode,
}

impl App {
    pub fn new(inventory: InventoryStore) -> Self {
        let focused_host_id = inventory
            .current()
            .hosts()
            .first()
            .map(|host| host.id.clone());
        Self {
            inventory,
            status: "Ready".to_owned(),
            terminal_size: (0, 0),
            operation_in_progress: false,
            focused_host_id,
            search_query: String::new(),
            active_group: None,
            active_tag: None,
            input_mode: InputMode::Normal,
        }
    }

    pub fn update(&mut self, event: UiEvent) -> AppAction {
        match event {
            UiEvent::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            UiEvent::Resize { width, height } => {
                self.terminal_size = (width, height);
                AppAction::None
            }
            UiEvent::InventoryReloaded(result) => {
                self.operation_in_progress = false;
                match result {
                    Ok(candidate) => {
                        self.inventory = InventoryStore::new(candidate);
                        self.reconcile_focus();
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

    pub fn visible_hosts(&self) -> Vec<&Host> {
        let query = self.search_query.to_lowercase();
        self.inventory
            .current()
            .hosts()
            .iter()
            .filter(|host| {
                self.active_group.as_ref().is_none_or(|group| {
                    host.group
                        .as_ref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(group))
                })
            })
            .filter(|host| {
                self.active_tag.as_ref().is_none_or(|tag| {
                    host.tags
                        .iter()
                        .any(|value| value.eq_ignore_ascii_case(tag))
                })
            })
            .filter(|host| {
                query.is_empty()
                    || host.name.to_lowercase().contains(&query)
                    || host.address.to_lowercase().contains(&query)
                    || host
                        .group
                        .as_ref()
                        .is_some_and(|group| group.to_lowercase().contains(&query))
                    || host
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query))
            })
            .collect()
    }

    pub fn focused_host(&self) -> Option<&Host> {
        let focused_id = self.focused_host_id.as_deref()?;
        self.visible_hosts()
            .into_iter()
            .find(|host| host.id == focused_id)
    }

    pub fn focused_index(&self) -> Option<usize> {
        let focused_id = self.focused_host_id.as_deref()?;
        self.visible_hosts()
            .iter()
            .position(|host| host.id == focused_id)
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    pub fn active_group(&self) -> Option<&str> {
        self.active_group.as_deref()
    }

    pub fn active_tag(&self) -> Option<&str> {
        self.active_tag.as_deref()
    }

    pub fn input_mode(&self) -> InputMode {
        self.input_mode
    }

    fn handle_key(&mut self, key: KeyEvent) -> AppAction {
        if self.input_mode == InputMode::Search {
            match key.code {
                KeyCode::Enter => self.input_mode = InputMode::Normal,
                KeyCode::Esc => {
                    self.search_query.clear();
                    self.input_mode = InputMode::Normal;
                    self.reconcile_focus();
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.reconcile_focus();
                }
                KeyCode::Char(character) => {
                    self.search_query.push(character);
                    self.reconcile_focus();
                }
                _ => {}
            }
            return AppAction::None;
        }

        match key.code {
            KeyCode::Char('q') => AppAction::Quit,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_focus(-1);
                AppAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_focus(1);
                AppAction::None
            }
            KeyCode::Char('/') => {
                self.input_mode = InputMode::Search;
                AppAction::None
            }
            KeyCode::Char('g') => {
                self.cycle_group();
                AppAction::None
            }
            KeyCode::Char('t') => {
                self.cycle_tag();
                AppAction::None
            }
            KeyCode::Char('c') => {
                self.search_query.clear();
                self.active_group = None;
                self.active_tag = None;
                self.input_mode = InputMode::Normal;
                self.reconcile_focus();
                AppAction::None
            }
            KeyCode::Char('R') if !self.operation_in_progress => {
                self.operation_in_progress = true;
                self.status = "Reloading inventory...".to_owned();
                AppAction::ReloadInventory
            }
            _ => AppAction::None,
        }
    }

    fn move_focus(&mut self, direction: isize) {
        let next_id = {
            let visible = self.visible_hosts();
            if visible.is_empty() {
                None
            } else {
                let current = self
                    .focused_host_id
                    .as_ref()
                    .and_then(|id| visible.iter().position(|host| &host.id == id))
                    .unwrap_or(0);
                let last = visible.len() - 1;
                let next = if direction.is_negative() {
                    current.saturating_sub(direction.unsigned_abs())
                } else {
                    current.saturating_add(direction as usize).min(last)
                };
                Some(visible[next].id.clone())
            }
        };
        self.focused_host_id = next_id;
    }

    fn reconcile_focus(&mut self) {
        let next_id = {
            let visible = self.visible_hosts();
            if self
                .focused_host_id
                .as_ref()
                .is_some_and(|focused| visible.iter().any(|host| &host.id == focused))
            {
                self.focused_host_id.clone()
            } else {
                visible.first().map(|host| host.id.clone())
            }
        };
        self.focused_host_id = next_id;
    }

    fn cycle_group(&mut self) {
        let mut values: Vec<_> = self
            .inventory
            .current()
            .hosts()
            .iter()
            .filter_map(|host| host.group.clone())
            .collect();
        sort_and_deduplicate(&mut values);
        self.active_group = next_filter(&values, self.active_group.as_deref());
        self.reconcile_focus();
    }

    fn cycle_tag(&mut self) {
        let mut values: Vec<_> = self
            .inventory
            .current()
            .hosts()
            .iter()
            .flat_map(|host| host.tags.iter().cloned())
            .collect();
        sort_and_deduplicate(&mut values);
        self.active_tag = next_filter(&values, self.active_tag.as_deref());
        self.reconcile_focus();
    }
}

fn sort_and_deduplicate(values: &mut Vec<String>) {
    values.sort_by_key(|value| value.to_lowercase());
    values.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
}

fn next_filter(values: &[String], current: Option<&str>) -> Option<String> {
    let Some(current) = current else {
        return values.first().cloned();
    };
    values
        .iter()
        .position(|value| value.eq_ignore_ascii_case(current))
        .and_then(|index| values.get(index + 1))
        .cloned()
}
