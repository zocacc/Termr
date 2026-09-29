use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::inventory::{Host, Inventory, InventoryError, InventoryStore};

#[derive(Debug)]
pub enum UiEvent {
    Key(KeyEvent),
    Resize {
        width: u16,
        height: u16,
    },
    Tick,
    InventoryReloaded(Result<Inventory, InventoryError>),
    ConnectionStatusChanged {
        host_id: String,
        status: ConnectionStatus,
    },
    TerminalFailure(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    None,
    Quit,
    ReloadInventory,
    CancelOperation,
    Fail(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Error,
}

impl ConnectionStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    Error { title: String, message: String },
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
    selected_host_ids: HashSet<String>,
    connection_statuses: HashMap<String, ConnectionStatus>,
    overlay: Overlay,
    progress_frame: usize,
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
            selected_host_ids: HashSet::new(),
            connection_statuses: HashMap::new(),
            overlay: Overlay::None,
            progress_frame: 0,
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
                if !self.operation_in_progress {
                    return AppAction::None;
                }
                self.operation_in_progress = false;
                match result {
                    Ok(candidate) => {
                        self.inventory = InventoryStore::new(candidate);
                        self.reconcile_inventory_state();
                        self.reconcile_focus();
                        self.status = format!(
                            "Inventory reloaded: {} hosts",
                            self.inventory.current().hosts().len()
                        );
                    }
                    Err(error) => {
                        self.status = "Inventory reload failed".to_owned();
                        self.overlay = Overlay::Error {
                            title: "Inventory reload failed".to_owned(),
                            message: error.to_string(),
                        };
                    }
                }
                AppAction::None
            }
            UiEvent::ConnectionStatusChanged { host_id, status } => {
                if self
                    .inventory
                    .current()
                    .hosts()
                    .iter()
                    .any(|host| host.id == host_id)
                {
                    self.connection_statuses.insert(host_id, status);
                }
                AppAction::None
            }
            UiEvent::TerminalFailure(message) => AppAction::Fail(message),
            UiEvent::Tick => {
                self.progress_frame = self.progress_frame.wrapping_add(1);
                AppAction::None
            }
            UiEvent::Key(_) => AppAction::None,
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

    pub fn selected_host_ids(&self) -> &HashSet<String> {
        &self.selected_host_ids
    }

    pub fn connection_status(&self, host_id: &str) -> ConnectionStatus {
        self.connection_statuses
            .get(host_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    pub fn progress_indicator(&self) -> &'static str {
        const FRAMES: [&str; 4] = ["|", "/", "-", "\\"];
        if self.operation_in_progress {
            FRAMES[self.progress_frame % FRAMES.len()]
        } else {
            ""
        }
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
        if self.overlay != Overlay::None {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.overlay = Overlay::None;
            }
            return AppAction::None;
        }

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
            KeyCode::Char(' ') => {
                if let Some(host_id) = self.focused_host().map(|host| host.id.clone())
                    && !self.selected_host_ids.remove(&host_id)
                {
                    self.selected_host_ids.insert(host_id);
                }
                AppAction::None
            }
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
            KeyCode::Char('?') => {
                self.overlay = Overlay::Help;
                AppAction::None
            }
            KeyCode::Esc => {
                if self.operation_in_progress {
                    self.operation_in_progress = false;
                    self.status = "Inventory reload cancelled".to_owned();
                    AppAction::CancelOperation
                } else {
                    self.status = "Ready".to_owned();
                    AppAction::None
                }
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

    fn reconcile_inventory_state(&mut self) {
        let ids: HashSet<_> = self
            .inventory
            .current()
            .hosts()
            .iter()
            .map(|host| host.id.as_str())
            .collect();
        self.selected_host_ids
            .retain(|host_id| ids.contains(host_id.as_str()));
        self.connection_statuses
            .retain(|host_id, _| ids.contains(host_id.as_str()));
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

pub const fn help_text() -> &'static str {
    concat!(
        "Navigation\n",
        "  ↑/↓ or j/k  Move focus\n",
        "  Space       Toggle host selection\n",
        "  /           Search hosts\n",
        "  g           Cycle group filter\n",
        "  t           Cycle tag filter\n",
        "  c           Clear search and filters\n",
        "  R           Reload inventory\n",
        "  ?           Toggle this help\n",
        "  Esc         Cancel or close\n",
        "  q           Quit"
    )
}
