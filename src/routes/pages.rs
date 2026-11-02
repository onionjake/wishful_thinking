use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};

use crate::db;
use crate::error::AppResult;
use crate::templates::{render, Ctx, DashboardPage, HomePage};
use crate::AppState;

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
