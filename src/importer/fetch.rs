//! Fetching remote pages safely: scheme/host validation, SSRF protection,
//! redirect limits, timeouts and a response-size cap.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header;
use url::Url;

const MAX_BYTES: usize = 4 * 1024 * 1024;
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0 Safari/537.36 WishfulThinking/0.1";

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error(
        "That doesn't look like a web address. Paste a link starting with http:// or https://"
    )]
    InvalidUrl,
    #[error("Links to private or local network addresses can't be imported")]
    Forbidden,
    #[error("The store took too long to respond")]
    Timeout,
    #[error("The store responded with HTTP {0}. Some shops block automated requests — you can still fill in the details by hand")]
    Status(u16),
    #[error("The page is too large to import")]
    TooLarge,
    #[error("Couldn't reach that site ({0})")]
    Network(String),
}

pub enum Fetched {
    Html {
        final_url: Url,
        body: String,
    },
    /// The link points directly at an image.
    Image {
        final_url: Url,
    },
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // carrier-grade NAT 100.64/10
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80) // link local
        }
    }
}

/// Validate a user-supplied URL before any network activity.
pub fn validate_url(raw: &str, allow_private: bool) -> Result<Url, FetchError> {
    let mut raw = raw.trim().to_string();
    if !raw.contains("://") {
        raw = format!("https://{raw}");
    }
    let url = Url::parse(&raw).map_err(|_| FetchError::InvalidUrl)?;
    check_url(&url, allow_private)?;
    Ok(url)
}

fn check_url(url: &Url, allow_private: bool) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(FetchError::InvalidUrl);
    }
    let host = url.host().ok_or(FetchError::InvalidUrl)?;
    if allow_private {
        return Ok(());
    }
    match host {
        url::Host::Ipv4(ip) if !is_public_ip(IpAddr::V4(ip)) => Err(FetchError::Forbidden),
        url::Host::Ipv6(ip) if !is_public_ip(IpAddr::V6(ip)) => Err(FetchError::Forbidden),
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            if d == "localhost"
                || d.ends_with(".localhost")
                || d.ends_with(".local")
                || d.ends_with(".internal")
                || !d.contains('.')
            {
                Err(FetchError::Forbidden)
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

/// DNS resolver that refuses to hand out private addresses, so a public hostname
/// can't be pointed at internal services (including after redirects).
struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<SocketAddr> =
                addrs.into_iter().filter(|a| is_public_ip(a.ip())).collect();
            if public.is_empty() {
                return Err(format!("{host} does not resolve to a public address").into());
            }
            Ok(Box::new(public.into_iter()) as Addrs)
        })
    }
}

pub fn build_client(allow_private: bool) -> reqwest::Client {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/*;q=0.8,*/*;q=0.5",
        ),
    );
    headers.insert(
        header::ACCEPT_LANGUAGE,
        header::HeaderValue::from_static("en-US,en;q=0.9"),
    );
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 6 {
                attempt.error("too many redirects")
            } else if check_url(attempt.url(), allow_private).is_err() {
                attempt.error("redirect to a forbidden address")
            } else {
                attempt.follow()
            }
        }));
    if !allow_private {
        builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
    }
    builder.build().expect("http client")
}

pub async fn fetch(client: &reqwest::Client, url: &Url) -> Result<Fetched, FetchError> {
    let mut resp = client.get(url.clone()).send().await.map_err(map_err)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::Status(status.as_u16()));
    }
    let final_url = resp.url().clone();
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if content_type.starts_with("image/") {
        return Ok(Fetched::Image { final_url });
    }
    if resp
        .content_length()
        .is_some_and(|l| l as usize > MAX_BYTES)
    {
        return Err(FetchError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(map_err)? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BYTES {
            // Product details live in <head> and near the top; a truncated page is still useful.
            body.truncate(MAX_BYTES);
            break;
        }
    }
    Ok(Fetched::Html {
        final_url,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn map_err(e: reqwest::Error) -> FetchError {
    if e.is_timeout() {
        FetchError::Timeout
    } else if e.is_redirect() {
        FetchError::Forbidden
    } else {
        let msg = e.to_string();
        if msg.contains("public address") {
            FetchError::Forbidden
        } else {
            FetchError::Network(short_error(&e))
        }
    }
}

fn short_error(e: &reqwest::Error) -> String {
    use std::error::Error;
    let mut src: &dyn Error = e;
    while let Some(next) = src.source() {
        src = next;
    }
    src.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_hosts() {
        for bad in [
            "http://127.0.0.1/",
            "http://localhost:8080/",
            "http://10.1.2.3/x",
            "http://192.168.0.1",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data",
            "file:///etc/passwd",
            "http://user:pw@example.com/",
            "http://intranet/",
        ] {
            assert!(
                validate_url(bad, false).is_err(),
                "{bad} should be rejected"
            );
        }
        assert!(validate_url("example.com/product", false).is_ok());
        assert!(validate_url("https://www.target.com/p/123", false).is_ok());
        assert!(validate_url("http://127.0.0.1:9000/", true).is_ok());
    }
}
