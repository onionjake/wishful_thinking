//! Structured-data and heuristic extraction of product details from an HTML page.
//!
//! Sources are tried from most to least reliable:
//! 1. schema.org `Product` JSON-LD (what most shops publish for search engines)
//! 2. schema.org microdata (`itemprop="price"` …)
//! 3. OpenGraph / Twitter card / `product:price:*` meta tags
//! 4. A handful of well-known store layouts (Amazon, Shopify-style)
//! 5. Generic fallbacks: `<title>`, `<h1>`, first large image

use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use super::price;

/// Where a field came from; used to decide whether an LLM result should override it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    None,
    JsonLd,
    Microdata,
    Meta,
    StoreLayout,
    Generic,
    Llm,
}

impl Source {
    /// Structured sources are trusted over model output.
    pub fn is_structured(self) -> bool {
        matches!(self, Source::JsonLd | Source::Microdata | Source::Meta | Source::StoreLayout)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProductInfo {
    pub title: Option<String>,
    pub title_source: Source,
    pub price_cents: Option<i64>,
    pub currency: Option<String>,
    pub price_source: Source,
    pub image_url: Option<String>,
    pub image_source: Source,
    pub description: Option<String>,
    pub store: Option<String>,
    pub brand: Option<String>,
}

impl ProductInfo {
    /// True when the core fields a wish list needs are all present.
    pub fn is_complete(&self) -> bool {
        self.title.is_some() && self.price_cents.is_some() && self.image_url.is_some()
    }

    fn set_title(&mut self, v: Option<String>, src: Source) {
        if self.title.is_none() {
            if let Some(v) = v.map(|s| clean_text(&s)).filter(|s| !s.is_empty()) {
                self.title = Some(v);
                self.title_source = src;
            }
        }
    }

    fn set_image(&mut self, v: Option<String>, base: &Url, src: Source) {
        if self.image_url.is_none() {
            if let Some(abs) = v.and_then(|s| absolutize(base, &s)) {
                self.image_url = Some(abs);
                self.image_source = src;
            }
        }
    }

    fn set_price(&mut self, cents: Option<i64>, currency: Option<String>, src: Source) {
        let took_price = self.price_cents.is_none() && cents.is_some_and(|c| c > 0);
        if took_price {
            self.price_cents = cents;
            self.price_source = src;
        }
        if currency.is_some() && (took_price || self.currency.is_none()) {
            self.currency = currency;
        }
    }

    fn set_description(&mut self, v: Option<String>) {
        if self.description.is_none() {
            self.description = v.map(|s| truncate(&clean_text(&s), 500)).filter(|s| !s.is_empty());
        }
    }
}

pub fn clean_text(s: &str) -> String {
    let decoded = decode_entities(s);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// JSON-LD and meta content sometimes carries leftover HTML entities.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

pub fn absolutize(base: &Url, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with("data:") {
        return None;
    }
    let u = base.join(href).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

/// A friendly store name derived from the host, e.g. `www.target.com` -> `Target`.
pub fn store_from_url(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let host = host.trim_start_matches("www.").trim_start_matches("m.");
    let parts: Vec<&str> = host.split('.').collect();
    // Pick the registrable label: second-to-last, or third-to-last for e.g. "co.uk".
    let label = if parts.len() >= 3 && parts[parts.len() - 2].len() <= 3 && parts[parts.len() - 1].len() == 2 {
        parts[parts.len() - 3]
    } else if parts.len() >= 2 {
        parts[parts.len() - 2]
    } else {
        parts[0]
    };
    let mut c = label.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str())
}

pub fn extract(html: &str, page_url: &Url) -> ProductInfo {
    let doc = Html::parse_document(html);
    let mut info = ProductInfo::default();

    extract_json_ld(&doc, page_url, &mut info);
    extract_microdata(&doc, page_url, &mut info);
    extract_meta(&doc, page_url, &mut info);
    extract_store_layouts(&doc, page_url, &mut info);
    extract_generic(&doc, page_url, &mut info);

    if info.store.is_none() {
        info.store = store_from_url(page_url);
    }
    if let Some(t) = info.title.take() {
        info.title = Some(tidy_title(&t, info.store.as_deref()));
    }
    info
}

/// Remove store names and SEO noise from page titles: "Amazon.com: Foo : Toys & Games" -> "Foo".
pub fn tidy_title(title: &str, store: Option<&str>) -> String {
    let mut t = title.trim().to_string();
    if let Some(rest) = t.strip_prefix("Amazon.com: ").or_else(|| t.strip_prefix("Amazon.com : ")) {
        t = rest.to_string();
        if let Some(idx) = t.rfind(" : ") {
            t.truncate(idx);
        }
    }
    if let Some(store) = store {
        let lower_store = store.to_lowercase();
        for sep in [" | ", " - ", " – ", " — ", " :: "] {
            if let Some(idx) = t.rfind(sep) {
                let tail = t[idx + sep.len()..].to_lowercase();
                if tail.contains(&lower_store) || lower_store.contains(tail.trim()) {
                    t.truncate(idx);
                    break;
                }
            }
        }
    }
    t.trim().to_string()
}

// ---------------------------------------------------------------- JSON-LD

fn extract_json_ld(doc: &Html, base: &Url, info: &mut ProductInfo) {
    for script in doc.select(&sel(r#"script[type="application/ld+json"]"#)) {
        let raw: String = script.text().collect();
        let Ok(value) = serde_json::from_str::<Value>(raw.trim()) else {
            continue;
        };
        let mut products = Vec::new();
        collect_products(&value, &mut products, 0);
        for p in products {
            apply_product(p, base, info);
        }
    }
}

fn type_matches(v: &Value, wanted: &[&str]) -> bool {
    match v.get("@type") {
        Some(Value::String(t)) => wanted.iter().any(|w| t.eq_ignore_ascii_case(w) || t.ends_with(&format!("/{w}"))),
        Some(Value::Array(ts)) => ts.iter().any(|t| t.as_str().is_some_and(|t| wanted.iter().any(|w| t.eq_ignore_ascii_case(w)))),
        _ => false,
    }
}

fn collect_products<'a>(v: &'a Value, out: &mut Vec<&'a Value>, depth: usize) {
    if depth > 6 {
        return;
    }
    match v {
        Value::Array(items) => items.iter().for_each(|i| collect_products(i, out, depth + 1)),
        Value::Object(map) => {
            if type_matches(v, &["Product", "ProductGroup", "IndividualProduct", "ProductModel", "Book", "Vehicle"]) {
                out.push(v);
            }
            for key in ["@graph", "mainEntity", "itemListElement", "item"] {
                if let Some(child) = map.get(key) {
                    collect_products(child, out, depth + 1);
                }
            }
        }
        _ => {}
    }
}

fn json_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Object(m) => m.get("name").or_else(|| m.get("url")).or_else(|| m.get("@value")).and_then(json_str),
        Value::Array(a) => a.first().and_then(json_str),
        _ => None,
    }
}

fn json_image(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => a.iter().find_map(json_image),
        Value::Object(m) => m.get("url").or_else(|| m.get("contentUrl")).and_then(json_image),
        _ => None,
    }
}

