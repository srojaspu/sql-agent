use anyhow::Result;
use clap::Parser;
use sql_agent::{
    agent::{Agent, Session},
    config::Config,
    tui::{self, AppState, TerminalGuard},
};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "sql-agent",
    version,
    about = "Agente IA read-only para SQL Server"
)]
struct Args {
    #[arg(short, long)]
    verbose: bool,

    #[arg(long)]
    check_db: bool,

    #[arg(long)]
    no_tui: bool,

    #[arg()]
    question: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(if args.verbose { "info" } else { "warn" }))
        .without_time()
        .with_writer(std::io::stderr)
        .init();

    let mut config = Config::from_env()?;
    config.verbose = args.verbose;

    // Banner solo en modo no-TUI (one-shot / check-db / fallback). En TUI la propia UI hace el draw.
    if !(tui::is_interactive(args.no_tui) && args.question.is_none() && !args.check_db) {
        println!("\n╔════════════════════════════════════════════════════╗");
        println!("║             SQL AGENT PROFESSIONAL 0.7            ║");
        println!("╚════════════════════════════════════════════════════╝");
        println!("⚙️  Modelo: {}", config.ollama_model);
        println!(
            "🗄️  SQL Server: {}:{}",
            config.database_host, config.database_port
        );
        println!("📁 Base: {}", config.database_name);
        println!("🛡️  Solo lectura: ACTIVADO");
        println!("🔐 SQL Validator: ACTIVADO");
        println!("⚡ Cache esquema: ACTIVADO");
        println!("🔢 Máximo de pasos: {}", config.max_steps);
    }

    let agent = Agent::new(config);

    if args.check_db {
        agent.check_db().await?;
        return Ok(());
    }

    // One-shot preserved: if question provided, always one-shot
    if let Some(q) = args.question {
        println!("👤 {q}");
        let answer = agent.run(&q).await?;
        println!("\n━━━━━━━━ RESULTADO ━━━━━━━━");
        println!("{answer}");
        return Ok(());
    }

    // No question — decide TUI vs fallback
    if tui::is_interactive(args.no_tui) {
        // Attempt TUI; graceful fallback if terminal init fails
        match run_tui(agent).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("TUI no disponible ({e}), fallback a modo one-shot.");
                eprintln!(
                    "Usa: cargo run -- \"tu pregunta\"  o  cargo run -- --no-tui \"pregunta\""
                );
                anyhow::bail!("Falta la pregunta");
            }
        }
    } else {
        // Pipe / non-TTY / --no-tui without question → plain fallback
        if args.no_tui {
            anyhow::bail!("Falta la pregunta (modo --no-tui sin pregunta)");
        } else {
            eprintln!("Modo no interactivo detectado (pipe o sin TTY).");
            eprintln!("Usa: cargo run -- \"tu pregunta\"  para one-shot, o ejecuta en terminal interactiva para TUI.");
            anyhow::bail!("Falta la pregunta");
        }
    }
}

