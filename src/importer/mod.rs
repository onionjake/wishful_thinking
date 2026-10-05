//! Import a wish-list item from a store URL.
//!
//! The pipeline is: validate the URL → fetch it safely → extract structured data and
//! heuristics → optionally ask an LLM to fill gaps or clean up → merge, trusting structured
//! data over model output.

pub mod extract;
pub mod fetch;
pub mod llm;
pub mod price;

use std::sync::Arc;

use serde::Serialize;
use url::Url;

use extract::{ProductInfo, Source};
use llm::{LlmMode, LlmProduct, LlmProvider};

/// Characters of page digest sent to the model.
const DIGEST_CHARS: usize = 24_000;

#[derive(Clone)]
pub struct Importer {
    http: reqwest::Client,
    llm: Option<Arc<dyn LlmProvider>>,
    mode: LlmMode,
    allow_private: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportOutcome {
    pub url: String,
    pub info: ProductInfo,
    /// Model label when an LLM contributed.
    pub llm_used: Option<String>,
    /// Non-fatal problems to show the user (e.g. "store blocked the request").
    pub warnings: Vec<String>,
}

impl ImportOutcome {
    pub fn source_label(&self) -> &'static str {
        let structured = [self.info.title_source, self.info.price_source, self.info.image_source]
            .iter()
            .any(|s| s.is_structured());
        match (structured, self.llm_used.is_some()) {
            (_, true) if !structured => "llm",
            (true, true) => "structured+llm",
            (true, false) => "structured",
            _ => "page",
        }
    }
}

impl Importer {
    pub fn new(llm: Option<Arc<dyn LlmProvider>>, mode: LlmMode, allow_private: bool) -> Self {
        Importer { http: fetch::build_client(allow_private), llm, mode, allow_private }
    }

    pub fn llm_label(&self) -> Option<String> {
        match self.mode {
            LlmMode::Off => None,
            _ => self.llm.as_ref().map(|l| l.label()),
        }
    }

    /// Import a URL. Network failures are reported as warnings with whatever could be
    /// inferred from the URL itself, so the user can always finish the item by hand.
    pub async fn import(&self, raw_url: &str) -> Result<ImportOutcome, fetch::FetchError> {
        let url = fetch::validate_url(raw_url, self.allow_private)?;
        let mut outcome = ImportOutcome {
            url: url.to_string(),
            info: ProductInfo { store: extract::store_from_url(&url), ..Default::default() },
            llm_used: None,
            warnings: Vec::new(),
        };

        let (final_url, html) = match fetch::fetch(&self.http, &url).await {
            Ok(fetch::Fetched::Html { final_url, body }) => (final_url, body),
            Ok(fetch::Fetched::Image { final_url }) => {
                outcome.info.image_url = Some(final_url.to_string());
                outcome.info.image_source = Source::Generic;
                outcome.warnings.push("That link is an image — it's been set as the item picture.".into());
                return Ok(outcome);
            }
            Err(e @ (fetch::FetchError::InvalidUrl | fetch::FetchError::Forbidden)) => return Err(e),
            Err(e) => {
                outcome.warnings.push(e.to_string());
                return Ok(outcome);
            }
        };

        outcome.url = final_url.to_string();
        outcome.info = extract::extract(&html, &final_url);

        let want_llm = match self.mode {
            LlmMode::Off => false,
            LlmMode::Fallback => !outcome.info.is_complete(),
            LlmMode::Always => true,
        };
        if let (true, Some(llm)) = (want_llm, &self.llm) {
            let digest = extract::page_digest(&html, &final_url, DIGEST_CHARS);
            match llm.extract_product(&digest).await {
                Ok(product) => {
                    merge_llm(&mut outcome.info, product, &final_url);
                    outcome.llm_used = Some(llm.label());
                }
                Err(e) => {
                    tracing::warn!(url = %final_url, error = %e, "LLM extraction failed");
                    outcome.warnings.push("The AI assistant couldn't read this page; showing what we found directly.".into());
                }
            }
        }
        if outcome.info.title.is_none() && outcome.info.price_cents.is_none() {
            outcome
                .warnings
                .push("We couldn't find product details on that page. Fill in what you know below.".into());
        }
        Ok(outcome)
    }
}

