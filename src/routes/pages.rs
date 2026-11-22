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
    let open_claims: Vec<(i64, Option<String>)> = sqlx::query_as(
        "SELECT i.wishlist_id, c.reveal_on FROM claims c JOIN items i ON i.id = c.item_id
          WHERE c.user_id = ? AND c.purchased = 0",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let mut reservation_count = 0;
    for (list_id, reveal_on) in open_claims {
        // An admin acting as someone doesn't count their own surprises.
        if let Some(admin) = ctx.acting_admin() {
            if db::claim_is_secret_from(&state.db, list_id, reveal_on.as_deref(), admin.id).await? {
                continue;
            }
        }
        reservation_count += 1;
    }
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
    let (stream, content_type, exact) = images::variant(&state, &key, variant)
        .await?
        .ok_or(AppError::NotFound)?;
    // A stand-in for a missing size is only cached briefly, so the real one shows once remade.
    let cache = if exact {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=300"
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache.to_string()),
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