async fn run_tui(agent: Agent) -> Result<()> {
    use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind};
    use ratatui::{backend::CrosstermBackend, Terminal};
    use std::time::Duration;
    use tokio::sync::mpsc;

    let _guard = TerminalGuard::new()?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // AppState holds Session from PR2
    let mut app = AppState::new(Session::new());
    if let Ok(Some(sess)) = Session::load_last().await {
        app.session = sess;
        app.set_status("Historial cargado — /history para ver");
    }

    let (tx, mut rx) = mpsc::channel::<tui::AppEvent>(32);
    let agent = std::sync::Arc::new(agent);

    loop {
        terminal.draw(|f| tui::ui::draw(f, &app))?;

        tokio::select! {
            // Agent → UI events
            maybe_ev = rx.recv() => {
                if let Some(ev) = maybe_ev {
                    let is_quit = matches!(ev, tui::AppEvent::Quit);
                    app.handle_event(ev);
                    if is_quit { break; }
                }
            }
            // Poll crossterm events (non-blocking via tokio sleep)
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if event::poll(Duration::from_millis(0))? {
                    if let CEvent::Key(key) = event::read()? {
                        if key.kind == KeyEventKind::Release {
                            continue;
                        }
                        if tui::ui::handle_key(key, &mut app) {
                            break;
                        }
                        match key.code {
                            KeyCode::Enter => {
                                let input = app.input.trim().to_string();
                                if input.is_empty() { continue; }
                                let cmd = tui::ui::parse_command(&input);
                                match cmd {
                                    tui::ui::Command::Quit => break,
                                    tui::ui::Command::Clear => {
                                        app.clear();
                                        // also clear session history
                                        app.session.messages.clear();
                                        app.input.clear();
                                    },
                                    tui::ui::Command::History => {
                                        app.toggle_history();
                                        app.input.clear();
                                    },
                                    tui::ui::Command::Help => {
                                        app.toggle_help();
                                        app.input.clear();
                                    },
                                    tui::ui::Command::Tables => {
                                        app.input.clear();
                                        let ag = agent.clone();
                                        let tx2 = tx.clone();
                                        tokio::spawn(async move {
                                            match ag.tui_list_tables().await {
                                                Ok(txt) => { let _ = tx2.send(tui::AppEvent::AgentTool{name:"search_schema".into(), content: txt}).await; let _ = tx2.send(tui::AppEvent::AgentDone("Tablas listadas".into())).await; },
                                                Err(e) => { let _ = tx2.send(tui::AppEvent::Error(e.to_string())).await; },
                                            }
                                        });
                                    },
                                    tui::ui::Command::Describe(tbl) => {
                                        app.input.clear();
                                        let ag = agent.clone();
                                        let tx2 = tx.clone();
                                        let tbl2 = tbl.clone();
                                        tokio::spawn(async move {
                                            match ag.tui_describe(&tbl2).await {
                                                Ok(txt) => { let _ = tx2.send(tui::AppEvent::AgentTool{name:"describe_table".into(), content: txt}).await; let _ = tx2.send(tui::AppEvent::AgentDone(format!("Estructura de {tbl2}"))).await; },
                                                Err(e) => { let _ = tx2.send(tui::AppEvent::Error(e.to_string())).await; },
                                            }
                                        });
                                    },
                                    tui::ui::Command::Refresh => {
                                        app.input.clear();
                                        let ag = agent.clone();
                                        let tx2 = tx.clone();
                                        // need mutable session; we handle via agent method that clears cache
                                        // For now, emit step and then refresh
                                        let _ = tx2.send(tui::AppEvent::AgentStep{step:1, tool:"refresh".into()}).await;
                                        tokio::spawn(async move {
                                            // refresh invalidates schema cache; session memory cleared via separate call
                                            ag.refresh_cache().await;
                                            let _ = tx2.send(tui::AppEvent::AgentDone("🔄 Esquema refrescado".into())).await;
                                        });
                                        app.session.schema_memory.clear();
                                        app.set_status("Refrescando esquema...");
                                    },
                                    tui::ui::Command::Unknown(u) => {
                                        app.set_status(format!("Comando desconocido: {u} — /help"));
                                        app.input.clear();
                                    },
                                    tui::ui::Command::Message(q) => {
                                        let q2 = q.clone();
                                        app.input.clear();
                                        app.handle_event(tui::AppEvent::Input(q.clone()));
                                        // push to session for history continuity
                                        app.session.push(sql_agent::llm::Message::user(q.clone()));
                                        let ag = agent.clone();
                                        let tx2 = tx.clone();
                                        let mut sess_clone = app.session.clone();
                                        tokio::spawn(async move {
                                            let _ = tx2.send(tui::AppEvent::AgentStep{step:1, tool:"search_schema".into()}).await;
                                            match ag.run_with_history(&mut sess_clone, &q2).await {
                                                Ok(res) => { let _ = tx2.send(tui::AppEvent::AgentDone(res)).await; },
                                                Err(e) => { let _ = tx2.send(tui::AppEvent::Error(e.to_string())).await; },
                                            }
                                        });
                                    }
                                }
                            },
                            KeyCode::Backspace => { app.input.pop(); },
                            KeyCode::Char(c) => {
                                // Don't capture if it's a control combo already handled
                                app.input.push(c);
                            },
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

// Pure helper for tests: mirrors tui::should_use_tui but uses IsTerminal trait directly
pub fn should_use_tui(no_tui: bool, stdout_tty: bool, stdin_tty: bool) -> bool {
    tui::should_use_tui(no_tui, stdout_tty, stdin_tty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn args_no_tui_flag_parses() {
        let a = Args::try_parse_from(["sql-agent", "--no-tui", "pregunta"]).unwrap();
        assert!(a.no_tui);
        assert_eq!(a.question.as_deref(), Some("pregunta"));
    }

    #[test]
    fn args_one_shot_preserved() {
        let a = Args::try_parse_from(["sql-agent", "mi pregunta"]).unwrap();
        assert!(!a.no_tui);
        assert_eq!(a.question.as_deref(), Some("mi pregunta"));
        assert!(!a.check_db);
    }

    #[test]
    fn args_bare_run_no_question() {
        let a = Args::try_parse_from(["sql-agent"]).unwrap();
        assert!(a.question.is_none());
        assert!(!a.no_tui);
    }

    #[test]
    fn args_check_db_still_works() {
        let a = Args::try_parse_from(["sql-agent", "--check-db"]).unwrap();
        assert!(a.check_db);
        assert!(a.question.is_none());
        let b = Args::try_parse_from(["sql-agent", "--check-db", "--no-tui"]).unwrap();
        assert!(b.check_db && b.no_tui);
    }

    #[test]
    fn hybrid_is_interactive_pure_logic() {
        assert!(should_use_tui(false, true, true));
        assert!(!should_use_tui(true, true, true));
        assert!(!should_use_tui(false, false, true));
        assert!(!should_use_tui(false, true, false));
        assert!(!should_use_tui(false, false, false));
    }

    #[test]
    fn hybrid_no_tui_forces_fallback() {
        // --no-tui should force fallback even when TTY
        assert!(!should_use_tui(true, true, true));
        // pipe (non-TTY) forces fallback
        assert!(!should_use_tui(false, false, false));
    }
}
