//! TUI event loop runner.
//!
//! Extracted from `main.rs` to keep the binary entry point clean and allow
//! testing the TUI loop logic independently.

use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::tui::{AppEvent, AppState, TerminalGuard};

/// Run the TUI event loop with the given agent.
/// Returns Ok(()) on clean exit, Err on terminal error.
pub async fn run_tui(agent: Agent) -> Result<()> {
    let _guard = TerminalGuard::new()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // AppState holds Session from PR2
    let mut app = AppState::new(crate::agent::Session::new());
    if let Ok(Some(sess)) = crate::agent::Session::load_last().await {
        app.session = sess;
        app.set_status("Historial cargado — /history para ver");
    }

    let (tx, mut rx) = mpsc::channel::<AppEvent>(32);
    let agent = std::sync::Arc::new(agent);

    // Event-driven render: draw once, then only after state actually changed
    // (agent event or key). Idle 50ms ticks with no key do no draw at all.
    terminal.draw(|f| crate::tui::ui::draw(f, &app))?;
    let mut last_drawn = app.version;

    loop {
        tokio::select! {
            // Agent → UI events
            maybe_ev = rx.recv() => {
                if let Some(ev) = maybe_ev {
                    let is_quit = matches!(ev, AppEvent::Quit);
                    app.handle_event(ev);
                    if is_quit { break; }
                    if crate::tui::ui::needs_redraw(last_drawn, &app) {
                        terminal.draw(|f| crate::tui::ui::draw(f, &app))?;
                        last_drawn = app.version;
                    }
                }
            }
            // Poll crossterm events (responsive 20ms tick with event queue draining)
            _ = tokio::time::sleep(Duration::from_millis(20)) => {
                let mut should_quit = false;
                while event::poll(Duration::ZERO)? {
                    match event::read()? {
                        CEvent::Key(key) => {
                            if key.kind == KeyEventKind::Release {
                                continue;
                            }
                            if crate::tui::ui::handle_key(key, &mut app) {
                                should_quit = true;
                                break;
                            }
                            handle_key_event(&agent, &tx, &mut app, key).await;
                        }
                        CEvent::Resize(_, _) => {
                            terminal.autoresize()?;
                            terminal.clear()?;
                            terminal.draw(|f| crate::tui::ui::draw(f, &app))?;
                            last_drawn = app.version;
                        }
                        _ => {}
                    }
                }
                if should_quit {
                    break;
                }
                if app.version != last_drawn {
                    terminal.draw(|f| crate::tui::ui::draw(f, &app))?;
                    last_drawn = app.version;
                }
            }
        }
    }

    Ok(())
}

