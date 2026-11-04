use std::str::FromStr;

use sqlx::sqlite::SqliteConnectOptions;
use tracing_subscriber::EnvFilter;
use wishful_thinking::{app, auth, db, AppState, Config};

const USAGE: &str = "usage: wishful_thinking [reset-password EMAIL]";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {}
        ["reset-password", email] => return reset_password(email).await,
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wishful_thinking=info,tower_http=info".into()),
        )
        .init();

    let config = Config::from_env()?;
    match &config.llm {
        Some(llm) => {
            tracing::info!(provider = ?llm.kind, model = %llm.model, mode = ?config.llm_mode, "LLM-assisted import enabled")
        }
        None => tracing::info!("LLM-assisted import disabled (set WT_LLM_PROVIDER to enable)"),
    }
    let bind = config.bind.clone();
    let state = AppState::new(config).await?;

    // Expired sessions are swept hourly.
    let db = state.db.clone();
    tokio::spawn(async move {
        loop {
            let now = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
            if let Err(e) = sqlx::query("DELETE FROM sessions WHERE expires_at < ?")
                .bind(now)
                .execute(&db)
                .await
            {
                tracing::warn!(error = %e, "session cleanup failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(
        "Wishful Thinking listening on http://{}",
        listener.local_addr()?
    );
    axum::serve(listener, app(state)).await?;
    Ok(())
}

/// `wishful_thinking reset-password EMAIL`: print a new password for the account and sign it
/// out everywhere. The app sends no email, so this is how someone who forgot their password gets back in.
async fn reset_password(email: &str) -> anyhow::Result<()> {
    let config = Config::from_env()?;
    // Pointed at the wrong place, connecting would create an empty database instead.
    let opts = SqliteConnectOptions::from_str(&config.database_url)?;
    let path = opts.get_filename();
    anyhow::ensure!(
        path.exists(),
        "no database at {} (set WT_DATABASE_URL)",
        path.display()
    );
    let db = db::connect(&config.database_url).await?;
    match auth::reset_password(&db, email).await? {
        Some(password) => println!("New password for {email}: {password}"),
        None => anyhow::bail!("no account with email {email}"),
    }
    Ok(())
}