fn apply_product(p: &Value, base: &Url, info: &mut ProductInfo) {
    info.set_title(p.get("name").and_then(json_str), Source::JsonLd);
    info.set_image(p.get("image").and_then(json_image), base, Source::JsonLd);
    info.set_description(p.get("description").and_then(json_str));
    if info.brand.is_none() {
        info.brand = p.get("brand").and_then(json_str).map(|s| clean_text(&s));
    }
    if let Some(offers) = p.get("offers") {
        let (cents, cur) = offer_price(offers);
        info.set_price(cents, cur, Source::JsonLd);
    }
    // ProductGroup: variants carry the offers.
    if info.price_cents.is_none() {
        if let Some(Value::Array(variants)) = p.get("hasVariant") {
            for v in variants {
                if let Some(offers) = v.get("offers") {
                    let (cents, cur) = offer_price(offers);
                    info.set_price(cents, cur, Source::JsonLd);
                }
                info.set_image(v.get("image").and_then(json_image), base, Source::JsonLd);
            }
        }
    }
}

fn offer_price(offers: &Value) -> (Option<i64>, Option<String>) {
    match offers {
        Value::Array(list) => {
            // Prefer an in-stock offer, else the first one with a price.
            let mut fallback = (None, None);
            for o in list {
                let r = offer_price(o);
                if r.0.is_some() {
                    let in_stock = o
                        .get("availability")
                        .and_then(Value::as_str)
                        .is_some_and(|a| a.contains("InStock"));
                    if in_stock {
                        return r;
                    }
                    if fallback.0.is_none() {
                        fallback = r;
                    }
                }
            }
            fallback
        }
        Value::Object(_) => {
            let currency = offers
                .get("priceCurrency")
                .and_then(json_str)
                .and_then(|c| price::normalize_currency(&c))
                .or_else(|| {
                    offers
                        .get("priceSpecification")
                        .and_then(|ps| ps.get("priceCurrency").or_else(|| ps.get(0).and_then(|p| p.get("priceCurrency"))))
                        .and_then(json_str)
                        .and_then(|c| price::normalize_currency(&c))
                });
            let cur_code = currency.clone().unwrap_or_else(|| "USD".into());
            let raw = offers
                .get("price")
                .or_else(|| offers.get("lowPrice"))
                .or_else(|| offers.get("priceSpecification").and_then(|ps| ps.get("price").or_else(|| ps.get(0).and_then(|p| p.get("price")))))
                .or_else(|| offers.get("highPrice"));
            let cents = raw.and_then(|r| match r {
                // A JSON number is always a dot-decimal value.
                Value::Number(n) => n.as_f64().map(|f| (f * price::minor_units(&cur_code) as f64).round() as i64),
                // schema.org says dot-decimal, but some sites write "1.299,00" anyway.
                Value::String(s) => s
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .map(|f| (f * price::minor_units(&cur_code) as f64).round() as i64)
                    .or_else(|| price::parse_amount(s, &cur_code)),
                _ => None,
            });
            // Some sites nest a further "offers" (AggregateOffer → offers[]).
            if cents.is_none() {
                if let Some(inner) = offers.get("offers") {
                    return offer_price(inner);
                }
            }
            (cents, currency)
        }
        _ => (None, None),
    }
}

