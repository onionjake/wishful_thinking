//! Try the Amazon wish list importer from the command line:
//!
//! ```sh
//! cargo run --example import_list -- https://www.amazon.com/hz/wishlist/ls/<id>
//! ```

use wishful_thinking::importer::llm::LlmMode;
use wishful_thinking::importer::Importer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: import_list <url>"))?;
    let importer = Importer::new(None, LlmMode::Off, false);
    match importer.import_amazon_list(&url).await {
        Ok(list) => println!("{list:#?}"),
        Err(e) => println!("error: {e}"),
    }
    Ok(())
}
