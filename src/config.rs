use std::env;

use crate::importer::llm::{LlmConfig, LlmMode};

/// Runtime configuration, read from environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub database_url: String,
    /// Absolute base URL used when building share links (e.g. `https://wishes.example.com`).
    /// When unset, links are derived from the request's Host header.
    pub base_url: Option<String>,
    /// Mark cookies `Secure` (enable when serving over HTTPS).
    pub secure_cookies: bool,
    /// Allow the URL importer to fetch private/loopback addresses. Only for local development.
    pub allow_private_fetch: bool,
    pub llm: Option<LlmConfig>,
    pub llm_mode: LlmMode,
}

fn var(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn flag(name: &str) -> bool {
    matches!(var(name).as_deref(), Some("1" | "true" | "yes" | "on"))
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let llm = LlmConfig::from_env()?;
        let llm_mode = match var("WT_LLM_MODE").as_deref() {
            None => LlmMode::Fallback,
            Some(m) => m.parse()?,
        };
        Ok(Config {
            bind: var("WT_BIND").unwrap_or_else(|| "127.0.0.1:3000".into()),
            database_url: var("WT_DATABASE_URL").unwrap_or_else(|| "sqlite://wishful.db".into()),
            base_url: var("WT_BASE_URL").map(|u| u.trim_end_matches('/').to_string()),
            secure_cookies: flag("WT_SECURE_COOKIES"),
            allow_private_fetch: flag("WT_ALLOW_PRIVATE_FETCH"),
            llm,
            llm_mode,
        })
    }

    /// A configuration suitable for tests: in-memory database, no LLM.
    pub fn for_tests() -> Self {
        Config {
            bind: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            base_url: Some("http://wishful.test".into()),
            secure_cookies: false,
            allow_private_fetch: true,
            llm: None,
            llm_mode: LlmMode::Off,
        }
    }
}