// ---------------------------------------------------------------- microdata

fn attr_or_text(el: ElementRef) -> Option<String> {
    let v = el
        .value()
        .attr("content")
        .map(str::to_string)
        .or_else(|| el.value().attr("src").map(str::to_string))
        .or_else(|| el.value().attr("href").map(str::to_string))
        .unwrap_or_else(|| el.text().collect::<String>());
    let v = clean_text(&v);
    (!v.is_empty()).then_some(v)
}

fn extract_microdata(doc: &Html, base: &Url, info: &mut ProductInfo) {
    let scope = doc
        .select(&sel(r#"[itemtype*="schema.org/Product"]"#))
        .next();
    let Some(scope) = scope else { return };
    let first = |s: &str| scope.select(&sel(s)).next().and_then(attr_or_text);
    info.set_title(first(r#"[itemprop="name"]"#), Source::Microdata);
    info.set_image(first(r#"[itemprop="image"]"#), base, Source::Microdata);
    info.set_description(first(r#"[itemprop="description"]"#));
    let currency = first(r#"[itemprop="priceCurrency"]"#).and_then(|c| price::normalize_currency(&c));
    if let Some(p) = first(r#"[itemprop="price"]"#).or_else(|| first(r#"[itemprop="lowPrice"]"#)) {
        let parsed = price::parse_price(&p, currency.as_deref());
        info.set_price(parsed.as_ref().map(|p| p.0), currency.or(parsed.map(|p| p.1)), Source::Microdata);
    }
}

// ---------------------------------------------------------------- meta tags

fn meta(doc: &Html, keys: &[&str]) -> Option<String> {
    for key in keys {
        for attr in ["property", "name", "itemprop"] {
            let s = format!(r#"meta[{attr}="{key}"]"#);
            if let Some(v) = doc
                .select(&sel(&s))
                .filter_map(|m| m.value().attr("content"))
                .map(clean_text)
                .find(|v| !v.is_empty())
            {
                return Some(v);
            }
        }
    }
    None
}

fn extract_meta(doc: &Html, base: &Url, info: &mut ProductInfo) {
    info.set_title(meta(doc, &["og:title", "twitter:title"]), Source::Meta);
    info.set_image(
        meta(doc, &["og:image:secure_url", "og:image", "og:image:url", "twitter:image", "twitter:image:src"]),
        base,
        Source::Meta,
    );
    info.set_description(meta(doc, &["og:description", "twitter:description", "description"]));
    if info.store.is_none() {
        info.store = meta(doc, &["og:site_name", "application-name"]);
    }
    let currency = meta(doc, &["product:price:currency", "og:price:currency"]).and_then(|c| price::normalize_currency(&c));
    if let Some(amount) = meta(doc, &["product:price:amount", "og:price:amount", "product:sale_price:amount"]) {
        let parsed = price::parse_price(&amount, currency.as_deref());
        info.set_price(parsed.as_ref().map(|p| p.0), currency.clone().or(parsed.map(|p| p.1)), Source::Meta);
    }
    // Twitter "label1=Price, data1=$19.99" product cards.
    if info.price_cents.is_none() {
        for i in 1..=2 {
            let label = meta(doc, &[&format!("twitter:label{i}")]);
            if label.is_some_and(|l| l.to_lowercase().contains("price")) {
                if let Some(data) = meta(doc, &[&format!("twitter:data{i}")]) {
                    let parsed = price::parse_price(&data, currency.as_deref());
                    info.set_price(parsed.as_ref().map(|p| p.0), parsed.map(|p| p.1), Source::Meta);
                }
            }
        }
    }
}

// ---------------------------------------------------------------- store layouts

fn first_text(doc: &Html, selectors: &[&str]) -> Option<String> {
    selectors.iter().find_map(|s| {
        doc.select(&sel(s))
            .map(|e| clean_text(&e.text().collect::<String>()))
            .find(|t| !t.is_empty())
    })
}

fn extract_store_layouts(doc: &Html, base: &Url, info: &mut ProductInfo) {
    // Amazon (and its many regional domains).
    info.set_title(first_text(doc, &["#productTitle", "#title"]), Source::StoreLayout);
    if info.image_url.is_none() {
        if let Some(img) = doc.select(&sel("#landingImage, #imgBlkFront, #main-image")).next() {
            // data-a-dynamic-image is a JSON map of url -> [w, h]; pick the largest.
            let dynamic = img
                .value()
                .attr("data-a-dynamic-image")
                .and_then(|j| serde_json::from_str::<serde_json::Map<String, Value>>(j).ok())
                .and_then(|m| {
                    m.into_iter()
                        .max_by_key(|(_, v)| v.get(0).and_then(Value::as_i64).unwrap_or(0))
                        .map(|(k, _)| k)
                });
            let src = img.value().attr("data-old-hires").filter(|s| !s.is_empty()).map(str::to_string)
                .or(dynamic)
                .or_else(|| img.value().attr("src").map(str::to_string));
            info.set_image(src, base, Source::StoreLayout);
        }
    }
    if info.price_cents.is_none() {
        let candidates = [
            "#corePrice_feature_div .a-offscreen",
            "#corePriceDisplay_desktop_feature_div .a-offscreen",
            "#priceblock_ourprice",
            "#priceblock_dealprice",
            ".a-price .a-offscreen",
            // Common e-commerce themes.
            "[data-testid=\"product-price\"]",
            "[data-test=\"product-price\"]",
            ".product__price .price-item--sale",
            ".product__price .price-item--regular",
            ".price-item--regular",
            ".product-price",
            ".price .amount",
        ];
        if let Some(raw) = first_text(doc, &candidates) {
            if let Some((cents, cur)) = price::parse_price(&raw, info.currency.as_deref()) {
                info.set_price(Some(cents), Some(cur), Source::StoreLayout);
            }
        }
    }
}

// ---------------------------------------------------------------- generic fallbacks

fn extract_generic(doc: &Html, base: &Url, info: &mut ProductInfo) {
    info.set_title(first_text(doc, &["h1"]), Source::Generic);
    info.set_title(first_text(doc, &["title"]), Source::Generic);
    if info.image_url.is_none() {
        let link = doc
            .select(&sel(r#"link[rel="image_src"]"#))
            .next()
            .and_then(|l| l.value().attr("href").map(str::to_string));
        info.set_image(link, base, Source::Generic);
    }
    if info.image_url.is_none() {
        // First image that declares itself reasonably large.
        let big = doc.select(&sel("img[src]")).find(|img| {
            let w = img.value().attr("width").and_then(|w| w.trim_end_matches("px").parse::<u32>().ok()).unwrap_or(0);
            w >= 200
        });
        info.set_image(big.and_then(|i| i.value().attr("src").map(str::to_string)), base, Source::Generic);
    }
}

// ---------------------------------------------------------------- LLM digest

/// A compact, text-only summary of a page to hand to an LLM: metadata, structured data
/// snippets, candidate images and visible text, capped at `max_chars`.
pub fn page_digest(html: &str, page_url: &Url, max_chars: usize) -> String {
    let doc = Html::parse_document(html);
    let mut out = String::new();
    out.push_str(&format!("URL: {page_url}\n"));
    if let Some(t) = first_text(&doc, &["title"]) {
        out.push_str(&format!("<title>: {t}\n"));
    }
    out.push_str("\n## Meta tags\n");
    for m in doc.select(&sel("meta[content]")).take(60) {
        let key = m.value().attr("property").or_else(|| m.value().attr("name")).or_else(|| m.value().attr("itemprop"));
        if let (Some(k), Some(c)) = (key, m.value().attr("content")) {
            if k.starts_with("og:") || k.starts_with("product:") || k.starts_with("twitter:") || k == "description" || k.contains("price") {
                out.push_str(&format!("{k} = {}\n", truncate(&clean_text(c), 300)));
            }
        }
    }
    let ld: Vec<String> = doc
        .select(&sel(r#"script[type="application/ld+json"]"#))
        .map(|s| truncate(&s.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" "), 2500))
        .take(4)
        .collect();
    if !ld.is_empty() {
        out.push_str("\n## JSON-LD\n");
        for l in ld {
            out.push_str(&l);
            out.push('\n');
        }
    }
    out.push_str("\n## Candidate images\n");
    let mut seen = std::collections::HashSet::new();
    for img in doc.select(&sel("img")).take(200) {
        let src = img.value().attr("src").or_else(|| img.value().attr("data-src")).or_else(|| img.value().attr("data-old-hires"));
        if let Some(abs) = src.and_then(|s| absolutize(page_url, s)) {
            let lower = abs.to_lowercase();
            if lower.contains("sprite") || lower.contains("pixel") || lower.ends_with(".svg") || lower.ends_with(".gif") {
                continue;
            }
            if seen.insert(abs.clone()) {
                let alt = img.value().attr("alt").map(clean_text).unwrap_or_default();
                out.push_str(&format!("- {abs} (alt: {})\n", truncate(&alt, 80)));
                if seen.len() >= 15 {
                    break;
                }
            }
        }
    }
    out.push_str("\n## Visible text\n");
    let body_text = visible_text(&doc);
    out.push_str(&body_text);
    truncate(&out, max_chars)
}

fn visible_text(doc: &Html) -> String {
    let skip = ["script", "style", "noscript", "svg", "nav", "footer", "header", "iframe", "template"];
    let mut out = String::new();
    let root = doc.root_element();
    let mut stack = vec![root];
    // Iterative DFS over elements, collecting text nodes outside skipped tags.
    while let Some(el) = stack.pop() {
        if skip.contains(&el.value().name()) {
            continue;
        }
        for child in el.children().rev() {
            if let Some(e) = ElementRef::wrap(child) {
                stack.push(e);
            }
        }
        for child in el.children() {
            if let Some(t) = child.value().as_text() {
                let t = t.trim();
                if !t.is_empty() {
                    out.push_str(t);
                    out.push(' ');
                }
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("https://shop.example.com/products/robot").unwrap()
    }

    #[test]
    fn json_ld_product() {
        let html = r#"<html><head><title>Robot Kit | Example Shop</title>
        <script type="application/ld+json">{"@context":"https://schema.org","@graph":[{"@type":"WebPage"},
          {"@type":"Product","name":"Build-a-Bot Robot Kit","image":["/img/robot.jpg"],
           "brand":{"@type":"Brand","name":"Botco"},
           "offers":{"@type":"Offer","price":"49.99","priceCurrency":"USD","availability":"https://schema.org/InStock"}}]}
        </script></head><body></body></html>"#;
        let p = extract(html, &url());
        assert_eq!(p.title.as_deref(), Some("Build-a-Bot Robot Kit"));
        assert_eq!(p.price_cents, Some(4999));
        assert_eq!(p.currency.as_deref(), Some("USD"));
        assert_eq!(p.image_url.as_deref(), Some("https://shop.example.com/img/robot.jpg"));
        assert_eq!(p.brand.as_deref(), Some("Botco"));
        assert_eq!(p.price_source, Source::JsonLd);
    }

    #[test]
    fn json_ld_numeric_and_aggregate() {
        let html = r#"<script type="application/ld+json">[{"@type":["Product"],"name":"Mug",
          "offers":{"@type":"AggregateOffer","lowPrice":12.5,"highPrice":20,"priceCurrency":"EUR"}}]</script>"#;
        let p = extract(html, &url());
        assert_eq!(p.price_cents, Some(1250));
        assert_eq!(p.currency.as_deref(), Some("EUR"));
    }

    #[test]
    fn open_graph_product() {
        let html = r#"<head>
          <meta property="og:title" content="Cozy Knit Blanket" />
          <meta property="og:image" content="https://cdn.example.com/blanket.jpg" />
          <meta property="og:site_name" content="Blanket Co" />
          <meta property="product:price:amount" content="89.00" />
          <meta property="product:price:currency" content="GBP" /></head>"#;
        let p = extract(html, &url());
        assert_eq!(p.title.as_deref(), Some("Cozy Knit Blanket"));
        assert_eq!(p.price_cents, Some(8900));
        assert_eq!(p.currency.as_deref(), Some("GBP"));
        assert_eq!(p.store.as_deref(), Some("Blanket Co"));
    }

    #[test]
    fn amazon_layout() {
        let html = r#"<html><head><title>Amazon.com: LEGO Classic Bricks : Toys &amp; Games</title></head><body>
          <span id="productTitle">   LEGO Classic Large Creative Brick Box 10698   </span>
          <div id="corePrice_feature_div"><span class="a-price"><span class="a-offscreen">$47.99</span></span></div>
          <img id="landingImage" src="https://m.media-amazon.com/small.jpg"
               data-a-dynamic-image='{"https://m.media-amazon.com/small.jpg":[100,100],"https://m.media-amazon.com/big.jpg":[1000,1000]}'>
        </body></html>"#;
        let p = extract(html, &Url::parse("https://www.amazon.com/dp/B00NHQFA1I").unwrap());
        assert_eq!(p.title.as_deref(), Some("LEGO Classic Large Creative Brick Box 10698"));
        assert_eq!(p.price_cents, Some(4799));
        assert_eq!(p.image_url.as_deref(), Some("https://m.media-amazon.com/big.jpg"));
        assert_eq!(p.store.as_deref(), Some("Amazon"));
    }

    #[test]
    fn microdata() {
        let html = r#"<div itemscope itemtype="https://schema.org/Product">
          <h2 itemprop="name">Trail Shoes</h2><img itemprop="image" src="shoe.png">
          <span itemprop="priceCurrency" content="USD">$</span><span itemprop="price" content="120.00">120</span></div>"#;
        let p = extract(html, &url());
        assert_eq!(p.title.as_deref(), Some("Trail Shoes"));
        assert_eq!(p.price_cents, Some(12000));
        assert_eq!(p.image_url.as_deref(), Some("https://shop.example.com/products/shoe.png"));
    }

    #[test]
    fn generic_fallback_and_title_tidy() {
        let html = r#"<html><head><title>Wooden Train Set - Toyland</title></head><body><p>Hello</p></body></html>"#;
        let p = extract(html, &Url::parse("https://toyland.co.uk/train").unwrap());
        assert_eq!(p.store.as_deref(), Some("Toyland"));
        assert_eq!(p.title.as_deref(), Some("Wooden Train Set"));
        assert!(p.price_cents.is_none());
        assert!(!p.is_complete());
    }

    #[test]
    fn digest_contains_text_and_images() {
        let html = r#"<html><head><title>T</title><script>var x=1;</script></head>
            <body><nav>Menu</nav><h1>Fancy Lamp</h1><p>Only $35.00 today</p><img src="/lamp.jpg" alt="Lamp"></body></html>"#;
        let d = page_digest(html, &url(), 5000);
        assert!(d.contains("Fancy Lamp"));
        assert!(d.contains("$35.00"));
        assert!(d.contains("https://shop.example.com/lamp.jpg"));
        assert!(!d.contains("var x"));
        assert!(!d.contains("Menu"));
    }
}
