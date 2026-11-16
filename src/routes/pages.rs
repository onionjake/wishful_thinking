use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};

use crate::error::{AppError, AppResult};
use crate::templates::{render, Ctx, DashboardPage, HomePage};
use crate::AppState;
use crate::{db, images, storage};

pub async fn home(State(state): State<AppState>, ctx: Ctx) -> AppResult<Response> {
    let Some(user) = ctx.user.clone() else {
        return render(HomePage { ctx });
    };
    let my_lists = db::lists_managed_by(&state.db, user.id).await?;
    let shared_lists = db::lists_shared_with(&state.db, user.id).await?;
    let families = db::families_for_user(&state.db, user.id).await?;
    let reservation_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM claims WHERE user_id = ? AND purchased = 0")
            .bind(user.id)
            .fetch_one(&state.db)
            .await?;
    render(DashboardPage {
        ctx,
        my_lists,
        shared_lists,
        families,
        reservation_count,
    })
}

const CACHE: &str = "public, max-age=3600";

pub async fn stylesheet() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, CACHE),
        ],
        include_str!("../../static/style.css"),
    )
}

pub async fn script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, CACHE),
        ],
        include_str!("../../static/app.js"),
    )
}

pub async fn favicon() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, CACHE),
        ],
        include_str!("../../static/favicon.svg"),
    )
}

pub async fn manifest() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/manifest+json"),
            (header::CACHE_CONTROL, CACHE),
        ],
        include_str!("../../static/manifest.webmanifest"),
    )
}

/// Served from the site root so its scope covers the whole app. `no-cache` lets
/// browsers pick up changes to it promptly.
pub async fn service_worker() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../../static/sw.js"),
    )
}

fn png(bytes: &'static [u8]) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, CACHE),
        ],
        bytes,
    )
}

pub async fn apple_touch_icon() -> impl IntoResponse {
    png(include_bytes!("../../static/apple-touch-icon.png"))
}

pub async fn icon_192() -> impl IntoResponse {
    png(include_bytes!("../../static/icon-192.png"))
}

pub async fn icon_512() -> impl IntoResponse {
    png(include_bytes!("../../static/icon-512.png"))
}

pub async fn icon_maskable_512() -> impl IntoResponse {
    png(include_bytes!("../../static/icon-maskable-512.png"))
}

/// A stored picture at one of the page sizes (`/images/<key>/thumb`, `/images/<key>/card`).
///
/// Keys are content hashes, so a key's bytes never change: cache it forever. Pictures aren't
/// secret (they're product photos, and public lists show them to anyone), so this needs no
/// sign-in; the key is unguessable unless you already have the picture.
pub async fn stored_image(
    State(state): State<AppState>,
    axum::extract::Path((key, variant)): axum::extract::Path<(String, String)>,
) -> AppResult<Response> {
    let variant = images::Variant::from_name(&variant).ok_or(AppError::NotFound)?;
    if !storage::is_valid_key(&key) {
        return Err(AppError::NotFound);
    }
    let (stream, content_type) = images::variant(&state, &key, variant)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_string(),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox".to_string(),
            ),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}
