//! Try the URL importer from the command line:
//!
//! ```sh
//! cargo run --example import -- https://www.example.com/some/product
//! WT_LLM_PROVIDER=anthropic ANTHROPIC_API_KEY=... WT_LLM_MODE=always cargo run --example import -- <url>
//! ```

use wishful_thinking::importer::Importer;
use wishful_thinking::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: import <url>"))?;
    let config = Config::from_env()?;
    let llm = config.llm.as_ref().map(|c| c.build());
    let importer = Importer::new(llm, config.llm_mode, config.allow_private_fetch);
    match importer.import(&url).await {
        Ok(outcome) => println!("{}", serde_json::to_string_pretty(&outcome)?),
        Err(e) => println!("error: {e}"),
    }
    Ok(())
}
