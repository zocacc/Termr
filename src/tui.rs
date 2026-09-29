use std::io::{self, stdout};
use std::path::PathBuf;

use anyhow::{Context, Result};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::app::{App, AppAction, Overlay, help_text};
use crate::event::{EventPump, runtime};
use crate::inventory::InventoryStore;

pub fn run(inventory: InventoryStore, hosts_file: PathBuf) -> Result<()> {
    runtime()?.block_on(run_async(inventory, hosts_file))
}

async fn run_async(inventory: InventoryStore, hosts_file: PathBuf) -> Result<()> {
    let mut terminal = TerminalGuard::enter()?;
    let mut events = EventPump::start();
    let mut app = App::new(inventory);
    let mut failure = None;

    terminal
        .terminal
        .draw(|frame| render(frame, &app))
        .context("failed to draw the terminal interface")?;

    while let Some(event) = events.receive().await {
        match app.update(event) {
            AppAction::None => {}
            AppAction::Quit => break,
            AppAction::ReloadInventory => events.reload_inventory(hosts_file.clone()),
            AppAction::CancelOperation => events.cancel_operation(),
            AppAction::Fail(message) => {
                failure = Some(message);
                break;
            }
        }

        terminal
            .terminal
            .draw(|frame| render(frame, &app))
            .context("failed to draw the terminal interface")?;
    }

    drop(events);
    terminal.restore()?;
    if let Some(message) = failure {
        anyhow::bail!(message);
    }
    Ok(())
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

    frame.render_widget(
        Paragraph::new(format!(
            "Termr  selected: {}",
            app.selected_host_ids().len()
        )),
        header,
    );
    let visible = app.visible_hosts();
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new("No hosts match the current search and filters")
                .block(Block::default().title(" Hosts ").borders(Borders::ALL)),
            body,
        );
    } else {
        let items = visible.iter().map(|host| {
            let selected = if app.selected_host_ids().contains(&host.id) {
                "[x]"
            } else {
                "[ ]"
            };
            ListItem::new(format!(
                "{selected} {}  {}:{}  {}",
                host.name,
                host.address,
                host.port,
                app.connection_status(&host.id).label()
            ))
        });
        let list = List::new(items)
            .block(Block::default().title(" Hosts ").borders(Borders::ALL))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        let mut state = ListState::default().with_selected(app.focused_index());
        frame.render_stateful_widget(list, body, &mut state);
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{} {}  /{}  g:{}  t:{}  [?] Help [R] Reload [q] Quit",
            app.progress_indicator(),
            app.status(),
            app.search_query(),
            app.active_group().unwrap_or("all"),
            app.active_tag().unwrap_or("all")
        )),
        footer,
    );

    match app.overlay() {
        Overlay::None => {}
        Overlay::Help => render_overlay(frame, " Help ", help_text()),
        Overlay::Error { title, message } => render_overlay(frame, title, message),
    }
}

fn render_overlay(frame: &mut Frame<'_>, title: &str, message: &str) {
    let area = centered_rect(frame.area(), 70, 18);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(message)
            .wrap(Wrap { trim: false })
            .block(Block::default().title(title).borders(Borders::ALL)),
        area,
    );
}

fn centered_rect(area: Rect, maximum_width: u16, maximum_height: u16) -> Rect {
    let width = maximum_width.min(area.width.saturating_sub(2)).max(1);
    let height = maximum_height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
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
