mod account;
mod families;
mod items;
mod lists;
mod pages;

use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use tower_http::trace::TraceLayer;

use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(pages::home))
        .route("/static/style.css", get(pages::stylesheet))
        .route("/static/app.js", get(pages::script))
        .route("/favicon.svg", get(pages::favicon))
        .route("/healthz", get(|| async { "ok" }))
        // accounts
        .route("/signup", get(account::signup_page).post(account::signup))
        .route("/login", get(account::login_page).post(account::login))
        .route("/logout", post(account::logout))
        .route(
            "/account",
            get(account::account_page).post(account::update_account),
        )
        .route("/account/password", post(account::change_password))
        // lists
        .route("/lists/new", get(lists::new_list_page))
        .route("/lists", post(lists::create_list))
        .route("/lists/{id}", get(lists::view_list))
        .route(
            "/lists/{id}/edit",
            get(lists::edit_list_page).post(lists::update_list),
        )
        .route("/lists/{id}/delete", post(lists::delete_list))
        .route("/lists/{id}/archive", post(lists::toggle_archive))
        .route(
            "/lists/{id}/share",
            get(lists::share_page).post(lists::update_families),
        )
        .route("/lists/{id}/public", post(lists::update_public_link))
        .route("/lists/{id}/managers", post(lists::add_manager))
        .route(
            "/lists/{id}/managers/{uid}/remove",
            post(lists::remove_manager),
        )
        .route("/p/{token}", get(lists::public_list))
        // items
        .route("/lists/{id}/items/new", get(items::new_item_page))
        .route("/lists/{id}/items", post(items::create_item))
        .route(
            "/items/{id}/edit",
            get(items::edit_item_page).post(items::update_item),
        )
        .route("/items/{id}/delete", post(items::delete_item))
        .route("/items/{id}/received", post(items::toggle_received))
        .route("/items/{id}/copy", post(items::copy_item))
        .route("/items/{id}/claim", post(items::claim_item))
        .route("/claims/{id}/delete", post(items::unclaim))
        .route("/claims/{id}/purchased", post(items::toggle_purchased))
        .route("/p/{token}/items/{id}/claim", post(items::public_claim))
        .route("/p/{token}/claims/{id}/delete", post(items::public_unclaim))
        .route("/reservations", get(items::reservations))
        .route("/add", get(items::add_page))
        .route("/api/import", post(items::import_api))
        // families
        .route(
            "/families",
            get(families::families_page).post(families::create_family),
        )
        .route("/families/{id}", get(families::family_page))
        .route("/families/{id}/edit", post(families::rename_family))
        .route("/families/{id}/invites", post(families::create_invite))
        .route(
            "/families/{id}/invites/{iid}/revoke",
            post(families::revoke_invite),
        )
        .route(
            "/families/{id}/members/{uid}/remove",
            post(families::remove_member),
        )
        .route("/families/{id}/leave", post(families::leave_family))
        .route("/families/{id}/delete", post(families::delete_family))
        .route(
            "/join/{token}",
            get(families::join_page).post(families::join),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
