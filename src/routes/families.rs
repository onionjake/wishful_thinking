use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;

use crate::auth::redirect_flash;
use crate::db::{self, FamilySummary};
use crate::error::{AppError, AppResult};
use crate::templates::{render, Ctx, FamiliesPage, FamilyPage, InviteView, JoinPage};
use crate::util;
use crate::AppState;

pub async fn families_page(State(state): State<AppState>, ctx: Ctx) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let families = db::families_for_user(&state.db, user.id).await?;
    render(FamiliesPage {
        ctx,
        families,
        error: None,
    })
}

#[derive(Deserialize)]
pub struct FamilyForm {
    name: String,
    description: Option<String>,
}

pub async fn create_family(
    State(state): State<AppState>,
    ctx: Ctx,
    Form(f): Form<FamilyForm>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let name = f.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        let families = db::families_for_user(&state.db, user.id).await?;
        return render(FamiliesPage {
            ctx,
            families,
            error: Some("Give the family a name (up to 80 characters).".into()),
        });
    }
    let mut tx = state.db.begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO families (name, description, created_by) VALUES (?, ?, ?) RETURNING id",
    )
    .bind(name)
    .bind(util::non_empty(f.description.as_deref()))
    .bind(user.id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO family_members (family_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(id)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(redirect_flash(
        &format!("/families/{id}"),
        "ok",
        "Family created. Make an invite link to bring everyone in.",
    ))
}

/// The family summary for a member, or 404 for non-members.
async fn member_family(state: &AppState, family_id: i64, user_id: i64) -> AppResult<FamilySummary> {
    db::families_for_user(&state.db, user_id)
        .await?
        .into_iter()
        .find(|f| f.id == family_id)
        .ok_or(AppError::NotFound)
}

#[derive(sqlx::FromRow)]
struct InviteRow {
    id: i64,
    token: String,
    created_by: i64,
    created_by_name: String,
    expires_at: Option<String>,
    uses: i64,
    max_uses: Option<i64>,
}

pub async fn family_page(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let family = member_family(&state, id, user.id).await?;
    let members = db::family_members(&state.db, id).await?;
    let lists = db::lists_in_family(&state.db, id).await?;
    let invites: Vec<InviteRow> = sqlx::query_as(
        "SELECT i.id, i.token, i.created_by, u.display_name AS created_by_name, i.expires_at, i.uses, i.max_uses
           FROM family_invites i JOIN users u ON u.id = i.created_by
          WHERE i.family_id = ? AND i.revoked = 0
            AND (i.expires_at IS NULL OR i.expires_at > datetime('now'))
            AND (i.max_uses IS NULL OR i.uses < i.max_uses)
          ORDER BY i.id DESC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let invites = invites
        .into_iter()
        .map(|i| InviteView {
            id: i.id,
            url: state.absolute(&ctx.base_url, &format!("/join/{}", i.token)),
            created_by_name: i.created_by_name,
            created_by: i.created_by,
            expires_at: i
                .expires_at
                .map(|e| util::pretty_date(&e[..10.min(e.len())])),
            uses: i.uses,
            max_uses: i.max_uses,
        })
        .collect();
    let is_owner = family.role == "owner";
    render(FamilyPage {
        ctx,
        family,
        members,
        lists,
        invites,
        is_owner,
        me: user.id,
    })
}

pub async fn rename_family(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(f): Form<FamilyForm>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let family = member_family(&state, id, user.id).await?;
    if family.role != "owner" {
        return Err(AppError::Forbidden);
    }
    let name = f.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Ok(redirect_flash(
            &format!("/families/{id}"),
            "error",
            "Family names need 1–80 characters.",
        ));
    }
    sqlx::query("UPDATE families SET name = ?, description = ? WHERE id = ?")
        .bind(name)
        .bind(util::non_empty(f.description.as_deref()))
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(redirect_flash(
        &format!("/families/{id}"),
        "ok",
        "Family updated.",
    ))
}

#[derive(Deserialize)]
pub struct InviteForm {
    expires_days: Option<i64>,
    max_uses: Option<i64>,
}

pub async fn create_invite(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(f): Form<InviteForm>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    member_family(&state, id, user.id).await?;
    let expires = f.expires_days.filter(|d| *d > 0).map(|d| d.min(365));
    let max_uses = f.max_uses.filter(|m| *m > 0);
    sqlx::query(
        "INSERT INTO family_invites (family_id, token, created_by, expires_at, max_uses)
         VALUES (?, ?, ?, CASE WHEN ? IS NULL THEN NULL ELSE datetime('now', '+' || ? || ' days') END, ?)",
    )
    .bind(id)
    .bind(util::random_token(18))
    .bind(user.id)
    .bind(expires)
    .bind(expires)
    .bind(max_uses)
    .execute(&state.db)
    .await?;
    Ok(redirect_flash(
        &format!("/families/{id}#invites"),
        "ok",
        "Invite link ready — copy it and send it to your family.",
    ))
}

pub async fn revoke_invite(
    State(state): State<AppState>,
    ctx: Ctx,
    Path((id, iid)): Path<(i64, i64)>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let family = member_family(&state, id, user.id).await?;
    let res = sqlx::query("UPDATE family_invites SET revoked = 1 WHERE id = ? AND family_id = ? AND (created_by = ? OR ?)")
        .bind(iid)
        .bind(id)
        .bind(user.id)
        .bind(family.role == "owner")
        .execute(&state.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Forbidden);
    }
    Ok(redirect_flash(
        &format!("/families/{id}#invites"),
        "ok",
        "Invite link revoked.",
    ))
}

