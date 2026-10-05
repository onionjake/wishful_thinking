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
}
