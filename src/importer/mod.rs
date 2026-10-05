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
        let structured = [
            self.info.title_source,
            self.info.price_source,
            self.info.image_source,
        ]
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
        Importer {
            http: fetch::build_client(allow_private),
            llm,
            mode,
            allow_private,
        }
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
            info: ProductInfo {
                store: extract::store_from_url(&url),
                ..Default::default()
            },
            llm_used: None,
            warnings: Vec::new(),
        };

        let (final_url, html) = match fetch::fetch(&self.http, &url).await {
            Ok(fetch::Fetched::Html { final_url, body }) => (final_url, body),
            Ok(fetch::Fetched::Image { final_url }) => {
                outcome.info.image_url = Some(final_url.to_string());
                outcome.info.image_source = Source::Generic;
                outcome
                    .warnings
                    .push("That link is an image — it's been set as the item picture.".into());
                return Ok(outcome);
            }
            Err(e @ (fetch::FetchError::InvalidUrl | fetch::FetchError::Forbidden)) => {
                return Err(e)
            }
            Err(e) => {
                outcome.warnings.push(e.to_string());
                guess_title_from_url(&mut outcome.info, &url);
                return Ok(outcome);
            }
        };

        if extract::is_bot_wall(&html, &final_url) {
            outcome.warnings.push(
                "This store showed a robot check instead of the product, so we couldn't read it. We've guessed what we can — please fill in the rest."
                    .into(),
            );
            guess_title_from_url(&mut outcome.info, &url);
            return Ok(outcome);
        }
        if redirected_away(&url, &final_url) {
            outcome.warnings.push(format!(
                "The store sent us to a different page ({}), so the product may be unavailable. Double-check the details.",
                final_url.path()
            ));
            guess_title_from_url(&mut outcome.info, &url);
            return Ok(outcome);
        }

        outcome.url = final_url.to_string();
        outcome.info = extract::extract(&html, &final_url);
        if !outcome.info.is_complete() {
            self.shopify_fallback(&mut outcome.info, &html, &final_url)
                .await;
        }

        let want_llm = match self.mode {
            LlmMode::Off => false,
            LlmMode::Fallback => !outcome.info.is_complete(),
            LlmMode::Always => true,
        };
        if let (true, Some(llm)) = (want_llm, &self.llm) {
            let digest = extract::page_digest(&html, &final_url, DIGEST_CHARS);
            match llm.extract_product(&digest).await {
                Ok(product) => {
                    merge_llm(&mut outcome.info, product, &final_url, &html);
                    outcome.llm_used = Some(llm.label());
                }
                Err(e) => {
                    tracing::warn!(url = %final_url, error = %e, "LLM extraction failed");
                    outcome.warnings.push(
                        "The AI assistant couldn't read this page; showing what we found directly."
                            .into(),
                    );
                }
            }
        }
        if outcome.info.title.is_none() && outcome.info.price_cents.is_none() {
            outcome.warnings.push(
                "We couldn't find product details on that page. Fill in what you know below."
                    .into(),
            );
        }
        guess_title_from_url(&mut outcome.info, &url);
        Ok(outcome)
    }

    /// Shopify stores expose every product as JSON at `/products/<handle>.js`, which has the
    /// price and images even when the HTML renders them client-side.
    async fn shopify_fallback(&self, info: &mut ProductInfo, html: &str, page_url: &Url) {
        if !(html.contains("cdn.shopify.com")
            || html.contains("/cdn/shop/")
            || html.contains("Shopify.shop"))
        {
            return;
        }
        let Some(handle) = page_url
            .path_segments()
            .and_then(|segs| {
                segs.skip_while(|s| *s != "products")
                    .nth(1)
                    .map(str::to_string)
            })
            .filter(|h| !h.is_empty())
        else {
            return;
        };
        let Ok(json_url) = page_url.join(&format!("/products/{handle}.js")) else {
            return;
        };
        let Ok(fetch::Fetched::Html { body, .. }) = fetch::fetch(&self.http, &json_url).await
        else {
            return;
        };
        let Ok(p) = serde_json::from_str::<serde_json::Value>(&body) else {
            return;
        };
        let currency = shopify_currency(html)
            .or_else(|| info.currency.clone())
            .unwrap_or_else(|| "USD".into());
        if info.title.is_none() || !info.title_source.is_structured() {
            if let Some(t) = p.get("title").and_then(|t| t.as_str()) {
                info.title = Some(extract::clean_text(t));
                info.title_source = Source::StoreLayout;
            }
        }
        if info.price_cents.is_none() {
            // Shopify reports prices in minor units already.
            if let Some(c) = p.get("price").and_then(|c| c.as_i64()).filter(|c| *c > 0) {
                info.price_cents = Some(if price::minor_units(&currency) == 1 {
                    c / 100
                } else {
                    c
                });
                info.currency = Some(currency);
                info.price_source = Source::StoreLayout;
            }
        }
        if info.image_url.is_none() || !info.image_source.is_structured() {
            let img = p
                .get("featured_image")
                .and_then(|i| i.as_str())
                .or_else(|| p.pointer("/images/0").and_then(|i| i.as_str()));
            if let Some(abs) = img.and_then(|i| extract::absolutize(page_url, i)) {
                info.image_url = Some(extract::upgrade_image_url(&abs));
                info.image_source = Source::StoreLayout;
            }
        }
        if info.brand.is_none() {
            info.brand = p.get("vendor").and_then(|v| v.as_str()).map(str::to_string);
        }
    }
}

