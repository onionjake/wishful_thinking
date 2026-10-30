use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// A URL-safe random token with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

pub fn sha256_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Only allow local, absolute-path redirects (prevents open redirects via `?next=`).
pub fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.starts_with("/\\") => {
            n.to_string()
        }
        _ => "/".to_string(),
    }
}

/// Trim a form field; empty becomes `None`.
pub fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}

pub fn parse_date(s: &str) -> Option<time::Date> {
    time::Date::parse(
        s.trim(),
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .ok()
}

/// "today", "tomorrow", "in 12 days", "3 days ago".
pub fn countdown(date: &str) -> Option<String> {
    let d = parse_date(date)?;
    let days = (d - today()).whole_days();
    Some(match days {
        0 => "today".into(),
        1 => "tomorrow".into(),
        -1 => "yesterday".into(),
        n if n > 1 => format!("in {n} days"),
        n => format!("{} days ago", -n),
    })
}

/// "December 25, 2026".
pub fn pretty_date(date: &str) -> String {
    match parse_date(date) {
        Some(d) => format!("{} {}, {}", d.month(), d.day(), d.year()),
        None => date.to_string(),
    }
}

/// The first day of the month a month picker value ("2027-01") names.
pub fn parse_month(s: &str) -> Option<time::Date> {
    let (y, m) = s.trim().split_once('-')?;
    let month = time::Month::try_from(m.parse::<u8>().ok()?).ok()?;
    time::Date::from_calendar_date(y.parse().ok()?, month, 1).ok()
}

/// The first day of the month after `d`'s.
pub fn next_month(d: time::Date) -> time::Date {
    let year = if d.month() == time::Month::December {
        d.year() + 1
    } else {
        d.year()
    };
    time::Date::from_calendar_date(year, d.month().next(), 1).unwrap_or(d)
}

pub fn this_month() -> time::Date {
    today().replace_day(1).unwrap_or_else(|_| today())
}

/// A reveal month a gift-giver may pick: this month (shows right away) up to five years out.
pub fn reveal_month(s: &str) -> Option<time::Date> {
    parse_month(s).filter(|d| *d >= this_month() && d.year() <= today().year() + 5)
}

/// Whether a reservation's reveal date (`YYYY-MM-DD`) has arrived. No date means never.
pub fn is_revealed(reveal_on: Option<&str>) -> bool {
    reveal_on.and_then(parse_date).is_some_and(|d| d <= today())
}

/// "2027-01", the value of a month picker option.
pub fn month_value(d: time::Date) -> String {
    format!("{:04}-{:02}", d.year(), u8::from(d.month()))
}

/// "January 2027".
pub fn pretty_month(d: time::Date) -> String {
    format!("{} {}", d.month(), d.year())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_is_local_only() {
        assert_eq!(safe_next(Some("/lists/3")), "/lists/3");
        assert_eq!(safe_next(Some("//evil.com")), "/");
        assert_eq!(safe_next(Some("https://evil.com")), "/");
        assert_eq!(safe_next(None), "/");
    }

    #[test]
    fn tokens_are_long_and_unique() {
        let a = random_token(24);
        assert_eq!(a.len(), 32);
        assert_ne!(a, random_token(24));
    }

    #[test]
    fn months() {
        let jan = parse_month("2027-01").unwrap();
        assert_eq!(jan.to_string(), "2027-01-01");
        assert_eq!(month_value(jan), "2027-01");
        assert_eq!(pretty_month(jan), "January 2027");
        assert_eq!(next_month(parse_date("2026-12-25").unwrap()), jan);
        assert_eq!(parse_month("2027-13"), None);
        assert_eq!(parse_month("soon"), None);
        assert!(reveal_month(&month_value(this_month())).is_some());
        assert!(reveal_month("2001-01").is_none());
    }
}
