use anyhow::{Context, Result};
use std::time::Duration;
use tiberius::{AuthMethod, Client, Config};
use tokio::{net::TcpStream, time::timeout};
use tokio_util::compat::TokioAsyncWriteCompatExt;

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("Falta {key}"))
}

#[tokio::test]
async fn test_sql_server_connection() -> Result<()> {
    dotenvy::dotenv().ok();

    let host = env("DATABASE_HOST")?;
    let port: u16 = env("DATABASE_PORT")?.parse()?;
    let db = env("DATABASE_NAME")?;
    let user = env("DATABASE_USER")?;
    let pass = env("DATABASE_PASSWORD")?;
    let trust = std::env::var("DATABASE_TRUST_CERT").unwrap_or_else(|_| "true".into()) == "true";

    println!("[1/4] Configuración... OK");
    println!("[2/4] TCP {}:{}...", host, port);

    let mut cfg = Config::new();
    cfg.host(&host);
    cfg.port(port);
    cfg.database(&db);
    cfg.authentication(AuthMethod::sql_server(&user, &pass));

    if trust {
        cfg.trust_cert();
    }

    let tcp = timeout(Duration::from_secs(10), TcpStream::connect(cfg.get_addr())).await??;

    tcp.set_nodelay(true)?;
    println!("[2/4] TCP... OK");

    println!("[3/4] TLS/autenticación...");
    let mut client = timeout(
        Duration::from_secs(20),
        Client::connect(cfg, tcp.compat_write()),
    )
    .await??;

    println!("[3/4] SQL Server... OK");

    println!("[4/4] SELECT DB_NAME(), SUSER_SNAME()...");
    let rows = client
        .query(
            "SELECT DB_NAME() AS db_name, SUSER_SNAME() AS login_name",
            &[],
        )
        .await?
        .into_first_result()
        .await?;

    if rows.len() != 1 {
        anyhow::bail!("SELECT de prueba no devolvió una fila");
    }

    println!("[4/4] Query... OK");
    Ok(())
}
