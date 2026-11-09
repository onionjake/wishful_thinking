//! Product data gathered by the bookmarklet in the user's own browser.
//!
//! Many big stores (Walmart, Target, …) answer server-side requests with a robot check, but
//! the shopper already has the real page open. The bookmarklet copies the parts of that page
//! the extractor reads — meta tags, JSON-LD, microdata, the heading, the main photo and a bit
//! of visible text — and this module rebuilds them into a small HTML document so the normal
//! extraction pipeline can run on it unchanged.

use serde::Deserialize;

/// Caps that keep a hostile or enormous page from producing a huge document.
const MAX_META: usize = 80;
const MAX_JSON_LD: usize = 6;
const MAX_JSON_LD_CHARS: usize = 60_000;
const MAX_MICRODATA: usize = 60;
const MAX_FIELD_CHARS: usize = 2_000;
const MAX_TEXT_CHARS: usize = 8_000;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PageSnapshot {
    /// `document.title`.
    pub title: String,
    /// `[key, content]` for `<meta property|name|itemprop=key content=…>` tags.
    pub meta: Vec<(String, String)>,
    /// Text of `<script type="application/ld+json">` blocks.
    pub json_ld: Vec<String>,
    /// `[itemprop, value]` pairs inside the page's schema.org `Product` scope.
    pub microdata: Vec<(String, String)>,
    /// Text of the first `<h1>`.
    pub h1: String,
    /// Address of the largest visible picture.
    pub image: String,
    /// Text of the most likely price element.
    pub price: String,
    /// A slice of the page's visible text, for the AI fallback.
    pub text: String,
}

