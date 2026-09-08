use anyhow::Result;
use clap::Parser;
use sql_agent::{
    agent::Agent,
    config::Config,
    tui::{self, run_tui},
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
        println!(
            "⚙️  Proveedor/modelo: {}/{}",
            config.llm.provider,
            if config.llm.model.is_empty() {
                &config.llm.ollama_model
            } else {
                &config.llm.model
            }
        );
        println!("🗄️  SQL Server: {}:{}", config.db.host, config.db.port);
        println!("📁 Base: {}", config.db.name);
        println!("🛡️  Solo lectura: ACTIVADO");
        println!("🔐 SQL Validator: ACTIVADO");
        println!("⚡ Cache esquema: ACTIVADO");
        println!("🔢 Máximo de pasos: {}", config.limits.max_steps);
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
