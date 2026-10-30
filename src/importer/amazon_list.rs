//! Import a whole public or shared Amazon wish list.
//!
//! Amazon has no wish list API, so this reads the same HTML a signed-out visitor gets. Lists
//! render in a "list" or a "grid" layout chosen by their owner, and arrive ten or so items at a
//! time: the first page is a full document, later pages are bare `<li>` fragments.

use std::collections::HashSet;
use std::time::Duration;

use scraper::{ElementRef, Html, Selector};
use url::Url;

use super::{extract, fetch, price, Importer};

/// About ten items each, so this caps an import at roughly 300 wishes.
const MAX_PAGES: usize = 30;
/// Pause between pages so a long list doesn't look like a scraper to Amazon.
const PAGE_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum ListImportError {
    #[error("That doesn't look like an Amazon wish list link. Open the list on Amazon, choose “Send list to others”, and paste the link here.")]
    NotAmazon,
    #[error("We couldn't open that list. On Amazon, open the list's “Manage list” settings, set its privacy to Public or Shared, and try again.")]
    NotAList,
    #[error("Amazon asked for a robot check instead of showing the list. Please try again in a little while.")]
    Blocked,
    #[error("{0}")]
    Fetch(fetch::FetchError),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListItem {
    pub title: String,
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub price_cents: Option<i64>,
    pub currency: String,
    pub notes: Option<String>,
    pub priority: i64,
    pub quantity: i64,
}

#[derive(Clone, Debug, Default)]
pub struct AmazonList {
    pub name: Option<String>,
    pub items: Vec<ListItem>,
    /// Items already fully bought on Amazon, which were left out.
    pub already_bought: usize,
    /// True when the list was only partly read (too long, or Amazon stopped answering).
    pub incomplete: bool,
}

/// Amazon's storefronts and share-link shorteners. Exact names, so `amazon.evil.com` doesn't pass.
fn is_amazon_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if matches!(host.as_str(), "a.co" | "amzn.to" | "amzn.eu" | "amzn.asia") {
        return true;
    }
    let host = host.strip_prefix("www.").unwrap_or(&host);
    host.strip_prefix("amazon.")
        .is_some_and(|tld| currency_for_tld(tld).is_some())
}

fn currency_for_tld(tld: &str) -> Option<&'static str> {
    Some(match tld {
        "com" => "USD",
        "ca" => "CAD",
        "com.mx" => "MXN",
        "com.br" => "BRL",
        "co.uk" => "GBP",
        "de" | "fr" | "it" | "es" | "nl" | "ie" | "com.be" => "EUR",
        "se" => "SEK",
        "pl" => "PLN",
        "com.au" => "AUD",
        "co.jp" => "JPY",
        "in" => "INR",
        "sg" => "SGD",
        "ae" => "AED",
        _ => return None,
    })
}

fn currency_for_host(url: &Url) -> &'static str {
    url.host_str()
        .map(|h| h.trim_start_matches("www."))
        .and_then(|h| h.strip_prefix("amazon."))
        .and_then(currency_for_tld)
        .unwrap_or("USD")
}

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

fn text_of(el: ElementRef) -> String {
    extract::clean_text(&el.text().collect::<String>())
}

/// The first `selector` match whose text is a number.
fn number_in(li: ElementRef, selector: &str) -> Option<i64> {
    li.select(&sel(selector))
        .find_map(|el| text_of(el).parse().ok())
}

fn asin_of(li: ElementRef) -> Option<String> {
    let params = li.value().attr("data-reposition-action-params")?;
    let params: serde_json::Value = serde_json::from_str(params).ok()?;
    let external = params.get("itemExternalId")?.as_str()?;
    let asin = external.strip_prefix("ASIN:")?.split('|').next()?;
    (!asin.is_empty() && asin.chars().all(|c| c.is_ascii_alphanumeric())).then(|| asin.to_string())
}