impl PageSnapshot {
    /// Rebuild the snapshot as a minimal HTML page the extractor understands.
    pub fn to_html(&self) -> String {
        let mut h = String::from("<!doctype html><html><head>");
        h.push_str(&format!("<title>{}</title>", esc(&self.title)));
        for (k, v) in self.meta.iter().take(MAX_META) {
            let k = esc(&cap(k, 100));
            let v = esc(&cap(v, MAX_FIELD_CHARS));
            h.push_str(&format!(
                r#"<meta property="{k}" name="{k}" content="{v}">"#
            ));
        }
        for j in self.json_ld.iter().take(MAX_JSON_LD) {
            // `</` can't appear inside a script element; `<\/` means the same thing in JSON.
            let j = cap(j, MAX_JSON_LD_CHARS).replace("</", "<\\/");
            h.push_str(&format!(
                r#"<script type="application/ld+json">{j}</script>"#
            ));
        }
        h.push_str("</head><body>");
        if !self.microdata.is_empty() {
            h.push_str(r#"<div itemscope itemtype="https://schema.org/Product">"#);
            for (k, v) in self.microdata.iter().take(MAX_MICRODATA) {
                h.push_str(&format!(
                    r#"<meta itemprop="{}" content="{}">"#,
                    esc(&cap(k, 100)),
                    esc(&cap(v, MAX_FIELD_CHARS))
                ));
            }
            h.push_str("</div>");
        }
        if !self.h1.trim().is_empty() {
            h.push_str(&format!("<h1>{}</h1>", esc(&cap(&self.h1, 500))));
        }
        if !self.price.trim().is_empty() {
            h.push_str(&format!(
                r#"<span class="product-price">{}</span>"#,
                esc(&cap(&self.price, 100))
            ));
        }
        if self.image.starts_with("http://") || self.image.starts_with("https://") {
            h.push_str(&format!(
                r#"<img src="{}" width="1000" alt="">"#,
                esc(&cap(&self.image, MAX_FIELD_CHARS))
            ));
        }
        if !self.text.trim().is_empty() {
            h.push_str(&format!(
                "<main>{}</main>",
                esc(&cap(&self.text, MAX_TEXT_CHARS))
            ));
        }
        h.push_str("</body></html>");
        h
    }
}

fn cap(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::importer::extract::{self, Source};
    use url::Url;

    fn walmart() -> Url {
        Url::parse("https://www.walmart.com/ip/15094307760?sid=abc").unwrap()
    }

    #[test]
    fn json_ld_from_the_browser_is_extracted() {
        let snap = PageSnapshot {
            title: "Mini Brands Series 3 - Walmart.com".into(),
            json_ld: vec![r#"{"@context":"https://schema.org","@type":"Product","name":"Zuru 5 Surprise Mini Brands Series 3","image":"https://i5.walmartimages.com/seo/mini.jpeg","offers":{"@type":"Offer","price":9.97,"priceCurrency":"USD"}}"#.into()],
            ..Default::default()
        };
        let info = extract::extract(&snap.to_html(), &walmart());
        assert_eq!(
            info.title.as_deref(),
            Some("Zuru 5 Surprise Mini Brands Series 3")
        );
        assert_eq!(info.price_cents, Some(997));
        assert_eq!(
            info.image_url.as_deref(),
            Some("https://i5.walmartimages.com/seo/mini.jpeg")
        );
        assert_eq!(info.image_source, Source::JsonLd);
        assert_eq!(info.store.as_deref(), Some("Walmart"));
    }

    #[test]
    fn meta_microdata_and_fallbacks() {
        let snap = PageSnapshot {
            title: "Shop".into(),
            meta: vec![
                ("og:title".into(), "Cosy \"Blanket\" & Throw".into()),
                ("og:image".into(), "https://shop.test/b.jpg".into()),
            ],
            microdata: vec![
                ("price".into(), "24.50".into()),
                ("priceCurrency".into(), "EUR".into()),
            ],
            ..Default::default()
        };
        let info = extract::extract(
            &snap.to_html(),
            &Url::parse("https://shop.test/p/1").unwrap(),
        );
        assert_eq!(info.title.as_deref(), Some("Cosy \"Blanket\" & Throw"));
        assert_eq!(info.image_url.as_deref(), Some("https://shop.test/b.jpg"));
        assert_eq!(info.price_cents, Some(2450));
        assert_eq!(info.currency.as_deref(), Some("EUR"));

        // Nothing structured: heading, visible price and the big photo still work.
        let snap = PageSnapshot {
            h1: "Garden gnome".into(),
            price: "$12.00".into(),
            image: "https://shop.test/gnome.png".into(),
            ..Default::default()
        };
        let info = extract::extract(
            &snap.to_html(),
            &Url::parse("https://shop.test/p/2").unwrap(),
        );
        assert_eq!(info.title.as_deref(), Some("Garden gnome"));
        assert_eq!(info.price_cents, Some(1200));
        assert_eq!(
            info.image_url.as_deref(),
            Some("https://shop.test/gnome.png")
        );
    }

    /// What a real Walmart product page offered in Firefox: no Product JSON-LD, but
    /// OpenGraph tags and an `itemprop="price"` outside any Product scope.
    #[test]
    fn walmart_page_without_product_json_ld() {
        let img = "https://i5.walmartimages.com/seo/Mini-Brands-Fill-the-Fridge-Mini-Series-1-Capsule_09b27dc3.jpeg";
        let snap = PageSnapshot {
            title: "Mini Brands Fill the Fridge Mini Series 1 Capsule, Plastic - Walmart.com".into(),
            meta: vec![
                ("og:title".into(), "Mini Brands Fill the Fridge Mini Series 1 Capsule by ZURU - Surprise Unbox Collectible Toy for Kids Ages 8+ - Walmart.com".into()),
                ("og:type".into(), "product.item".into()),
                ("og:image".into(), img.into()),
                ("og:site_name".into(), "Walmart.com".into()),
            ],
            json_ld: vec![
                r#"{"@context":"https://schema.org","@type":"WebPage","speakable":{"@type":"SpeakableSpecification"}}"#.into(),
            ],
            h1: "Mini Brands Fill the Fridge Mini Series 1 Capsule by ZURU - Surprise Unbox Collectible Toy for Kids Ages 8+".into(),
            price: "$7.97".into(),
            image: img.into(),
            ..Default::default()
        };
        let url = Url::parse("https://www.walmart.com/ip/Mini-Brands-Fill-the-Fridge-Mini-Series-1-Capsule-by-ZURU-Surprise-Unbox-Collectible-Toy-for-Kids-Ages-8/15094307760").unwrap();
        let info = extract::extract(&snap.to_html(), &url);
        assert_eq!(
            info.title.as_deref(),
            Some("Mini Brands Fill the Fridge Mini Series 1 Capsule by ZURU - Surprise Unbox Collectible Toy for Kids Ages 8+")
        );
        assert_eq!(info.price_cents, Some(797));
        assert_eq!(info.currency.as_deref(), Some("USD"));
        assert_eq!(info.image_url.as_deref(), Some(img));
        assert!(info.is_complete());
    }

    #[test]
    fn page_content_cannot_break_out_of_the_document() {
        let snap = PageSnapshot {
            title: "</title><script>alert(1)</script>".into(),
            json_ld: vec![
                r#"{"@type":"Product","name":"x</script><img src=x onerror=alert(1)>"}"#.into(),
            ],
            image: "javascript:alert(1)".into(),
            ..Default::default()
        };
        let html = snap.to_html();
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("</script><img"));
        assert!(!html.contains("javascript:"));
        let info = extract::extract(&html, &walmart());
        assert_eq!(
            info.title.as_deref(),
            Some("x</script><img src=x onerror=alert(1)>"),
            "JSON-LD still parses after escaping"
        );
    }
}