/// Remove a user from a family, unsharing their lists from it and handing over ownership if needed.
async fn remove_from_family(state: &AppState, family_id: i64, user_id: i64) -> AppResult<bool> {
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM family_members WHERE family_id = ? AND user_id = ?")
        .bind(family_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM wishlist_families WHERE family_id = ? AND wishlist_id IN (SELECT id FROM wishlists WHERE owner_id = ?)")
        .bind(family_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM family_members WHERE family_id = ?")
            .bind(family_id)
            .fetch_one(&mut *tx)
            .await?;
    let deleted = if remaining == 0 {
        sqlx::query("DELETE FROM families WHERE id = ?")
            .bind(family_id)
            .execute(&mut *tx)
            .await?;
        true
    } else {
        // Promote the longest-standing member if no owner is left.
        sqlx::query(
            "UPDATE family_members SET role = 'owner'
              WHERE family_id = ?1 AND NOT EXISTS (SELECT 1 FROM family_members WHERE family_id = ?1 AND role = 'owner')
                AND user_id = (SELECT user_id FROM family_members WHERE family_id = ?1 ORDER BY joined_at, user_id LIMIT 1)",
        )
        .bind(family_id)
        .execute(&mut *tx)
        .await?;
        false
    };
    tx.commit().await?;
    Ok(deleted)
}

pub async fn remove_member(
    State(state): State<AppState>,
    ctx: Ctx,
    Path((id, uid)): Path<(i64, i64)>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let family = member_family(&state, id, user.id).await?;
    if family.role != "owner" || uid == user.id {
        return Err(AppError::Forbidden);
    }
    remove_from_family(&state, id, uid).await?;
    Ok(redirect_flash(
        &format!("/families/{id}"),
        "ok",
        "Member removed.",
    ))
}

pub async fn leave_family(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let family = member_family(&state, id, user.id).await?;
    remove_from_family(&state, id, user.id).await?;
    Ok(redirect_flash(
        "/families",
        "ok",
        &format!("You left {}.", family.name),
    ))
}

pub async fn delete_family(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let family = member_family(&state, id, user.id).await?;
    if family.role != "owner" {
        return Err(AppError::Forbidden);
    }
    sqlx::query("DELETE FROM families WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(redirect_flash(
        "/families",
        "ok",
        &format!(
            "Deleted {}. Lists are kept, just no longer shared there.",
            family.name
        ),
    ))
}

// ---------------------------------------------------------------- joining

#[derive(sqlx::FromRow)]
struct InviteInfo {
    id: i64,
    family_id: i64,
    family_name: String,
    inviter_name: String,
}

async fn valid_invite(state: &AppState, token: &str) -> AppResult<Option<InviteInfo>> {
    Ok(sqlx::query_as(
        "SELECT i.id, i.family_id, f.name AS family_name, u.display_name AS inviter_name
           FROM family_invites i JOIN families f ON f.id = i.family_id JOIN users u ON u.id = i.created_by
          WHERE i.token = ? AND i.revoked = 0
            AND (i.expires_at IS NULL OR i.expires_at > datetime('now'))
            AND (i.max_uses IS NULL OR i.uses < i.max_uses)",
    )
    .bind(token)
    .fetch_optional(&state.db)
    .await?)
}

pub async fn join_page(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(token): Path<String>,
) -> AppResult<Response> {
    let Some(invite) = valid_invite(&state, &token).await? else {
        return Err(AppError::BadRequest(
            "This invite link has expired or was turned off. Ask whoever sent it for a new one."
                .into(),
        ));
    };
    let member_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM family_members WHERE family_id = ?")
            .bind(invite.family_id)
            .fetch_one(&state.db)
            .await?;
    let already_member = match &ctx.user {
        Some(u) => db::family_role(&state.db, invite.family_id, u.id)
            .await?
            .is_some(),
        None => false,
    };
    render(JoinPage {
        ctx,
        token,
        family_name: invite.family_name,
        inviter_name: invite.inviter_name,
        member_count,
        already_member,
        family_id: invite.family_id,
    })
}

pub async fn join(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(token): Path<String>,
) -> AppResult<Response> {
    let Some(user) = ctx.user.clone() else {
        return Ok(Redirect::to(&format!(
            "/signup?next={}",
            util::urlencode(&format!("/join/{token}"))
        ))
        .into_response());
    };
    let Some(invite) = valid_invite(&state, &token).await? else {
        return Err(AppError::BadRequest(
            "This invite link has expired or was turned off.".into(),
        ));
    };
    let mut tx = state.db.begin().await?;
    let res = sqlx::query(
        "INSERT OR IGNORE INTO family_members (family_id, user_id, role) VALUES (?, ?, 'member')",
    )
    .bind(invite.family_id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() > 0 {
        sqlx::query("UPDATE family_invites SET uses = uses + 1 WHERE id = ?")
            .bind(invite.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(redirect_flash(
        &format!("/families/{}", invite.family_id),
        "ok",
        &format!(
            "Welcome to {}! Lists shared with the family now show up on your home page.",
            invite.family_name
        ),
    ))
}
