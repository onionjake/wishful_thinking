use tracing_subscriber::EnvFilter;
use wishful_thinking::{app, AppState, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "wishful_thinking=info,tower_http=info".into()))
        .init();

    let config = Config::from_env()?;
    match &config.llm {
        Some(llm) => tracing::info!(provider = ?llm.kind, model = %llm.model, mode = ?config.llm_mode, "LLM-assisted import enabled"),
        None => tracing::info!("LLM-assisted import disabled (set WT_LLM_PROVIDER to enable)"),
    }
    let bind = config.bind.clone();
    let state = AppState::new(config).await?;

    // Expired sessions are swept hourly.
    let db = state.db.clone();
    tokio::spawn(async move {
        loop {
            let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
            if let Err(e) = sqlx::query("DELETE FROM sessions WHERE expires_at < ?").bind(now).execute(&db).await {
                tracing::warn!(error = %e, "session cleanup failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("Wishful Thinking listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app(state)).await?;
    Ok(())
}
