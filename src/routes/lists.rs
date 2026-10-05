use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Form;
use serde::Deserialize;

use crate::auth::{self, redirect_flash};
use crate::db::{self, Access, User, Wishlist};
use crate::error::{AppError, AppResult};
use crate::templates::{render, Ctx, ItemView, ListFormPage, ListPage, SharePage};
use crate::util;
use crate::AppState;

/// Load a list and check the current user may edit it.
pub async fn editable_list(
    state: &AppState,
    ctx: &Ctx,
    list_id: i64,
) -> AppResult<(Wishlist, Access, User)> {
    let user = ctx.require_user()?.clone();
    let list = db::get_list(&state.db, list_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let access = db::list_access(&state.db, &list, Some(user.id)).await?;
    if !access.can_edit() {
        return Err(if access.can_view() {
            AppError::Forbidden
        } else {
            AppError::NotFound
        });
    }
    Ok((list, access, user))
}

#[derive(Deserialize)]
pub struct ListForm {
    title: String,
    recipient_name: Option<String>,
    description: Option<String>,
    event_date: Option<String>,
    show_claims_to_owner: Option<String>,
    #[serde(default)]
    families: Vec<i64>,
}

struct CleanList {
    title: String,
    recipient_name: Option<String>,
    description: Option<String>,
    event_date: Option<String>,
    show_claims_to_owner: bool,
}

fn validate_list(f: &ListForm) -> Result<CleanList, String> {
    let title = f.title.trim().to_string();
    if title.is_empty() || title.chars().count() > 100 {
        return Err("Give the list a name (up to 100 characters).".into());
    }
    let event_date = util::non_empty(f.event_date.as_deref());
    if let Some(d) = &event_date {
        if util::parse_date(d).is_none() {
            return Err("The event date should look like 2026-12-25.".into());
        }
    }
    Ok(CleanList {
        title,
        recipient_name: util::non_empty(f.recipient_name.as_deref())
            .map(|s| s.chars().take(60).collect()),
        description: util::non_empty(f.description.as_deref())
            .map(|s| s.chars().take(2000).collect()),
        event_date,
        show_claims_to_owner: f.show_claims_to_owner.is_some(),
    })
}

pub async fn new_list_page(State(state): State<AppState>, ctx: Ctx) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let families = db::families_for_user(&state.db, user.id)
        .await?
        .into_iter()
        .map(|f| (f, true))
        .collect();
    render(ListFormPage {
        ctx,
        list: None,
        title: String::new(),
        recipient_name: String::new(),
        description: String::new(),
        event_date: String::new(),
        show_claims_to_owner: false,
        families,
        error: None,
    })
}

pub async fn create_list(
    State(state): State<AppState>,
    ctx: Ctx,
    axum_extra::extract::Form(f): axum_extra::extract::Form<ListForm>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let clean = match validate_list(&f) {
        Ok(c) => c,
        Err(msg) => {
            let families = db::families_for_user(&state.db, user.id)
                .await?
                .into_iter()
                .map(|fam| {
                    let on = f.families.contains(&fam.id);
                    (fam, on)
                })
                .collect();
            return render(ListFormPage {
                ctx,
                list: None,
                title: f.title.clone(),
                recipient_name: f.recipient_name.clone().unwrap_or_default(),
                description: f.description.clone().unwrap_or_default(),
                event_date: f.event_date.clone().unwrap_or_default(),
                show_claims_to_owner: f.show_claims_to_owner.is_some(),
                families,
                error: Some(msg),
            });
        }
    };
    let mut tx = state.db.begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO wishlists (owner_id, title, recipient_name, description, event_date, show_claims_to_owner)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user.id)
    .bind(&clean.title)
    .bind(&clean.recipient_name)
    .bind(&clean.description)
    .bind(&clean.event_date)
    .bind(clean.show_claims_to_owner)
    .fetch_one(&mut *tx)
    .await?;
    for fam in &f.families {
        // Only families the creator belongs to.
        sqlx::query(
            "INSERT OR IGNORE INTO wishlist_families (wishlist_id, family_id)
             SELECT ?, family_id FROM family_members WHERE family_id = ? AND user_id = ?",
        )
        .bind(id)
        .bind(fam)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(redirect_flash(
        &format!("/lists/{id}"),
        "ok",
        "List created. Add your first wish!",
    ))
}

