use anyhow::Result;
use clap::Parser;
use sql_agent::{agent::Agent, config::Config};
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
        .init();

    let mut config = Config::from_env()?;
    config.verbose = args.verbose;

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

    let agent = Agent::new(config);

    if args.check_db {
        agent.check_db().await?;
        return Ok(());
    }

    let question = args
        .question
        .ok_or_else(|| anyhow::anyhow!("Falta la pregunta"))?;

    println!("👤 {question}");

    let answer = agent.run(&question).await?;

    println!("\n━━━━━━━━ RESULTADO ━━━━━━━━");
    println!("{answer}");

    Ok(())
}