/// `Shopify.currency = {"active":"CAD","rate":"1.0"}` in the page source.
fn shopify_currency(html: &str) -> Option<String> {
    let idx = html.find("Shopify.currency")?;
    let rest = &html[idx..html.len().min(idx + 200)];
    let a = rest.find("\"active\":\"")? + "\"active\":\"".len();
    let code: String = rest[a..].chars().take(3).collect();
    price::normalize_currency(&code)
}

fn guess_title_from_url(info: &mut ProductInfo, url: &Url) {
    if info.title.is_none() {
        if let Some(t) = extract::title_from_url(url) {
            info.title = Some(t);
            info.title_source = Source::Generic;
        }
    }
}

/// True when the store redirected a product link somewhere unrelated on the same site
/// (typically a category page for a discontinued product).
fn redirected_away(original: &Url, fin: &Url) -> bool {
    let host = |u: &Url| {
        u.host_str()
            .unwrap_or("")
            .trim_start_matches("www.")
            .to_string()
    };
    if host(original) != host(fin) || original.path() == fin.path() {
        return false;
    }
    let last = original
        .path_segments()
        .and_then(|mut s| s.rfind(|x| !x.is_empty()))
        .unwrap_or("");
    let last = last.trim_end_matches(".html");
    last.len() >= 4 && !fin.as_str().contains(last)
}

/// Merge model output into heuristic results. Structured data (JSON-LD, microdata, meta
/// tags, known store layouts) always wins; model output replaces generic guesses and fills gaps.
pub fn merge_llm(info: &mut ProductInfo, p: LlmProduct, page_url: &Url, page_html: &str) {
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
        // Only accept images that really appear on the page (no invented URLs).
        let on_page = |img: &str| {
            Url::parse(img).is_ok_and(|u| {
                let path = u.path();
                path.len() > 1
                    && (page_html.contains(path) || page_html.contains(&path.replace('/', "\\/")))
            })
        };
        if let Some(img) = extract::absolutize(page_url, &p.image_url).filter(|i| on_page(i)) {
            info.image_url = Some(extract::upgrade_image_url(&img));
            info.image_source = Source::Llm;
        }
    }
    let store = extract::clean_text(&p.store);
    if !store.is_empty()
        && (info.store.is_none() || info.store == extract::store_from_url(page_url))
    {
        info.store = Some(extract::truncate(&store, 80));
    }
    let desc = extract::clean_text(&p.description);
    if info.description.is_none() && !desc.is_empty() {
        info.description = Some(extract::truncate(&desc, 300));
    }
}

fn llm_currency(raw: &str) -> Option<String> {
    let c = raw.trim();
    (!c.is_empty())
        .then(|| price::normalize_currency(c))
        .flatten()
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
            r#"<img src="/img/lamp.jpg">"#,
        );
        assert_eq!(info.title.as_deref(), Some("Great Lamp"));
        assert_eq!(info.price_cents, Some(2500), "structured price must win");
        assert_eq!(
            info.image_url.as_deref(),
            Some("https://www.example.com/img/lamp.jpg")
        );
        assert_eq!(info.store.as_deref(), Some("Example Home"));
    }

    #[test]
    fn redirect_detection() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(redirected_away(
            &u("https://www.ikea.com/us/en/p/kallax-80275887/"),
            &u("https://www.ikea.com/us/en/cat/products/")
        ));
        assert!(!redirected_away(
            &u("https://amzn.to/abc"),
            &u("https://www.amazon.com/dp/B0")
        ));
        assert!(!redirected_away(
            &u("https://shop.test/p/123"),
            &u("https://shop.test/products/lamp/123")
        ));
        assert!(!redirected_away(
            &u("https://shop.test/p/lamp"),
            &u("https://shop.test/p/lamp/")
        ));
    }

    #[test]
    fn shopify_currency_detected() {
        assert_eq!(
            shopify_currency(
                r#"<script>Shopify.currency = {"active":"CAD","rate":"1.0"};</script>"#
            )
            .as_deref(),
            Some("CAD")
        );
    }

    #[test]
    fn llm_price_used_when_missing() {
        let url = Url::parse("https://shop.test/x").unwrap();
        let mut info = ProductInfo::default();
        merge_llm(
            &mut info,
            LlmProduct {
                price: "1299.5".into(),
                currency: "EUR".into(),
                image_url: "https://shop.test/made-up.jpg".into(),
                ..Default::default()
            },
            &url,
            "<html></html>",
        );
        assert!(
            info.image_url.is_none(),
            "images not on the page are rejected"
        );
        assert_eq!(info.price_cents, Some(129950));
        assert_eq!(info.currency.as_deref(), Some("EUR"));
        assert_eq!(info.price_source, Source::Llm);
    }
}