pub async fn edit_list_page(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let (list, _, _) = editable_list(&state, &ctx, id).await?;
    render(ListFormPage {
        ctx,
        title: list.title.clone(),
        recipient_name: list.recipient_name.clone().unwrap_or_default(),
        description: list.description.clone().unwrap_or_default(),
        event_date: list.event_date.clone().unwrap_or_default(),
        show_claims_to_owner: list.show_claims_to_owner,
        list: Some(list),
        families: vec![],
        error: None,
    })
}

pub async fn update_list(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    axum_extra::extract::Form(f): axum_extra::extract::Form<ListForm>,
) -> AppResult<Response> {
    let (list, _, _) = editable_list(&state, &ctx, id).await?;
    let clean = match validate_list(&f) {
        Ok(c) => c,
        Err(msg) => {
            return render(ListFormPage {
                ctx,
                title: f.title.clone(),
                recipient_name: f.recipient_name.clone().unwrap_or_default(),
                description: f.description.clone().unwrap_or_default(),
                event_date: f.event_date.clone().unwrap_or_default(),
                show_claims_to_owner: f.show_claims_to_owner.is_some(),
                list: Some(list),
                families: vec![],
                error: Some(msg),
            })
        }
    };
    sqlx::query(
        "UPDATE wishlists SET title = ?, recipient_name = ?, description = ?, event_date = ?, show_claims_to_owner = ?,
                updated_at = datetime('now') WHERE id = ?",
    )
    .bind(&clean.title)
    .bind(&clean.recipient_name)
    .bind(&clean.description)
    .bind(&clean.event_date)
    .bind(clean.show_claims_to_owner)
    .bind(id)
    .execute(&state.db)
    .await?;
    Ok(redirect_flash(
        &format!("/lists/{id}"),
        "ok",
        "List updated.",
    ))
}

pub async fn delete_list(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let (list, access, _) = editable_list(&state, &ctx, id).await?;
    if access != Access::Owner {
        return Err(AppError::Forbidden);
    }
    sqlx::query("DELETE FROM wishlists WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(redirect_flash(
        "/",
        "ok",
        &format!("Deleted “{}”.", list.title),
    ))
}

pub async fn toggle_archive(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let (list, _, _) = editable_list(&state, &ctx, id).await?;
    sqlx::query("UPDATE wishlists SET archived = ? WHERE id = ?")
        .bind(!list.archived)
        .bind(id)
        .execute(&state.db)
        .await?;
    let msg = if list.archived {
        "List restored."
    } else {
        "List archived. Family members won't see it until you restore it."
    };
    Ok(redirect_flash(&format!("/lists/{id}"), "ok", msg))
}

// ---------------------------------------------------------------- viewing

#[derive(Deserialize, Default)]
pub struct ViewQuery {
    sort: Option<String>,
}

