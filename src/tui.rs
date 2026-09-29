use std::io::{self, stdout};
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

use crate::app::{App, AppAction, UiEvent};
use crate::event::{EventPump, runtime};
use crate::inventory::InventoryStore;

pub fn run(inventory: InventoryStore, hosts_file: PathBuf) -> Result<()> {
    runtime()?.block_on(run_async(inventory, hosts_file))
}

async fn run_async(inventory: InventoryStore, hosts_file: PathBuf) -> Result<()> {
    let mut terminal = TerminalGuard::enter()?;
    let mut events = EventPump::start();
    let mut app = App::new(inventory);

    terminal
        .terminal
        .draw(|frame| render(frame, &app))
        .context("failed to draw the terminal interface")?;

    while let Some(event) = events.receive().await {
        match app.update(event) {
            AppAction::None => {}
            AppAction::Quit => break,
            AppAction::ReloadInventory => events.reload_inventory(hosts_file.clone()),
        }

        terminal
            .terminal
            .draw(|frame| render(frame, &app))
            .context("failed to draw the terminal interface")?;
    }

    drop(events);
    terminal.restore()
}

pub async fn process_next_event(
    app: &mut App,
    receiver: &mut mpsc::UnboundedReceiver<UiEvent>,
) -> Option<AppAction> {
    receiver.recv().await.map(|event| app.update(event))
}

pub fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    if area.width < 20 || area.height < 4 {
        frame.render_widget(Paragraph::new("Termr - terminal too small"), area);
        return;
    }

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(Paragraph::new("Termr"), header);
    let visible = app.visible_hosts();
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new("No hosts match the current search and filters")
                .block(Block::default().title(" Hosts ").borders(Borders::ALL)),
            body,
        );
    } else {
        let items = visible
            .iter()
            .map(|host| ListItem::new(format!("{}  {}:{}", host.name, host.address, host.port)));
        let list = List::new(items)
            .block(Block::default().title(" Hosts ").borders(Borders::ALL))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        let mut state = ListState::default().with_selected(app.focused_index());
        frame.render_stateful_widget(list, body, &mut state);
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{}  /{}  g:{}  t:{}  [R] Reload [q] Quit",
            app.status(),
            app.search_query(),
            app.active_group().unwrap_or("all"),
            app.active_tag().unwrap_or("all")
        )),
        footer,
    );
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    restored: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        execute!(stdout(), EnterAlternateScreen).context("failed to enter alternate screen")?;

        let backend = CrosstermBackend::new(stdout());
        let terminal = Terminal::new(backend).context("failed to initialize terminal")?;

        Ok(Self {
            terminal,
            restored: false,
        })
    }

    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }

        disable_raw_mode().context("failed to disable terminal raw mode")?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)
            .context("failed to leave alternate screen")?;
        self.terminal
            .show_cursor()
            .context("failed to restore terminal cursor")?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.restored {
            let _ = disable_raw_mode();
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
            let _ = self.terminal.show_cursor();
        }
    }
}