/// Handle a single key event, extracted to reduce nesting in the main loop.
async fn handle_key_event(
    agent: &std::sync::Arc<Agent>,
    tx: &mpsc::Sender<AppEvent>,
    app: &mut AppState,
    key: crossterm::event::KeyEvent,
) {
    match key.code {
        KeyCode::Enter => {
            if app.is_loading {
                app.set_status("⏳ Consulta en progreso, por favor espera...");
                return;
            }
            let input = app.input.trim().to_string();
            if input.is_empty() {
                return;
            }
            let cmd = crate::tui::commands::parse_command(&input);
            match cmd {
                crate::tui::commands::Command::Quit => {
                    // Signal quit through the event system
                    let _ = tx.send(AppEvent::Quit).await;
                }
                crate::tui::commands::Command::Clear => {
                    app.clear();
                    app.session.messages.clear();
                    app.clear_input();
                }
                crate::tui::commands::Command::History => {
                    app.toggle_history();
                    app.clear_input();
                }
                crate::tui::commands::Command::Help => {
                    app.toggle_help();
                    app.clear_input();
                }
                crate::tui::commands::Command::Tables => {
                    app.clear_input();
                    app.is_loading = true;
                    app.set_status("Consultando tablas disponibles...");
                    let ag = agent.clone();
                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        match ag.tui_list_tables().await {
                            Ok(txt) => {
                                let _ = tx2
                                    .send(AppEvent::AgentTool {
                                        name: "list_tables".into(),
                                        content: txt,
                                    })
                                    .await;
                                let _ = tx2
                                    .send(AppEvent::AgentDone("Tablas listadas".into()))
                                    .await;
                            }
                            Err(e) => {
                                let _ = tx2.send(AppEvent::Error(e.to_string())).await;
                            }
                        }
                    });
                }
                crate::tui::commands::Command::Describe(tbl) => {
                    app.clear_input();
                    app.is_loading = true;
                    app.set_status(format!("Describiendo estructura de {tbl}..."));
                    let ag = agent.clone();
                    let tx2 = tx.clone();
                    let tbl2 = tbl.clone();
                    tokio::spawn(async move {
                        match ag.tui_describe(&tbl2).await {
                            Ok(txt) => {
                                let _ = tx2
                                    .send(AppEvent::AgentTool {
                                        name: "describe_table".into(),
                                        content: txt,
                                    })
                                    .await;
                                let _ = tx2
                                    .send(AppEvent::AgentDone(format!("Estructura de {tbl2}")))
                                    .await;
                            }
                            Err(e) => {
                                let _ = tx2.send(AppEvent::Error(e.to_string())).await;
                            }
                        }
                    });
                }
                crate::tui::commands::Command::Refresh => {
                    app.clear_input();
                    app.is_loading = true;
                    app.set_status("Refrescando caché de esquema...");
                    let ag = agent.clone();
                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        ag.refresh_cache().await;
                        let _ = tx2
                            .send(AppEvent::AgentDone("🔄 Esquema refrescado".into()))
                            .await;
                    });
                    app.session.schema_memory.clear();
                }
                crate::tui::commands::Command::Export { format, path } => {
                    app.clear_input();
                    let messages = &app.messages;
                    let exported = match format {
                        crate::tui::commands::ExportFormat::Csv => {
                            crate::tui::state::export_messages_csv(messages)
                        }
                        crate::tui::commands::ExportFormat::Json => {
                            crate::tui::state::export_messages_json(messages)
                        }
                    };
                    let default_path = match format {
                        crate::tui::commands::ExportFormat::Csv => "export.csv",
                        crate::tui::commands::ExportFormat::Json => "export.json",
                    };
                    let file_path = path.unwrap_or_else(|| default_path.to_string());
                    match std::fs::write(&file_path, exported) {
                        Ok(_) => app.set_status(format!("Resultados exportados a {}", file_path)),
                        Err(e) => app.set_status(format!("Error exportando: {e}")),
                    }
                }
                crate::tui::commands::Command::Unknown(u) => {
                    app.set_status(format!("Comando desconocido: {u} — escribe /help"));
                    app.clear_input();
                }
                crate::tui::commands::Command::Message(q) => {
                    let q2 = q.clone();
                    app.clear_input();
                    app.handle_event(AppEvent::Input(q.clone()));
                    app.session.push(crate::llm::Message::user(q.clone()));
                    let ag = agent.clone();
                    let tx2 = tx.clone();
                    let mut sess_clone = app.session.clone();
                    tokio::spawn(async move {
                        let _ = tx2
                            .send(AppEvent::AgentStep {
                                step: 1,
                                tool: "search_schema".into(),
                            })
                            .await;
                        match ag.run_with_history(&mut sess_clone, &q2).await {
                            Ok(res) => {
                                let _ = tx2
                                    .send(AppEvent::SessionUpdate(Box::new(sess_clone)))
                                    .await;
                                let _ = tx2.send(AppEvent::AgentDone(res)).await;
                            }
                            Err(e) => {
                                let _ = tx2.send(AppEvent::Error(e.to_string())).await;
                            }
                        }
                    });
                }
            }
        }
        KeyCode::Backspace => {
            app.pop_input();
        }
        KeyCode::Char(c)
            if key.modifiers.is_empty()
                || key.modifiers == crossterm::event::KeyModifiers::SHIFT =>
        {
            app.push_input(c);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::tui::commands::Command;

    #[test]
    fn test_command_parsing_integration() {
        // This test ensures the command parsing used by run_tui works correctly
        assert!(matches!(
            crate::tui::commands::parse_command("/quit"),
            Command::Quit
        ));
        assert!(matches!(
            crate::tui::commands::parse_command("/clear"),
            Command::Clear
        ));
        assert!(matches!(
            crate::tui::commands::parse_command("/history"),
            Command::History
        ));
        assert!(matches!(
            crate::tui::commands::parse_command("/help"),
            Command::Help
        ));
        assert!(matches!(
            crate::tui::commands::parse_command("/tables"),
            Command::Tables
        ));
        assert!(matches!(
            crate::tui::commands::parse_command("/refresh"),
            Command::Refresh
        ));
        assert!(matches!(
            crate::tui::commands::parse_command("/export csv"),
            Command::Export {
                format: crate::tui::commands::ExportFormat::Csv,
                path: None
            }
        ));
        assert!(
            matches!(crate::tui::commands::parse_command("/export json /tmp/out.json"), Command::Export { format: crate::tui::commands::ExportFormat::Json, path: Some(p) } if p == "/tmp/out.json")
        );
    }
}
