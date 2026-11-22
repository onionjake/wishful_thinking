//! Admin pages: every account at a glance, and acting on someone's behalf.
//!
//! An admin is a user with `users.is_admin = 1`, which is only ever set by hand in the database.
//! Rather than a second copy of every page, an admin *acts as* a user: their session is marked
//! (`sessions.acting_as`) and every ordinary page and action then works as that user would see
//! it, so adding, editing and removing wishes, sharing, invites and so on all come for free. The
//! acting admin sees exactly what the user sees, so surprises on their lists stay hidden.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;

use crate::auth::{self, redirect_flash};
use crate::db::{self, FamilySummary};
use crate::error::{AppError, AppResult};
use crate::templates::{
    render, AdminInvite, AdminList, AdminReservation, AdminUser, AdminUserPage, AdminUsersPage,
    Ctx, Flash,
};
use crate::util;
use crate::AppState;

#[derive(Deserialize, Default)]
pub struct SearchQuery {
    q: Option<String>,
}

pub async fn users_page(
    State(state): State<AppState>,
    ctx: Ctx,
    Query(query): Query<SearchQuery>,
) -> AppResult<Response> {
    ctx.require_admin()?;
    let q = query.q.unwrap_or_default().trim().to_string();
    let users = sqlx::query_as::<_, AdminUser>(&format!(
        "{USER_SELECT}
          WHERE ?1 = '' OR instr(lower(u.email), lower(?1)) > 0
             OR instr(lower(u.display_name), lower(?1)) > 0
          ORDER BY lower(u.display_name), u.id
          LIMIT 500"
    ))
    .bind(&q)
    .bind(now())
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.db)
        .await?;
    render(AdminUsersPage {
        ctx,
        users,
        q,
        total,
    })
    .map(no_store)
}

/// Sessions' expiry times are RFC 3339.
fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Admin pages hold other people's details (and, once, a new password): keep them out of caches.
fn no_store(mut resp: Response) -> Response {
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    resp
}

const USER_SELECT: &str = "SELECT u.id, u.email, u.display_name, u.is_admin, u.created_at,
    (SELECT COUNT(*) FROM wishlists w WHERE w.owner_id = u.id) AS list_count,
    (SELECT COUNT(*) FROM items i JOIN wishlists w ON w.id = i.wishlist_id
      WHERE w.owner_id = u.id AND w.archived = 0 AND i.received = 0) AS wish_count,
    (SELECT COUNT(*) FROM family_members fm WHERE fm.user_id = u.id) AS family_count,
    (SELECT COUNT(*) FROM sessions s WHERE s.user_id = u.id AND s.expires_at > ?2) AS session_count,
    (SELECT MAX(s.created_at) FROM sessions s WHERE s.user_id = u.id AND s.expires_at > ?2) AS last_sign_in
  FROM users u";

pub async fn user_page(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let admin_id = ctx.require_admin()?.id;
    let user = sqlx::query_as::<_, AdminUser>(&format!("{USER_SELECT} WHERE u.id = ?1"))
        .bind(id)
        .bind(now())
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    // Deliberately nothing about who reserved what on this person's lists: an admin is often
    // family too, and the user's own pages hide it from them.
    let mut lists = sqlx::query_as::<_, AdminList>(
        "SELECT w.id, w.title, w.recipient_name, w.event_date, w.archived, w.public_token,
                w.show_claims_to_owner, w.owner_id, u.display_name AS owner_name, w.created_at,
                (SELECT COUNT(*) FROM items i WHERE i.wishlist_id = w.id AND i.received = 0) AS open_count,
                (SELECT COUNT(*) FROM items i WHERE i.wishlist_id = w.id AND i.received = 1) AS received_count,
                (SELECT group_concat(f.name, ', ') FROM wishlist_families wf
                   JOIN families f ON f.id = wf.family_id WHERE wf.wishlist_id = w.id) AS family_names,
                (SELECT group_concat(mu.display_name || CASE WHEN m.sees_claims THEN ' (sees reservations)' ELSE '' END, ', ')
                   FROM wishlist_managers m JOIN users mu ON mu.id = m.user_id
                  WHERE m.wishlist_id = w.id) AS manager_names
           FROM wishlists w JOIN users u ON u.id = w.owner_id
          WHERE w.owner_id = ?1 OR w.id IN (SELECT wishlist_id FROM wishlist_managers WHERE user_id = ?1)
          ORDER BY w.archived, w.owner_id != ?1, COALESCE(w.event_date, '9999'), w.title",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    for list in &mut lists {
        list.public_url = list
            .public_token
            .as_ref()
            .map(|t| state.absolute(&ctx.base_url, &format!("/p/{t}")));
    }
    let families: Vec<FamilySummary> = db::families_for_user(&state.db, id).await?;
    let mut invites = sqlx::query_as::<_, AdminInvite>(
        "SELECT i.id, i.family_id, f.name AS family_name, i.token, i.created_at, i.expires_at,
                i.uses, i.max_uses, i.revoked,
                (i.revoked = 0 AND (i.expires_at IS NULL OR i.expires_at > datetime('now'))
                  AND (i.max_uses IS NULL OR i.uses < i.max_uses)) AS active
           FROM family_invites i JOIN families f ON f.id = i.family_id
          WHERE i.created_by = ? ORDER BY active DESC, i.created_at DESC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    for inv in &mut invites {
        inv.url = state.absolute(&ctx.base_url, &format!("/join/{}", inv.token));
    }
    let reservations = sqlx::query_as::<_, AdminReservation>(
        "SELECT c.id, i.title AS item_title, w.id AS list_id, w.title AS list_title,
                COALESCE(w.recipient_name, u.display_name) AS recipient, c.quantity, c.purchased,
                c.reveal_on, c.created_at
           FROM claims c JOIN items i ON i.id = c.item_id JOIN wishlists w ON w.id = i.wishlist_id
           JOIN users u ON u.id = w.owner_id
          WHERE c.user_id = ? ORDER BY c.purchased, COALESCE(w.event_date, '9999'), w.title, i.title",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    // Their reservations on lists the admin manages without seeing reservations would spoil the
    // admin's own surprises.
    let mut visible = Vec::with_capacity(reservations.len());
    for r in reservations {
        if db::claim_is_secret_from(&state.db, r.list_id, r.reveal_on.as_deref(), admin_id).await? {
            continue;
        }
        visible.push(r);
    }
    let reservations = visible;
    let hidden_gifts = db::gifts_secret_from(&state.db, id, admin_id).await?.len() as i64;
    let gift_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM gift_history WHERE buyer_id = ?")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    render(AdminUserPage {
        ctx,
        user,
        lists,
        families,
        invites,
        reservations,
        gift_count: gift_count - hidden_gifts,
    })
    .map(no_store)
}

