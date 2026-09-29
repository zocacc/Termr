use std::io::{self, stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::inventory::InventoryStore;

pub fn run(inventory: InventoryStore) -> Result<()> {
    let mut terminal = TerminalGuard::enter()?;
    let result = run_loop(&mut terminal.terminal, &inventory);
    terminal.restore()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    inventory: &InventoryStore,
) -> Result<()> {
    loop {
        terminal
            .draw(|frame| {
                let content = Paragraph::new(format!(
                    "{} hosts loaded. Press q to quit.",
                    inventory.current().hosts().len()
                ))
                .block(Block::default().title(" Termr ").borders(Borders::ALL));
                frame.render_widget(content, frame.area());
            })
            .context("failed to draw the terminal interface")?;

        if event::poll(Duration::from_millis(250)).context("failed to poll terminal events")?
            && let Event::Key(key) = event::read().context("failed to read a terminal event")?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            return Ok(());
        }
    }
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