fn parse_item(li: ElementRef, base: &Url, currency: &str) -> Option<(ListItem, bool)> {
    let name_el = li.select(&sel(r#"[id^="itemName_"]"#)).next();
    let link_el = li.select(&sel("a[aria-label][href]")).next();
    let img_el = li.select(&sel("img[src]")).next();
    let title = name_el
        .and_then(|a| a.value().attr("title").map(str::to_string))
        .or_else(|| name_el.map(text_of))
        .or_else(|| link_el.and_then(|a| a.value().attr("aria-label").map(str::to_string)))
        .or_else(|| img_el.and_then(|i| i.value().attr("alt").map(str::to_string)))
        .map(|t| extract::truncate(&extract::clean_text(&t), 200))
        .filter(|t| !t.is_empty())?;

    // Rebuild product links without Amazon's list and tracking parameters.
    let url = asin_of(li)
        .and_then(|asin| base.join(&format!("/dp/{asin}")).ok())
        .map(|u| u.to_string())
        .or_else(|| {
            name_el
                .or(link_el)
                .and_then(|a| a.value().attr("href"))
                .and_then(|href| extract::absolutize(base, href))
        });

    let price_text = li
        .select(&sel(
            r#"[id^="itemPrice_"] .a-offscreen, .a-price .a-offscreen"#,
        ))
        .next()
        .map(text_of)
        // Unavailable items carry data-price="-Infinity".
        .or_else(|| {
            li.value()
                .attr("data-price")
                .filter(|p| p.parse::<f64>().is_ok_and(|f| f.is_finite() && f > 0.0))
                .map(str::to_string)
        });
    let parsed = price_text.and_then(|p| price::parse_price(&p, Some(currency)));

    let requested = number_in(li, r#"[id^="itemRequested_"]"#).unwrap_or(1);
    let purchased = number_in(li, r#"[id^="itemPurchased_"]"#).unwrap_or(0);
    let bought_out = requested > 0 && purchased >= requested;

    // Amazon's scale runs from -2 (lowest) to 2 (highest).
    let priority = match number_in(li, r#"[id^="itemPriority_"]"#).unwrap_or(0) {
        p if p > 0 => 1,
        0 => 2,
        _ => 3,
    };
    let notes = li
        .select(&sel(r#"[id^="itemComment_"]"#))
        .next()
        .map(text_of)
        .filter(|c| !c.is_empty())
        .map(|c| extract::truncate(&c, 2000));

    let item = ListItem {
        title,
        url,
        image_url: img_el
            .and_then(|i| i.value().attr("src"))
            .and_then(|src| extract::absolutize(base, src))
            .filter(|src| !src.contains("/grey-pixel") && !src.ends_with(".gif"))
            .map(|src| full_size_image(&src)),
        currency: parsed
            .as_ref()
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| currency.to_string()),
        price_cents: parsed.map(|(c, _)| c),
        notes,
        priority,
        quantity: (requested - purchased).clamp(1, 99),
    };
    Some((item, bought_out))
}

/// Lists use 135px thumbnails: `…/I/abc._SS135_.jpg` → `…/I/abc._SL500_.jpg`.
fn full_size_image(src: &str) -> String {
    let Some((stem, ext)) = src.rsplit_once('.') else {
        return src.to_string();
    };
    match stem.rsplit_once("._") {
        Some((base, size)) if size.ends_with('_') => format!("{base}._SL500_.{ext}"),
        _ => src.to_string(),
    }
}

struct Page {
    name: Option<String>,
    items: Vec<ListItem>,
    already_bought: usize,
    next: Option<Url>,
}

fn parse_page(html: &str, page_url: &Url, currency: &str) -> Page {
    let doc = Html::parse_document(html);
    let mut page = Page {
        name: doc
            .select(&sel("#profile-list-name"))
            .next()
            .map(text_of)
            .filter(|n| !n.is_empty()),
        items: Vec::new(),
        already_bought: 0,
        next: None,
    };
    for li in doc.select(&sel("li[data-itemid]")) {
        match parse_item(li, page_url, currency) {
            Some((_, true)) => page.already_bought += 1,
            Some((item, false)) => page.items.push(item),
            None => {}
        }
    }
    if doc.select(&sel("#endOfListMarker")).next().is_none() {
        page.next = doc
            .select(&sel("a.wl-see-more[href]"))
            .next()
            .and_then(|a| a.value().attr("href"))
            .and_then(|href| page_url.join(href).ok())
            // Pagination stays on the store we started from.
            .filter(|next| next.host_str() == page_url.host_str());
    }
    page
}

/// Whether the page is a list at all. Amazon answers 200 for private and deleted lists too.
fn classify_first_page(html: &str, page_url: &Url) -> Result<(), ListImportError> {
    if html.contains("id=\"g-items") {
        Ok(())
    } else if html.contains("not a functioning page") {
        Err(ListImportError::NotAList)
    } else if extract::is_bot_wall(html, page_url) && html.contains("validateCaptcha") {
        Err(ListImportError::Blocked)
    } else {
        Err(ListImportError::NotAList)
    }
}

/// The ASIN in an Amazon product link (`/dp/<asin>` or `/gp/product/<asin>`).
pub fn asin_from_url(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    let i = segments
        .iter()
        .position(|s| *s == "dp" || *s == "product")?;
    let asin = segments.get(i + 1)?;
    (asin.len() == 10 && asin.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| asin.to_ascii_uppercase())
}

impl Importer {
    /// Read every page of an Amazon wish list. A failure after the first page keeps what was
    /// already read and sets `incomplete`.
    pub async fn import_amazon_list(&self, raw_url: &str) -> Result<AmazonList, ListImportError> {
        let url = fetch::validate_url(raw_url, self.allow_private).map_err(|e| match e {
            fetch::FetchError::InvalidUrl => ListImportError::NotAmazon,
            other => ListImportError::Fetch(other),
        })?;
        let on_amazon = |u: &Url| self.allow_private || u.host_str().is_some_and(is_amazon_host);
        if !on_amazon(&url) {
            return Err(ListImportError::NotAmazon);
        }
        let (mut page_url, html) = match fetch::fetch(&self.http, &url).await {
            Ok(fetch::Fetched::Html { final_url, body }) => (final_url, body),
            Ok(fetch::Fetched::Image { .. }) => return Err(ListImportError::NotAList),
            // Amazon answers automated requests it dislikes with a 503.
            Err(fetch::FetchError::Status(503)) => return Err(ListImportError::Blocked),
            Err(fetch::FetchError::Status(404)) => return Err(ListImportError::NotAList),
            Err(e) => return Err(ListImportError::Fetch(e)),
        };
        // Short links can redirect anywhere.
        if !on_amazon(&page_url) {
            return Err(ListImportError::NotAmazon);
        }
        classify_first_page(&html, &page_url)?;

        let currency = currency_for_host(&page_url);
        let first = parse_page(&html, &page_url, currency);
        let mut list = AmazonList {
            name: first.name,
            items: first.items,
            already_bought: first.already_bought,
            incomplete: false,
        };
        let mut next = first.next;
        let mut seen = HashSet::from([page_url.to_string()]);
        let mut pages = 1;
        while let Some(url) = next.take() {
            if !seen.insert(url.to_string()) {
                break;
            }
            if pages >= MAX_PAGES {
                list.incomplete = true;
                break;
            }
            tokio::time::sleep(PAGE_DELAY).await;
            let body = match fetch::fetch(&self.http, &url).await {
                Ok(fetch::Fetched::Html { final_url, body }) => {
                    page_url = final_url;
                    body
                }
                Ok(fetch::Fetched::Image { .. }) | Err(_) => {
                    list.incomplete = true;
                    break;
                }
            };
            if body.contains("validateCaptcha") {
                list.incomplete = true;
                break;
            }
            let page = parse_page(&body, &page_url, currency);
            list.items.extend(page.items);
            list.already_bought += page.already_bought;
            next = page.next;
            pages += 1;
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from real pages; both layouts share `li[data-itemId]` but little else.
    const LIST_ITEM: &str = r#"<ul id="g-items"><li data-id="L1" data-itemId="I1" data-price="9.99"
      data-reposition-action-params="{&quot;itemExternalId&quot;:&quot;ASIN:B0CJXW2KQZ|ATVPDKIKX0DER&quot;}">
      <div id="itemImage_I1"><img alt="Bags" src="https://m.media-amazon.com/images/I/61z.AL._SS135_.jpg"></div>
      <a id="itemName_I1" title="Kraft Gift Bags &amp; Tags" href="/dp/B0CJXW2KQZ/?coliid=I1&amp;colid=L1&amp;ref_=x">Kraft Gift Bags</a>
      <span id="itemPrice_I1" class="a-price"><span class="a-offscreen">$9.99</span></span>
      <span id="itemComment_I1"> the red ones </span>
      <span id="itemPriorityLabel_I1">high</span><span id="itemPriority_I1" class="a-hidden">1</span>
      <span id="itemRequested_I1">3</span><span id="itemPurchased_I1">1</span></li></ul>"#;

    const GRID_ITEM: &str = r#"<ul id="g-items-grid"><li id="item_I2" data-id="L1" data-itemId="I2"
      data-csa-c-item-id="B07RPT4LFN"
      data-reposition-action-params="{&quot;itemExternalId&quot;:&quot;ASIN:B07RPT4LFN|ATVPDKIKX0DER&quot;}">
      <a aria-label="Digital Timer" class="a-link-normal" href="/dp/B07RPT4LFN/?coliid=I2"><img alt="Digital Timer" src="https://m.media-amazon.com/images/I/41x._SS135_.jpg"></a>
      <span id="itemComment_I2"></span><span id="itemPriority_I2">0</span>
      <span id="itemRequested_I2">1</span><span id="itemPurchased_I2">0</span>
      <span class="a-price a-text-price"><span class="a-offscreen">$6.69</span></span></li></ul>"#;

    fn base() -> Url {
        Url::parse("https://www.amazon.com/hz/wishlist/ls/L1").unwrap()
    }

    #[test]
    fn list_layout() {
        let page = parse_page(LIST_ITEM, &base(), "USD");
        assert_eq!(
            page.items,
            vec![ListItem {
                title: "Kraft Gift Bags & Tags".into(),
                url: Some("https://www.amazon.com/dp/B0CJXW2KQZ".into()),
                image_url: Some("https://m.media-amazon.com/images/I/61z.AL._SL500_.jpg".into()),
                price_cents: Some(999),
                currency: "USD".into(),
                notes: Some("the red ones".into()),
                priority: 1,
                quantity: 2,
            }]
        );
    }

    #[test]
    fn grid_layout() {
        let page = parse_page(GRID_ITEM, &base(), "USD");
        let item = &page.items[0];
        assert_eq!(item.title, "Digital Timer");
        assert_eq!(
            item.url.as_deref(),
            Some("https://www.amazon.com/dp/B07RPT4LFN")
        );
        assert_eq!(item.price_cents, Some(669));
        assert_eq!((item.priority, item.quantity), (2, 1));
        assert_eq!(item.notes, None);
    }

    #[test]
    fn unavailable_items_have_no_price() {
        let html = r#"<li data-itemId="I3" data-price="-Infinity"><a id="itemName_I3" title="Gone" href="/dp/B000000000"></a></li>"#;
        let page = parse_page(html, &base(), "USD");
        assert_eq!(page.items[0].title, "Gone");
        assert_eq!(page.items[0].price_cents, None);
    }

    #[test]
    fn fully_bought_items_are_left_out() {
        let html = r#"<li data-itemId="I4"><a id="itemName_I4" title="Done" href="/dp/B000000001"></a>
          <span id="itemRequested_I4">1</span><span id="itemPurchased_I4">1</span></li>"#;
        let page = parse_page(html, &base(), "USD");
        assert!(page.items.is_empty());
        assert_eq!(page.already_bought, 1);
    }

    #[test]
    fn pagination_stops_at_the_end_and_stays_on_the_store() {
        let more = r#"<a class="wl-see-more" href="/hz/wishlist/slv/items?paginationToken=x">See more</a>"#;
        assert_eq!(
            parse_page(more, &base(), "USD").next.unwrap().as_str(),
            "https://www.amazon.com/hz/wishlist/slv/items?paginationToken=x"
        );
        let ended = format!(r#"{more}<div id="endOfListMarker"></div>"#);
        assert!(parse_page(&ended, &base(), "USD").next.is_none());
        let offsite = r#"<a class="wl-see-more" href="https://evil.example/x">See more</a>"#;
        assert!(parse_page(offsite, &base(), "USD").next.is_none());
    }

    #[test]
    fn real_lists_are_not_mistaken_for_robot_checks() {
        // Lists are titled just "Amazon.com", which the single-product importer treats as a wall.
        let title = "<html><head><title>Amazon.com</title></head>";
        let list = format!(r#"{title}<ul id="g-items"></ul>"#);
        assert!(classify_first_page(&list, &base()).is_ok());
        let grid = format!(r#"{title}<ul id="g-items-grid"></ul>"#);
        assert!(classify_first_page(&grid, &base()).is_ok());
        let dead =
            format!("{title}The Web address you entered is not a functioning page on our site");
        assert!(matches!(
            classify_first_page(&dead, &base()),
            Err(ListImportError::NotAList)
        ));
        let wall = format!(r#"{title}<form action="/errors/validateCaptcha">"#);
        assert!(matches!(
            classify_first_page(&wall, &base()),
            Err(ListImportError::Blocked)
        ));
    }

    #[test]
    fn only_amazon_hosts_are_accepted() {
        for good in ["www.amazon.com", "amazon.co.uk", "www.amazon.de", "a.co"] {
            assert!(is_amazon_host(good), "{good}");
        }
        for bad in ["amazon.evil.com", "notamazon.com", "www.amazon.com.evil.io"] {
            assert!(!is_amazon_host(bad), "{bad}");
        }
    }

    #[test]
    fn asins_are_found_in_product_links() {
        for url in [
            "https://www.amazon.com/dp/B0CJXW2KQZ",
            "https://www.amazon.com/Some-Name/dp/B0CJXW2KQZ/ref=sr_1?x=y",
            "https://www.amazon.com/gp/product/B0CJXW2KQZ",
        ] {
            assert_eq!(asin_from_url(url).as_deref(), Some("B0CJXW2KQZ"), "{url}");
        }
        assert_eq!(
            asin_from_url("https://www.amazon.com/hz/wishlist/ls/X"),
            None
        );
    }
}