#[derive(Deserialize)]
pub struct ActForm {
    next: Option<String>,
}

/// Start acting as this user, then go to `next` (their dashboard by default).
pub async fn act_as(
    State(state): State<AppState>,
    ctx: Ctx,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<ActForm>,
) -> AppResult<Response> {
    let admin = ctx.require_admin()?.clone();
    let name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    auth::set_acting_as(&state, &headers, admin.id, Some(id)).await?;
    tracing::info!(
        admin_id = admin.id,
        acting_as = id,
        "admin started acting as a user"
    );
    let next = util::safe_next(f.next.as_deref());
    let message = if id == admin.id {
        "You're acting as yourself again.".to_string()
    } else {
        format!("You're now acting as {name}. Everything you do is done as them.")
    };
    Ok(redirect_flash(&next, "ok", &message))
}

/// Go back to being yourself, on the admin page of the person you were acting as.
pub async fn stop_acting(
    State(state): State<AppState>,
    ctx: Ctx,
    headers: HeaderMap,
) -> AppResult<Response> {
    let admin = ctx.require_admin()?.clone();
    let was = ctx.is_acting().then(|| ctx.user.clone()).flatten();
    auth::set_acting_as(&state, &headers, admin.id, None).await?;
    Ok(match was {
        Some(u) => {
            tracing::info!(
                admin_id = admin.id,
                acting_as = u.id,
                "admin stopped acting as a user"
            );
            redirect_flash(
                &format!("/admin/users/{}", u.id),
                "ok",
                &format!("You're no longer acting as {}.", u.display_name),
            )
        }
        None => redirect_flash("/admin", "ok", "You're acting as yourself."),
    })
}

/// Give the account a new random password (shown once, on the page) and sign it out everywhere.
pub async fn reset_password(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let admin = ctx.require_admin()?.clone();
    if id == admin.id {
        return Err(AppError::BadRequest(
            "Change your own password from your account settings.".into(),
        ));
    }
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let password = auth::reset_password(&state.db, &email)
        .await?
        .ok_or(AppError::NotFound)?;
    tracing::info!(
        admin_id = admin.id,
        user_id = id,
        "admin reset a user's password"
    );
    // Redirect, so reloading the page can't reset it again. The password rides in the one-time
    // flash cookie, which is cleared as soon as the page has shown it.
    let flash = Flash {
        kind: "ok".into(),
        message: format!("New password for {email}: {password} Pass it on now, as it won't be shown again. They've been signed out everywhere and can change it on their account page."),
    };
    let mut resp = Redirect::to(&format!("/admin/users/{id}")).into_response();
    resp.headers_mut().append(
        header::SET_COOKIE,
        auth::cookie_header(
            auth::FLASH_COOKIE,
            &flash.encode(),
            60,
            state.config.secure_cookies,
        ),
    );
    Ok(no_store(resp))
}

/// End every session the account has (other admins acting as them are unaffected).
pub async fn sign_out_everywhere(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let admin = ctx.require_admin()?.clone();
    if id == admin.id {
        return Err(AppError::BadRequest(
            "That would sign you out too. Sign out from the account menu instead.".into(),
        ));
    }
    let ended = sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(id)
        .execute(&state.db)
        .await?
        .rows_affected();
    tracing::info!(
        admin_id = admin.id,
        user_id = id,
        ended,
        "admin signed a user out everywhere"
    );
    Ok(redirect_flash(
        &format!("/admin/users/{id}"),
        "ok",
        &format!(
            "Ended {ended} session{}.",
            if ended == 1 { "" } else { "s" }
        ),
    ))
}
