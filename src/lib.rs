//! Wishful Thinking — family wish lists with gift reservations and smart URL import.

pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod images;
pub mod importer;
pub mod routes;
pub mod storage;
pub mod templates;
pub mod util;

use std::sync::Arc;

use axum::Router;

pub use config::Config;

#[derive(Clone)]
pub struct AppState {
    pub db: db::Db,
    pub config: Arc<Config>,
    pub importer: importer::Importer,
    pub storage: storage::Storage,
    pub image_jobs: images::ImageJobs,
}

impl AppState {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let db = db::connect(&config.database_url).await?;
        let llm = config.llm.as_ref().map(|c| c.build());
        let importer = importer::Importer::new(llm, config.llm_mode, config.allow_private_fetch);
        let storage = storage::Storage::open(&config.storage_url)?;
        Ok(AppState {
            db,
            config: Arc::new(config),
            importer,
            storage,
            image_jobs: images::ImageJobs::default(),
        })
    }

    /// Build share links such as `https://host/p/<token>`.
    pub fn absolute(&self, base_url: &str, path: &str) -> String {
        format!(
            "{}{}",
            self.config.base_url.as_deref().unwrap_or(base_url),
            path
        )
    }
}

pub fn app(state: AppState) -> Router {
    routes::router(state)
}
