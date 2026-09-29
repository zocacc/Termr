use std::io::{self, stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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

use crate::app::{App, AppAction, ConnectionStatus, Overlay, UiEvent, help_text};
use crate::config::AppConfig;
use crate::event::{EventPump, runtime};
use crate::inventory::InventoryStore;
use crate::session::{SessionManager, SessionState, SessionUpdate};
use crate::ssh::host_key::{ChannelHostKeyPrompt, HostKeyPromptRequest, KnownHostsVerifier};
use crate::ssh::interactive::{InteractiveEvent, InteractiveSession};
use crate::ssh::{PtyRequest, RusshClient, connect_request_for_host};

enum SshWorkflow {
    Idle,
    Connecting {
        host_id: String,
        confirmation: Option<HostKeyPromptRequest>,
    },
    Interactive {
        host_id: String,
        session: InteractiveSession,
    },
}

impl SshWorkflow {
    fn host_id(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Connecting { host_id, .. } | Self::Interactive { host_id, .. } => Some(host_id),
        }
    }
}

pub fn run(
    inventory: InventoryStore,
    hosts_file: PathBuf,
    config: AppConfig,
    known_hosts_file: PathBuf,
) -> Result<()> {
    runtime()?.block_on(run_async(inventory, hosts_file, config, known_hosts_file))
}