/// Merge model output into heuristic results. Structured data (JSON-LD, microdata, meta
/// tags, known store layouts) always wins; model output replaces generic guesses and fills gaps.
pub fn merge_llm(info: &mut ProductInfo, p: LlmProduct, page_url: &Url) {
    let replaceable = |s: Source| !s.is_structured();
    let title = extract::clean_text(&p.title);
    if !title.is_empty() && replaceable(info.title_source) {
        info.title = Some(extract::truncate(&title, 200));
        info.title_source = Source::Llm;
    }
    if info.price_cents.is_none() && !p.price.trim().is_empty() {
        let currency = llm_currency(&p.currency).or_else(|| info.currency.clone());
        // The model is asked for a plain dot-decimal number; fall back to the lenient parser.
        let parsed = match p.price.trim().parse::<f64>() {
            Ok(f) if f.is_finite() => {
                let cur = currency.clone().unwrap_or_else(|| "USD".into());
                Some(((f * price::minor_units(&cur) as f64).round() as i64, cur))
            }
            _ => price::parse_price(&p.price, currency.as_deref()),
        };
        if let Some((cents, cur)) = parsed {
            if cents > 0 {
                info.price_cents = Some(cents);
                info.currency = Some(currency.unwrap_or(cur));
                info.price_source = Source::Llm;
            }
        }
    }
    if info.currency.is_none() {
        info.currency = llm_currency(&p.currency);
    }
    if replaceable(info.image_source) {
        if let Some(img) = extract::absolutize(page_url, &p.image_url) {
            info.image_url = Some(img);
            info.image_source = Source::Llm;
        }
    }
    let store = extract::clean_text(&p.store);
    if !store.is_empty() && (info.store.is_none() || info.store == extract::store_from_url(page_url)) {
        info.store = Some(extract::truncate(&store, 80));
    }
    let desc = extract::clean_text(&p.description);
    if info.description.is_none() && !desc.is_empty() {
        info.description = Some(extract::truncate(&desc, 300));
    }
}

fn llm_currency(raw: &str) -> Option<String> {
    let c = raw.trim();
    (!c.is_empty()).then(|| price::normalize_currency(c)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_fills_gaps_but_not_structured_fields() {
        let url = Url::parse("https://www.example.com/p/1").unwrap();
        let mut info = ProductInfo {
            title: Some("Example - Great Lamp - Buy Now".into()),
            title_source: Source::Generic,
            price_cents: Some(2500),
            currency: Some("USD".into()),
            price_source: Source::JsonLd,
            store: Some("Example".into()),
            ..Default::default()
        };
        merge_llm(
            &mut info,
            LlmProduct {
                title: "Great Lamp".into(),
                price: "99.00".into(),
                currency: "USD".into(),
                image_url: "/img/lamp.jpg".into(),
                store: "Example Home".into(),
                description: "A lamp.".into(),
            },
            &url,
        );
        assert_eq!(info.title.as_deref(), Some("Great Lamp"));
        assert_eq!(info.price_cents, Some(2500), "structured price must win");
        assert_eq!(info.image_url.as_deref(), Some("https://www.example.com/img/lamp.jpg"));
        assert_eq!(info.store.as_deref(), Some("Example Home"));
    }

    #[test]
    fn llm_price_used_when_missing() {
        let url = Url::parse("https://shop.test/x").unwrap();
        let mut info = ProductInfo::default();
        merge_llm(
            &mut info,
            LlmProduct { price: "1299.5".into(), currency: "EUR".into(), ..Default::default() },
            &url,
        );
        assert_eq!(info.price_cents, Some(129950));
        assert_eq!(info.currency.as_deref(), Some("EUR"));
        assert_eq!(info.price_source, Source::Llm);
    }
}