pub async fn view_list(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Query(q): Query<ViewQuery>,
) -> AppResult<Response> {
    let list = db::get_list(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let access = db::list_access(&state.db, &list, ctx.user.as_ref().map(|u| u.id)).await?;
    if !access.can_view() {
        return Err(if ctx.user.is_none() {
            AppError::LoginRequired(ctx.path.clone())
        } else {
            AppError::NotFound
        });
    }
    if list.archived && !access.can_edit() {
        return Err(AppError::NotFound);
    }
    let page = build_list_page(&state, ctx, list, access, false, None, q.sort).await?;
    render(page)
}

pub async fn public_list(
    State(state): State<AppState>,
    ctx: Ctx,
    headers: HeaderMap,
    Path(token): Path<String>,
    Query(q): Query<ViewQuery>,
) -> AppResult<Response> {
    let list: Wishlist = sqlx::query_as(&format!(
        "SELECT {} FROM wishlists WHERE public_token = ? AND archived = 0",
        db::WISHLIST_COLS
    ))
    .bind(&token)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    let access = db::list_access(&state.db, &list, ctx.user.as_ref().map(|u| u.id)).await?;
    let guest = auth::read_cookie(&headers, auth::GUEST_COOKIE);
    let page = build_list_page(&state, ctx, list, access, true, guest, q.sort).await?;
    render(page)
}

pub async fn build_list_page(
    state: &AppState,
    ctx: Ctx,
    list: Wishlist,
    access: Access,
    public_view: bool,
    guest_token: Option<String>,
    sort: Option<String>,
) -> AppResult<ListPage> {
    let me = ctx.user.as_ref().map(|u| u.id);
    let is_editor = access.can_edit();
    // Editors don't see reservations unless the list opts in (e.g. a parent running a child's list).
    let claims_visible = !is_editor || list.show_claims_to_owner;
    let claim_names_visible = claims_visible && (access != Access::None);
    let can_claim = claims_visible && (me.is_some() || public_view);

    let mut items = db::items_for_list(&state.db, list.id).await?;
    let claims = if claims_visible {
        db::claims_for_list(&state.db, list.id).await?
    } else {
        vec![]
    };
    if !claims_visible {
        for it in &mut items {
            it.claimed_qty = 0;
        }
    }
    let sort = sort.unwrap_or_else(|| "priority".into());
    match sort.as_str() {
        "price" => items.sort_by_key(|i| {
            (
                i.received,
                i.price_cents.is_none(),
                i.price_cents.unwrap_or(0),
            )
        }),
        "price_desc" => items.sort_by_key(|i| {
            (
                i.received,
                i.price_cents.is_none(),
                -i.price_cents.unwrap_or(0),
            )
        }),
        "newest" => items.sort_by_key(|i| (i.received, -i.id)),
        _ => {}
    }

    let mut active = Vec::new();
    let mut received = Vec::new();
    for item in items {
        let item_claims: Vec<_> = claims
            .iter()
            .filter(|c| c.item_id == item.id)
            .cloned()
            .collect();
        let my_claim = item_claims
            .iter()
            .find(|c| match (me, c.user_id, &guest_token, &c.guest_token) {
                (Some(me), Some(uid), _, _) => me == uid,
                (None, None, Some(g), Some(cg)) => g == cg,
                _ => false,
            })
            .cloned();
        let view = ItemView {
            item,
            claims: item_claims,
            my_claim,
        };
        if view.item.received {
            received.push(view);
        } else {
            active.push(view);
        }
    }

    let owner_name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = ?")
        .bind(list.owner_id)
        .fetch_one(&state.db)
        .await?;
    let family_names: Vec<String> = if is_editor {
        sqlx::query_scalar(
            "SELECT f.name FROM wishlist_families wf JOIN families f ON f.id = wf.family_id WHERE wf.wishlist_id = ? ORDER BY f.name",
        )
        .bind(list.id)
        .fetch_all(&state.db)
        .await?
    } else {
        vec![]
    };
    let public_url = list
        .public_token
        .as_ref()
        .map(|t| state.absolute(&ctx.base_url, &format!("/p/{t}")));
    let guest_name = String::new();
    Ok(ListPage {
        owner_name,
        is_editor,
        is_owner: access == Access::Owner,
        public_view,
        claims_visible,
        claim_names_visible,
        can_claim,
        items: active,
        received,
        family_names,
        public_url,
        guest_name,
        sort,
        list,
        ctx,
    })
}

// ---------------------------------------------------------------- sharing

pub async fn share_page(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let (list, access, user) = editable_list(&state, &ctx, id).await?;
    let shared: Vec<i64> =
        sqlx::query_scalar("SELECT family_id FROM wishlist_families WHERE wishlist_id = ?")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    let families = db::families_for_user(&state.db, user.id)
        .await?
        .into_iter()
        .map(|f| {
            let on = shared.contains(&f.id);
            (f, on)
        })
        .collect();
    let managers: Vec<User> = sqlx::query_as(
        "SELECT u.id, u.email, u.display_name FROM wishlist_managers m JOIN users u ON u.id = m.user_id
          WHERE m.wishlist_id = ? ORDER BY u.display_name",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    // Co-manager candidates: anyone in a family with the owner.
    let candidates: Vec<User> = sqlx::query_as(
        "SELECT DISTINCT u.id, u.email, u.display_name FROM family_members a
           JOIN family_members b ON b.family_id = a.family_id
           JOIN users u ON u.id = b.user_id
          WHERE a.user_id = ?1 AND b.user_id != ?1
            AND u.id NOT IN (SELECT user_id FROM wishlist_managers WHERE wishlist_id = ?2)
          ORDER BY u.display_name",
    )
    .bind(list.owner_id)
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let public_url = list
        .public_token
        .as_ref()
        .map(|t| state.absolute(&ctx.base_url, &format!("/p/{t}")));
    render(SharePage {
        ctx,
        list,
        is_owner: access == Access::Owner,
        families,
        public_url,
        managers,
        candidates,
    })
}

#[derive(Deserialize)]
pub struct FamiliesForm {
    #[serde(default)]
    families: Vec<i64>,
}

pub async fn update_families(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    axum_extra::extract::Form(f): axum_extra::extract::Form<FamiliesForm>,
) -> AppResult<Response> {
    let (_, _, user) = editable_list(&state, &ctx, id).await?;
    let mut tx = state.db.begin().await?;
    // Only touch families this editor belongs to; others (shared by a co-manager) stay as they are.
    sqlx::query(
        "DELETE FROM wishlist_families WHERE wishlist_id = ?
            AND family_id IN (SELECT family_id FROM family_members WHERE user_id = ?)",
    )
    .bind(id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    for fam in &f.families {
        sqlx::query(
            "INSERT OR IGNORE INTO wishlist_families (wishlist_id, family_id)
             SELECT ?, family_id FROM family_members WHERE family_id = ? AND user_id = ?",
        )
        .bind(id)
        .bind(fam)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(redirect_flash(
        &format!("/lists/{id}/share"),
        "ok",
        "Family sharing updated.",
    ))
}

#[derive(Deserialize)]
pub struct PublicForm {
    action: String,
}

pub async fn update_public_link(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(f): Form<PublicForm>,
) -> AppResult<Response> {
    editable_list(&state, &ctx, id).await?;
    let (token, msg) = match f.action.as_str() {
        "enable" | "regenerate" => (
            Some(util::random_token(18)),
            if f.action == "enable" {
                "Public link created. Anyone with the link can view this list."
            } else {
                "New link created. The old link no longer works."
            },
        ),
        "disable" => (None, "Public link turned off."),
        _ => return Err(AppError::BadRequest("Unknown action".into())),
    };
    sqlx::query("UPDATE wishlists SET public_token = ? WHERE id = ?")
        .bind(token)
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(redirect_flash(&format!("/lists/{id}/share"), "ok", msg))
}

#[derive(Deserialize)]
pub struct ManagerForm {
    user_id: i64,
}

pub async fn add_manager(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(f): Form<ManagerForm>,
) -> AppResult<Response> {
    let (list, access, _) = editable_list(&state, &ctx, id).await?;
    if access != Access::Owner {
        return Err(AppError::Forbidden);
    }
    // Must share a family with the owner.
    let res = sqlx::query(
        "INSERT OR IGNORE INTO wishlist_managers (wishlist_id, user_id)
         SELECT ?1, b.user_id FROM family_members a JOIN family_members b ON b.family_id = a.family_id
          WHERE a.user_id = ?2 AND b.user_id = ?3 AND b.user_id != ?2 LIMIT 1",
    )
    .bind(id)
    .bind(list.owner_id)
    .bind(f.user_id)
    .execute(&state.db)
    .await?;
    if res.rows_affected() == 0 {
        return Ok(redirect_flash(
            &format!("/lists/{id}/share"),
            "error",
            "Co-managers must be in one of your families.",
        ));
    }
    Ok(redirect_flash(
        &format!("/lists/{id}/share"),
        "ok",
        "Co-manager added. They can now add and edit items.",
    ))
}

pub async fn remove_manager(
    State(state): State<AppState>,
    ctx: Ctx,
    Path((id, uid)): Path<(i64, i64)>,
) -> AppResult<Response> {
    let (_, access, user) = editable_list(&state, &ctx, id).await?;
    // Owners can remove anyone; a manager can remove themselves.
    if access != Access::Owner && user.id != uid {
        return Err(AppError::Forbidden);
    }
    sqlx::query("DELETE FROM wishlist_managers WHERE wishlist_id = ? AND user_id = ?")
        .bind(id)
        .bind(uid)
        .execute(&state.db)
        .await?;
    if user.id == uid && access != Access::Owner {
        return Ok(redirect_flash(
            "/",
            "ok",
            "You're no longer a co-manager of that list.",
        ));
    }
    Ok(redirect_flash(
        &format!("/lists/{id}/share"),
        "ok",
        "Co-manager removed.",
    ))
}