async fn run_async(
    inventory: InventoryStore,
    hosts_file: PathBuf,
    config: AppConfig,
    known_hosts_file: PathBuf,
) -> Result<()> {
    let mut terminal = TerminalGuard::enter()?;
    let mut events = EventPump::start();
    let mut app = App::new(inventory);
    let mut failure = None;
    let (prompt, mut prompt_requests) = ChannelHostKeyPrompt::new(8);
    let verifier = Arc::new(KnownHostsVerifier::new(known_hosts_file, Arc::new(prompt)));
    let mut sessions = SessionManager::new(Arc::new(RusshClient), verifier);
    let mut workflow = SshWorkflow::Idle;

    terminal
        .terminal
        .draw(|frame| render(frame, &app))
        .context("failed to draw the terminal interface")?;

    loop {
        enum RuntimeEvent {
            Ui(Option<UiEvent>),
            Session(Option<SessionUpdate>),
            Prompt(Option<HostKeyPromptRequest>),
            Interactive(Option<InteractiveEvent>),
        }
        let event = tokio::select! {
            event = events.receive() => RuntimeEvent::Ui(event),
            update = sessions.next_update() => RuntimeEvent::Session(update),
            prompt = prompt_requests.recv() => RuntimeEvent::Prompt(prompt),
            event = async {
                match &mut workflow {
                    SshWorkflow::Interactive { session, .. } => session.next_event().await,
                    _ => std::future::pending().await,
                }
            } => RuntimeEvent::Interactive(event),
        };

        let action = match event {
            RuntimeEvent::Ui(Some(event)) => app.update(event),
            RuntimeEvent::Ui(None) => break,
            RuntimeEvent::Session(Some(update)) => {
                let status = match &update.state {
                    SessionState::Disconnected => ConnectionStatus::Disconnected,
                    SessionState::Connecting => ConnectionStatus::Connecting,
                    SessionState::Connected => ConnectionStatus::Connected,
                    SessionState::Error(_) => ConnectionStatus::Error,
                };
                app.update(UiEvent::ConnectionStatusChanged {
                    host_id: update.host_id.clone(),
                    status,
                });
                match update.state {
                    SessionState::Connected
                        if matches!(
                            &workflow,
                            SshWorkflow::Connecting { host_id, .. } if host_id == &update.host_id
                        ) =>
                    {
                        if let Some(connection) =
                            sessions.take_connection_for_interactive(&update.host_id)
                        {
                            let (width, height) = app.terminal_size();
                            let (columns, rows) = session_pty_size(width, height);
                            workflow = SshWorkflow::Interactive {
                                host_id: update.host_id,
                                session: InteractiveSession::spawn(
                                    connection,
                                    PtyRequest::new("xterm-256color", columns, rows),
                                ),
                            };
                        }
                    }
                    SessionState::Error(error) => {
                        app.update(UiEvent::InteractiveError {
                            host_id: update.host_id,
                            message: error.message,
                        });
                        workflow = SshWorkflow::Idle;
                    }
                    _ => {}
                }
                AppAction::None
            }
            RuntimeEvent::Session(None) => AppAction::None,
            RuntimeEvent::Prompt(Some(prompt)) => {
                if let SshWorkflow::Connecting {
                    host_id,
                    confirmation,
                } = &mut workflow
                    && host_id == &prompt.host_id
                    && confirmation.is_none()
                {
                    app.update(UiEvent::HostKeyConfirmation {
                        host: format!("{} ({}:{})", prompt.host_id, prompt.address, prompt.port),
                        algorithm: prompt.algorithm.clone(),
                        fingerprint: prompt.fingerprint.clone(),
                    });
                    *confirmation = Some(prompt);
                } else {
                    prompt.respond(false);
                }
                AppAction::None
            }
            RuntimeEvent::Prompt(None) => AppAction::None,
            RuntimeEvent::Interactive(Some(InteractiveEvent::Ready)) => {
                app.update(UiEvent::InteractiveReady {
                    host_id: workflow.host_id().unwrap_or_default().to_owned(),
                });
                AppAction::None
            }
            RuntimeEvent::Interactive(Some(InteractiveEvent::Output(output))) => {
                app.update(UiEvent::InteractiveOutput(output));
                AppAction::None
            }
            RuntimeEvent::Interactive(Some(InteractiveEvent::Error(error))) => {
                app.update(UiEvent::InteractiveError {
                    host_id: workflow.host_id().unwrap_or_default().to_owned(),
                    message: error.message,
                });
                AppAction::None
            }
            RuntimeEvent::Interactive(Some(InteractiveEvent::Closed) | None) => {
                let host_id = workflow.host_id().unwrap_or_default().to_owned();
                sessions.interactive_closed(&host_id);
                app.update(UiEvent::InteractiveClosed { host_id });
                workflow = SshWorkflow::Idle;
                AppAction::None
            }
        };

        match action {
            AppAction::None => {}
            AppAction::Quit => break,
            AppAction::ReloadInventory => events.reload_inventory(hosts_file.clone()),
            AppAction::CancelOperation => events.cancel_operation(),
            AppAction::ConnectHost(host_id) => {
                if matches!(workflow, SshWorkflow::Idle)
                    && let Some(host) = app
                        .inventory()
                        .current()
                        .hosts()
                        .iter()
                        .find(|host| host.id == host_id)
                    && let Ok(request) = connect_request_for_host(
                        host,
                        Duration::from_secs(config.connect_timeout_seconds),
                    )
                    && sessions.connect(request)
                {
                    app.update(UiEvent::ConnectionStatusChanged {
                        host_id: host_id.clone(),
                        status: ConnectionStatus::Connecting,
                    });
                    workflow = SshWorkflow::Connecting {
                        host_id,
                        confirmation: None,
                    };
                }
            }
            AppAction::SessionInput(input) => {
                if let SshWorkflow::Interactive { session, .. } = &workflow
                    && let Err(error) = session.send_input(input)
                {
                    app.update(UiEvent::InteractiveError {
                        host_id: workflow.host_id().unwrap_or_default().to_owned(),
                        message: error.to_string(),
                    });
                }
            }
            AppAction::ResizeSession => {
                if let SshWorkflow::Interactive { session, .. } = &workflow {
                    let (width, height) = app.terminal_size();
                    let (columns, rows) = session_pty_size(width, height);
                    if let Err(error) = session.resize(columns, rows) {
                        app.update(UiEvent::InteractiveError {
                            host_id: workflow.host_id().unwrap_or_default().to_owned(),
                            message: error.to_string(),
                        });
                    }
                }
            }
            AppAction::CloseSession => {
                if let SshWorkflow::Interactive { session, .. } = &workflow {
                    session.close();
                }
            }
            AppAction::TrustHostKey(trusted) => {
                if let SshWorkflow::Connecting { confirmation, .. } = &mut workflow
                    && let Some(confirmation) = confirmation.take()
                {
                    confirmation.respond(trusted);
                }
            }
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

    match workflow {
        SshWorkflow::Interactive { session, .. } => {
            session.close();
            session.finish().await;
        }
        SshWorkflow::Connecting {
            host_id,
            confirmation,
        } => {
            if let Some(confirmation) = confirmation {
                confirmation.respond(false);
            }
            sessions.disconnect(&host_id);
        }
        SshWorkflow::Idle => {}
    }
    sessions.shutdown();
    sessions.finish_shutdown().await;
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
    let hosts_content = if app.session_host_id().is_some() {
        let (hosts, session) = session_panels(body);
        let inner = bordered_inner(session);
        let output = session_output_tail(
            &app.session_output(),
            usize::from(inner.width),
            usize::from(inner.height),
        );
        frame.render_widget(
            Paragraph::new(output)
                .wrap(Wrap { trim: false })
                .block(Block::default().title(" Session ").borders(Borders::ALL)),
            session,
        );
        hosts
    } else {
        Some(body)
    };
    if let Some(content) = hosts_content {
        if visible.is_empty() {
            frame.render_widget(
                Paragraph::new("No hosts match the current search and filters")
                    .block(Block::default().title(" Hosts ").borders(Borders::ALL)),
                content,
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
            frame.render_stateful_widget(list, content, &mut state);
        }
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
        Overlay::HostKey {
            host,
            algorithm,
            fingerprint,
        } => render_overlay(
            frame,
            " Trust SSH host key? ",
            &format!(
                "Host: {host}\nAlgorithm: {algorithm}\nFingerprint: {fingerprint}\n\n[y] Trust  [n/Esc] Reject"
            ),
        ),
    }
}

fn session_pty_size(width: u16, height: u16) -> (u32, u32) {
    let area = Rect::new(0, 0, width.max(1), height.max(1));
    let body = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area)[1];
    let (_, session) = session_panels(body);
    let inner = bordered_inner(session);
    (
        u32::from(inner.width.max(1)),
        u32::from(inner.height.max(1)),
    )
}

fn session_panels(body: Rect) -> (Option<Rect>, Rect) {
    if body.width >= 50 {
        let [hosts, session] =
            Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
                .areas(body);
        (Some(hosts), session)
    } else {
        (None, body)
    }
}

fn bordered_inner(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

fn session_output_tail(output: &str, columns: usize, rows: usize) -> String {
    let capacity = columns.max(1).saturating_mul(rows.max(1));
    let mut tail: Vec<char> = output.chars().rev().take(capacity).collect();
    tail.reverse();
    tail.into_iter().collect()
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

#[cfg(test)]
mod pty_layout {
    use super::*;

    #[test]
    fn remote_size_matches_the_bordered_session_panel() {
        let (columns, rows) = session_pty_size(120, 40);
        assert!(columns < 120);
        assert!(rows < 38);
        assert!(columns > 1 && rows > 1);
    }

    #[test]
    fn viewport_keeps_the_newest_prompt_visible() {
        let output = format!("{}CURRENT> ", "old output\n".repeat(100));
        let visible = session_output_tail(&output, 20, 4);
        assert!(visible.ends_with("CURRENT> "));
        assert!(visible.chars().count() <= 80);
    }
}
