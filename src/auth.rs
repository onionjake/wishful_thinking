//! Accounts, sessions, request context and flash messages.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{COOKIE, HOST, ORIGIN, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};

use crate::db::{Db, User};
use crate::error::{AppError, AppResult};
use crate::templates::{Ctx, Flash};
use crate::util;
use crate::AppState;

pub const SESSION_COOKIE: &str = "wt_session";
pub const FLASH_COOKIE: &str = "wt_flash";
pub const GUEST_COOKIE: &str = "wt_guest";
const SESSION_DAYS: i64 = 30;

pub async fn hash_password(password: &str) -> anyhow::Result<String> {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| anyhow::anyhow!("hash error: {e}"))
    })
    .await?
}

pub async fn verify_password(password: &str, hash: &str) -> bool {
    let password = password.to_string();
    let hash = hash.to_string();
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .map(|parsed| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok()
            })
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// No 0/o or 1/l/i, so a reset password can be read out or typed in by hand.
const RESET_PASSWORD_CHARS: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// Give an account a new random password and end all its sessions. Returns the password,
/// or `None` if no account has this email.
pub async fn reset_password(db: &Db, email: &str) -> anyhow::Result<Option<String>> {
    use rand::seq::SliceRandom;
    let email = email.trim().to_lowercase();
    let password: String = (0..12)
        .map(|_| *RESET_PASSWORD_CHARS.choose(&mut rand::rngs::OsRng).unwrap() as char)
        .collect();
    let hash = hash_password(&password).await?;
    let mut tx = db.begin().await?;
    let user_id: Option<i64> =
        sqlx::query_scalar("UPDATE users SET password_hash = ? WHERE email = ? RETURNING id")
            .bind(hash)
            .bind(&email)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(user_id) = user_id else {
        return Ok(None);
    };
    sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(password))
}

pub fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

pub fn cookie_header(name: &str, value: &str, max_age_secs: i64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"
    ))
    .expect("valid cookie")
}

/// Create a session and return the Set-Cookie header for it.
pub async fn start_session(state: &AppState, user_id: i64) -> AppResult<HeaderValue> {
    let token = util::random_token(32);
    let expires = time::OffsetDateTime::now_utc() + time::Duration::days(SESSION_DAYS);
    let expires = expires
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(anyhow::Error::from)?;
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(util::sha256_hex(&token))
        .bind(user_id)
        .bind(expires)
        .execute(&state.db)
        .await?;
    Ok(cookie_header(
        SESSION_COOKIE,
        &token,
        SESSION_DAYS * 86400,
        state.config.secure_cookies,
    ))
}

pub async fn end_session(state: &AppState, headers: &HeaderMap) -> AppResult<HeaderValue> {
    if let Some(token) = read_cookie(headers, SESSION_COOKIE) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(util::sha256_hex(&token))
            .execute(&state.db)
            .await?;
    }
    Ok(cookie_header(
        SESSION_COOKIE,
        "",
        0,
        state.config.secure_cookies,
    ))
}

async fn user_for_session(db: &Db, token: &str) -> sqlx::Result<Option<User>> {
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    sqlx::query_as::<_, User>(
        "SELECT u.id, u.email, u.display_name FROM sessions s JOIN users u ON u.id = s.user_id
          WHERE s.token_hash = ? AND s.expires_at > ?",
    )
    .bind(util::sha256_hex(token))
    .bind(now)
    .fetch_optional(db)
    .await
}

impl FromRequestParts<AppState> for Ctx {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = match read_cookie(&parts.headers, SESSION_COOKIE) {
            Some(token) => user_for_session(&state.db, &token).await?,
            None => None,
        };
        let flash = read_cookie(&parts.headers, FLASH_COOKIE).and_then(|raw| Flash::decode(&raw));
        let path = parts
            .uri
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".into());
        let base_url = state.config.base_url.clone().unwrap_or_else(|| {
            let host = parts
                .headers
                .get(HOST)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("localhost");
            let scheme = if state.config.secure_cookies {
                "https"
            } else {
                "http"
            };
            format!("{scheme}://{host}")
        });
        Ok(Ctx {
            user,
            flash,
            path,
            base_url,
            llm_label: state.importer.llm_label(),
        })
    }
}

impl Ctx {
    /// The signed-in user, or a redirect to the login page that returns here afterwards.
    pub fn require_user(&self) -> AppResult<&User> {
        self.user
            .as_ref()
            .ok_or_else(|| AppError::LoginRequired(self.path.clone()))
    }
}

/// Redirect and show a one-time message on the next page.
pub fn redirect_flash(to: &str, kind: &str, message: &str) -> Response {
    let value = Flash {
        kind: kind.to_string(),
        message: message.to_string(),
    }
    .encode();
    let mut resp = Redirect::to(to).into_response();
    resp.headers_mut()
        .append(SET_COOKIE, cookie_header(FLASH_COOKIE, &value, 60, false));
    resp
}

/// Clears the flash cookie once it has been shown (unless the handler set a new one), and
/// rejects cross-site form posts as a CSRF defence on top of SameSite cookies.
pub async fn middleware(State(_state): State<AppState>, req: Request, next: Next) -> Response {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        if let Some(origin) = req.headers().get(ORIGIN).and_then(|o| o.to_str().ok()) {
            let host = req
                .headers()
                .get(HOST)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("");
            let origin_host = origin.split("://").nth(1).unwrap_or("");
            if origin != "null" && origin_host != host {
                return (StatusCode::FORBIDDEN, "Cross-site request blocked").into_response();
            }
        }
    }
    let had_flash = read_cookie(req.headers(), FLASH_COOKIE).is_some();
    let is_page = req.method() == Method::GET;
    let mut resp = next.run(req).await;
    let sets_flash = resp
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .any(|v| v.to_str().is_ok_and(|v| v.starts_with(FLASH_COOKIE)));
    let is_redirect = resp.status().is_redirection();
    if had_flash && is_page && !sets_flash && !is_redirect {
        resp.headers_mut()
            .append(SET_COOKIE, cookie_header(FLASH_COOKIE, "", 0, false));
    }
    resp
}
