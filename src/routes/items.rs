use axum::extract::{Path, Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use serde::{Deserialize, Serialize};

use crate::auth::{self, redirect_flash};
use crate::db::{self, Access, Item, Wishlist};
use crate::error::{AppError, AppResult};
use crate::importer::{price, ImportOutcome};
use crate::routes::lists::editable_list;
use crate::templates::{
    render, AddPage, Ctx, ItemFormPage, ItemFormValues, ReservationView, ReservationsPage,
};
use crate::util;
use crate::AppState;

async fn other_lists(state: &AppState, user_id: i64, except: i64) -> AppResult<Vec<(i64, String)>> {
    Ok(db::lists_managed_by(&state.db, user_id)
        .await?
        .into_iter()
        .filter(|l| l.id != except && !l.archived)
        .map(|l| (l.id, l.title))
        .collect())
}

fn values_from_import(imp: &ImportOutcome) -> ItemFormValues {
    let currency = imp.info.currency.clone().unwrap_or_else(|| "USD".into());
    ItemFormValues {
        url: imp.url.clone(),
        title: imp.info.title.clone().unwrap_or_default(),
        price: imp
            .info
            .price_cents
            .map(|c| price::to_input(c, &currency))
            .unwrap_or_default(),
        currency,
        image_url: imp.info.image_url.clone().unwrap_or_default(),
        store: imp
            .info
            .store
            .clone()
            .or_else(|| imp.info.brand.clone())
            .unwrap_or_default(),
        notes: String::new(),
        priority: 2,
        quantity: 1,
        import_source: imp.source_label().to_string(),
    }
}

fn values_from_item(item: &Item) -> ItemFormValues {
    ItemFormValues {
        url: item.url.clone().unwrap_or_default(),
        title: item.title.clone(),
        price: item.price_input(),
        currency: item.currency.clone(),
        image_url: item.image_url.clone().unwrap_or_default(),
        store: item.store.clone().unwrap_or_default(),
        notes: item.notes.clone().unwrap_or_default(),
        priority: item.priority,
        quantity: item.quantity,
        import_source: item.import_source.clone().unwrap_or_default(),
    }
}

#[derive(Deserialize, Default)]
pub struct NewItemQuery {
    url: Option<String>,
}

pub async fn new_item_page(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(list_id): Path<i64>,
    Query(q): Query<NewItemQuery>,
) -> AppResult<Response> {
    let (list, _, user) = editable_list(&state, &ctx, list_id).await?;
    let mut form = ItemFormValues {
        priority: 2,
        quantity: 1,
        currency: "USD".into(),
        ..Default::default()
    };
    let mut import = None;
    let mut import_error = None;
    if let Some(url) = util::non_empty(q.url.as_deref()) {
        match state.importer.import(&url).await {
            Ok(outcome) => {
                form = values_from_import(&outcome);
                import = Some(outcome);
            }
            Err(e) => {
                form.url = url;
                import_error = Some(e.to_string());
            }
        }
    }
    let other_lists = other_lists(&state, user.id, list.id).await?;
    render(ItemFormPage {
        ctx,
        list,
        item_id: None,
        form,
        import,
        import_error,
        error: None,
        other_lists,
    })
}

#[derive(Deserialize)]
pub struct ItemForm {
    url: Option<String>,
    title: String,
    price: Option<String>,
    currency: Option<String>,
    image_url: Option<String>,
    store: Option<String>,
    notes: Option<String>,
    priority: Option<i64>,
    quantity: Option<i64>,
    import_source: Option<String>,
}

struct CleanItem {
    url: Option<String>,
    title: String,
    price_cents: Option<i64>,
    currency: String,
    image_url: Option<String>,
    store: Option<String>,
    notes: Option<String>,
    priority: i64,
    quantity: i64,
    import_source: Option<String>,
}

fn http_url(raw: Option<&str>) -> Result<Option<String>, ()> {
    match util::non_empty(raw) {
        None => Ok(None),
        Some(u) => {
            let candidate = if u.contains("://") {
                u
            } else {
                format!("https://{u}")
            };
            match url::Url::parse(&candidate) {
                Ok(parsed)
                    if matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some() =>
                {
                    Ok(Some(parsed.to_string()))
                }
                _ => Err(()),
            }
        }
    }
}

impl ItemForm {
    fn values(&self) -> ItemFormValues {
        ItemFormValues {
            url: self.url.clone().unwrap_or_default(),
            title: self.title.clone(),
            price: self.price.clone().unwrap_or_default(),
            currency: self.currency.clone().unwrap_or_else(|| "USD".into()),
            image_url: self.image_url.clone().unwrap_or_default(),
            store: self.store.clone().unwrap_or_default(),
            notes: self.notes.clone().unwrap_or_default(),
            priority: self.priority.unwrap_or(2),
            quantity: self.quantity.unwrap_or(1),
            import_source: self.import_source.clone().unwrap_or_default(),
        }
    }

    fn validate(&self) -> Result<CleanItem, String> {
        let title = self.title.trim().to_string();
        if title.is_empty() || title.chars().count() > 200 {
            return Err("Every wish needs a name (up to 200 characters).".into());
        }
        let url = http_url(self.url.as_deref()).map_err(|_| {
            "The link should be a web address starting with http:// or https://".to_string()
        })?;
        let image_url = http_url(self.image_url.as_deref())
            .map_err(|_| "The picture should be a web address.".to_string())?;
        let currency = self
            .currency
            .as_deref()
            .and_then(price::normalize_currency)
            .unwrap_or_else(|| "USD".into());
        let price_cents = match util::non_empty(self.price.as_deref()) {
            None => None,
            Some(p) => Some(
                price::parse_amount(&p, &currency)
                    .filter(|c| *c >= 0 && *c < 10_000_000_000)
                    .ok_or_else(|| "The price should be a number like 24.99.".to_string())?,
            ),
        };
        Ok(CleanItem {
            url,
            title,
            price_cents,
            currency,
            image_url,
            store: util::non_empty(self.store.as_deref()).map(|s| s.chars().take(80).collect()),
            notes: util::non_empty(self.notes.as_deref()).map(|s| s.chars().take(2000).collect()),
            priority: self.priority.unwrap_or(2).clamp(1, 3),
            quantity: self.quantity.unwrap_or(1).clamp(1, 99),
            import_source: util::non_empty(self.import_source.as_deref())
                .map(|s| s.chars().take(40).collect()),
        })
    }
}

pub async fn create_item(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(list_id): Path<i64>,
    Form(f): Form<ItemForm>,
) -> AppResult<Response> {
    let (list, _, user) = editable_list(&state, &ctx, list_id).await?;
    let clean = match f.validate() {
        Ok(c) => c,
        Err(msg) => {
            let other_lists = other_lists(&state, user.id, list.id).await?;
            return render(ItemFormPage {
                ctx,
                list,
                item_id: None,
                form: f.values(),
                import: None,
                import_error: None,
                error: Some(msg),
                other_lists,
            });
        }
    };
    sqlx::query(
        "INSERT INTO items (wishlist_id, title, url, image_url, price_cents, currency, store, notes, priority, quantity, import_source)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(list_id)
    .bind(&clean.title)
    .bind(&clean.url)
    .bind(&clean.image_url)
    .bind(clean.price_cents)
    .bind(&clean.currency)
    .bind(&clean.store)
    .bind(&clean.notes)
    .bind(clean.priority)
    .bind(clean.quantity)
    .bind(&clean.import_source)
    .execute(&state.db)
    .await?;
    sqlx::query("UPDATE wishlists SET updated_at = datetime('now') WHERE id = ?")
        .bind(list_id)
        .execute(&state.db)
        .await?;
    Ok(redirect_flash(
        &format!("/lists/{list_id}"),
        "ok",
        &format!("Added “{}”.", clean.title),
    ))
}

/// Load an item and its list, requiring edit rights.
async fn editable_item(
    state: &AppState,
    ctx: &Ctx,
    item_id: i64,
) -> AppResult<(Item, Wishlist, i64)> {
    let item = db::get_item(&state.db, item_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let (list, _, user) = editable_list(state, ctx, item.wishlist_id).await?;
    Ok((item, list, user.id))
}

pub async fn edit_item_page(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let (item, list, uid) = editable_item(&state, &ctx, id).await?;
    let other_lists = other_lists(&state, uid, list.id).await?;
    render(ItemFormPage {
        ctx,
        list,
        item_id: Some(item.id),
        form: values_from_item(&item),
        import: None,
        import_error: None,
        error: None,
        other_lists,
    })
}

pub async fn update_item(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(f): Form<ItemForm>,
) -> AppResult<Response> {
    let (item, list, uid) = editable_item(&state, &ctx, id).await?;
    let clean = match f.validate() {
        Ok(c) => c,
        Err(msg) => {
            let other_lists = other_lists(&state, uid, list.id).await?;
            return render(ItemFormPage {
                ctx,
                list,
                item_id: Some(item.id),
                form: f.values(),
                import: None,
                import_error: None,
                error: Some(msg),
                other_lists,
            });
        }
    };
    sqlx::query(
        "UPDATE items SET title = ?, url = ?, image_url = ?, price_cents = ?, currency = ?, store = ?, notes = ?,
                priority = ?, quantity = ?, import_source = COALESCE(?, import_source), updated_at = datetime('now')
          WHERE id = ?",
    )
    .bind(&clean.title)
    .bind(&clean.url)
    .bind(&clean.image_url)
    .bind(clean.price_cents)
    .bind(&clean.currency)
    .bind(&clean.store)
    .bind(&clean.notes)
    .bind(clean.priority)
    .bind(clean.quantity)
    .bind(&clean.import_source)
    .bind(id)
    .execute(&state.db)
    .await?;
    Ok(redirect_flash(
        &format!("/lists/{}#item-{id}", list.id),
        "ok",
        "Saved.",
    ))
}

pub async fn delete_item(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let (item, list, _) = editable_item(&state, &ctx, id).await?;
    sqlx::query("DELETE FROM items WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(redirect_flash(
        &format!("/lists/{}", list.id),
        "ok",
        &format!("Removed “{}”.", item.title),
    ))
}

pub async fn toggle_received(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let (item, list, _) = editable_item(&state, &ctx, id).await?;
    sqlx::query("UPDATE items SET received = ? WHERE id = ?")
        .bind(!item.received)
        .bind(id)
        .execute(&state.db)
        .await?;
    let msg = if item.received {
        "Moved back to the wish list."
    } else {
        "Marked as received 🎉"
    };
    Ok(redirect_flash(&format!("/lists/{}", list.id), "ok", msg))
}

#[derive(Deserialize)]
pub struct CopyForm {
    target_list: i64,
    mode: String,
}

pub async fn copy_item(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(f): Form<CopyForm>,
) -> AppResult<Response> {
    let (item, list, _) = editable_item(&state, &ctx, id).await?;
    let (target, _, _) = editable_list(&state, &ctx, f.target_list).await?;
    if f.mode == "move" {
        sqlx::query("UPDATE items SET wishlist_id = ? WHERE id = ?")
            .bind(target.id)
            .bind(id)
            .execute(&state.db)
            .await?;
        // Reservations were made against the old list's audience; clear them on move.
        sqlx::query("DELETE FROM claims WHERE item_id = ?")
            .bind(id)
            .execute(&state.db)
            .await?;
        return Ok(redirect_flash(
            &format!("/lists/{}", list.id),
            "ok",
            &format!("Moved to “{}”.", target.title),
        ));
    }
    sqlx::query(
        "INSERT INTO items (wishlist_id, title, url, image_url, price_cents, currency, store, notes, priority, quantity, import_source)
         SELECT ?, title, url, image_url, price_cents, currency, store, notes, priority, quantity, import_source FROM items WHERE id = ?",
    )
    .bind(target.id)
    .bind(item.id)
    .execute(&state.db)
    .await?;
    Ok(redirect_flash(
        &format!("/lists/{}", list.id),
        "ok",
        &format!("Copied to “{}”.", target.title),
    ))
}

// ---------------------------------------------------------------- reservations

#[derive(Deserialize)]
pub struct ClaimForm {
    quantity: Option<i64>,
    guest_name: Option<String>,
}

enum Claimer {
    User(i64),
    Guest { name: String, token: String },
}

/// Insert a reservation if the item still has unreserved quantity.
async fn insert_claim(
    state: &AppState,
    item_id: i64,
    claimer: &Claimer,
    qty: Option<i64>,
) -> AppResult<Result<(), &'static str>> {
    let mut tx = state.db.begin().await?;
    let row: Option<(i64, i64, bool)> = sqlx::query_as(
        "SELECT i.quantity, COALESCE((SELECT SUM(quantity) FROM claims WHERE item_id = i.id), 0), i.received FROM items i WHERE i.id = ?",
    )
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((quantity, claimed, received)) = row else {
        return Err(AppError::NotFound);
    };
    if received {
        return Ok(Err("That gift has already been received."));
    }
    let remaining = quantity - claimed;
    if remaining <= 0 {
        return Ok(Err("Someone already reserved that one."));
    }
    let qty = qty.unwrap_or(1).clamp(1, remaining);
    let existing: Option<i64> = match claimer {
        Claimer::User(uid) => {
            sqlx::query_scalar("SELECT id FROM claims WHERE item_id = ? AND user_id = ?")
                .bind(item_id)
                .bind(uid)
                .fetch_optional(&mut *tx)
                .await?
        }
        Claimer::Guest { token, .. } => {
            sqlx::query_scalar("SELECT id FROM claims WHERE item_id = ? AND guest_token = ?")
                .bind(item_id)
                .bind(token)
                .fetch_optional(&mut *tx)
                .await?
        }
    };
    if let Some(claim_id) = existing {
        sqlx::query("UPDATE claims SET quantity = quantity + ? WHERE id = ?")
            .bind(qty)
            .bind(claim_id)
            .execute(&mut *tx)
            .await?;
    } else {
        match claimer {
            Claimer::User(uid) => {
                sqlx::query("INSERT INTO claims (item_id, user_id, quantity) VALUES (?, ?, ?)")
                    .bind(item_id)
                    .bind(uid)
                    .bind(qty)
                    .execute(&mut *tx)
                    .await?;
            }
            Claimer::Guest { name, token } => {
                sqlx::query("INSERT INTO claims (item_id, guest_name, guest_token, quantity) VALUES (?, ?, ?, ?)")
                    .bind(item_id)
                    .bind(name)
                    .bind(token)
                    .bind(qty)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(Ok(()))
}

/// Whether this user may reserve items on this list (and see reservations).
fn may_claim(list: &Wishlist, access: Access) -> bool {
    match access {
        Access::Family => true,
        Access::Owner | Access::Manager => list.show_claims_to_owner,
        Access::None => false,
    }
}

pub async fn claim_item(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(f): Form<ClaimForm>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let item = db::get_item(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let list = db::get_list(&state.db, item.wishlist_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let access = db::list_access(&state.db, &list, Some(user.id)).await?;
    if !may_claim(&list, access) {
        return Err(if access.can_view() {
            AppError::Forbidden
        } else {
            AppError::NotFound
        });
    }
    let back = format!("/lists/{}#item-{id}", list.id);
    Ok(
        match insert_claim(&state, id, &Claimer::User(user.id), f.quantity).await? {
            Ok(()) => redirect_flash(
                &back,
                "ok",
                &format!(
                    "You're getting “{}”. It's on your shopping list.",
                    item.title
                ),
            ),
            Err(msg) => redirect_flash(&back, "error", msg),
        },
    )
}

async fn public_list_by_token(state: &AppState, token: &str) -> AppResult<Wishlist> {
    sqlx::query_as::<_, Wishlist>(&format!(
        "SELECT {} FROM wishlists WHERE public_token = ? AND archived = 0",
        db::WISHLIST_COLS
    ))
    .bind(token)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)
}

pub async fn public_claim(
    State(state): State<AppState>,
    ctx: Ctx,
    headers: HeaderMap,
    Path((token, id)): Path<(String, i64)>,
    Form(f): Form<ClaimForm>,
) -> AppResult<Response> {
    let list = public_list_by_token(&state, &token).await?;
    let item = db::get_item(&state.db, id)
        .await?
        .filter(|i| i.wishlist_id == list.id)
        .ok_or(AppError::NotFound)?;
    let back = format!("/p/{token}#item-{id}");
    let mut set_cookie = None;
    let claimer = match &ctx.user {
        Some(user) => {
            let access = db::list_access(&state.db, &list, Some(user.id)).await?;
            if access.can_edit() && !list.show_claims_to_owner {
                return Ok(redirect_flash(
                    &back,
                    "error",
                    "You manage this list, so reservations are hidden from you.",
                ));
            }
            Claimer::User(user.id)
        }
        None => {
            let Some(name) = util::non_empty(f.guest_name.as_deref())
                .map(|n| n.chars().take(60).collect::<String>())
            else {
                return Ok(redirect_flash(
                    &back,
                    "error",
                    "Please enter your name so others know this gift is taken.",
                ));
            };
            let guest_token = auth::read_cookie(&headers, auth::GUEST_COOKIE)
                .filter(|t| t.len() >= 20 && t.len() <= 64);
            let guest_token = match guest_token {
                Some(t) => t,
                None => {
                    let t = util::random_token(24);
                    set_cookie = Some(auth::cookie_header(
                        auth::GUEST_COOKIE,
                        &t,
                        365 * 86400,
                        state.config.secure_cookies,
                    ));
                    t
                }
            };
            Claimer::Guest {
                name,
                token: guest_token,
            }
        }
    };
    let mut resp = match insert_claim(&state, item.id, &claimer, f.quantity).await? {
        Ok(()) => redirect_flash(
            &back,
            "ok",
            &format!("Thanks! “{}” is reserved for you.", item.title),
        ),
        Err(msg) => redirect_flash(&back, "error", msg),
    };
    if let Some(c) = set_cookie {
        resp.headers_mut().append(SET_COOKIE, c);
    }
    Ok(resp)
}

#[derive(sqlx::FromRow)]
struct ClaimOwner {
    user_id: Option<i64>,
    guest_token: Option<String>,
    wishlist_id: i64,
    purchased: bool,
}

async fn claim_owner(state: &AppState, claim_id: i64) -> AppResult<ClaimOwner> {
    sqlx::query_as::<_, ClaimOwner>(
        "SELECT c.user_id, c.guest_token, i.wishlist_id, c.purchased FROM claims c JOIN items i ON i.id = c.item_id WHERE c.id = ?",
    )
    .bind(claim_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)
}

#[derive(Deserialize, Default)]
pub struct BackForm {
    back: Option<String>,
}

pub async fn unclaim(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(f): Form<BackForm>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let owner = claim_owner(&state, id).await?;
    if owner.user_id != Some(user.id) {
        return Err(AppError::Forbidden);
    }
    sqlx::query("DELETE FROM claims WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    let back = f
        .back
        .map(|b| util::safe_next(Some(&b)))
        .unwrap_or_else(|| format!("/lists/{}", owner.wishlist_id));
    Ok(redirect_flash(
        &back,
        "ok",
        "Reservation released — someone else can get it now.",
    ))
}

pub async fn public_unclaim(
    State(state): State<AppState>,
    ctx: Ctx,
    headers: HeaderMap,
    Path((token, id)): Path<(String, i64)>,
) -> AppResult<Response> {
    let list = public_list_by_token(&state, &token).await?;
    let owner = claim_owner(&state, id).await?;
    let guest = auth::read_cookie(&headers, auth::GUEST_COOKIE);
    let mine = owner.wishlist_id == list.id
        && match (&ctx.user, owner.user_id) {
            (Some(u), Some(uid)) => u.id == uid,
            (None, None) => guest.is_some() && guest == owner.guest_token,
            _ => false,
        };
    if !mine {
        return Err(AppError::Forbidden);
    }
    sqlx::query("DELETE FROM claims WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(redirect_flash(
        &format!("/p/{token}"),
        "ok",
        "Reservation released.",
    ))
}

pub async fn toggle_purchased(
    State(state): State<AppState>,
    ctx: Ctx,
    Path(id): Path<i64>,
    Form(f): Form<BackForm>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let owner = claim_owner(&state, id).await?;
    if owner.user_id != Some(user.id) {
        return Err(AppError::Forbidden);
    }
    sqlx::query("UPDATE claims SET purchased = ? WHERE id = ?")
        .bind(!owner.purchased)
        .bind(id)
        .execute(&state.db)
        .await?;
    let back = f
        .back
        .map(|b| util::safe_next(Some(&b)))
        .unwrap_or_else(|| "/reservations".into());
    let msg = if owner.purchased {
        "Marked as not yet bought."
    } else {
        "Marked as bought. Nice!"
    };
    Ok(redirect_flash(&back, "ok", msg))
}

#[derive(sqlx::FromRow)]
struct ReservationRow {
    claim_id: i64,
    claim_qty: i64,
    purchased: bool,
    list_id: i64,
    list_title: String,
    recipient: String,
    event_date: Option<String>,
    item_id: i64,
}

pub async fn reservations(State(state): State<AppState>, ctx: Ctx) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let rows: Vec<ReservationRow> = sqlx::query_as(
        "SELECT c.id AS claim_id, c.quantity AS claim_qty, c.purchased, w.id AS list_id, w.title AS list_title,
                COALESCE(w.recipient_name, u.display_name) AS recipient, w.event_date, i.id AS item_id
           FROM claims c JOIN items i ON i.id = c.item_id JOIN wishlists w ON w.id = i.wishlist_id
           JOIN users u ON u.id = w.owner_id
          WHERE c.user_id = ? ORDER BY COALESCE(w.event_date, '9999'), w.title, i.title",
    )
    .bind(user.id)
    .fetch_all(&state.db)
    .await?;
    let mut to_buy = Vec::new();
    let mut purchased = Vec::new();
    let mut totals: std::collections::BTreeMap<String, i64> = Default::default();
    for r in rows {
        let Some(item) = db::get_item(&state.db, r.item_id).await? else {
            continue;
        };
        if !r.purchased {
            if let Some(c) = item.price_cents {
                *totals.entry(item.currency.clone()).or_default() += c * r.claim_qty;
            }
        }
        let view = ReservationView {
            claim_id: r.claim_id,
            quantity: r.claim_qty,
            purchased: r.purchased,
            item,
            list_id: r.list_id,
            list_title: r.list_title,
            recipient: r.recipient,
            event_date: r.event_date,
        };
        if view.purchased {
            purchased.push(view);
        } else {
            to_buy.push(view);
        }
    }
    let total_to_buy = (!totals.is_empty()).then(|| {
        totals
            .iter()
            .map(|(cur, c)| price::format(*c, cur))
            .collect::<Vec<_>>()
            .join(" + ")
    });
    render(ReservationsPage {
        ctx,
        to_buy,
        purchased,
        total_to_buy,
    })
}

// ---------------------------------------------------------------- import

#[derive(Deserialize, Default)]
pub struct AddQuery {
    url: Option<String>,
}

/// Target of the "Add to Wishful Thinking" bookmarklet: pick a list for the current page.
pub async fn add_page(
    State(state): State<AppState>,
    ctx: Ctx,
    Query(q): Query<AddQuery>,
) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let lists: Vec<_> = db::lists_managed_by(&state.db, user.id)
        .await?
        .into_iter()
        .filter(|l| !l.archived)
        .collect();
    let url = q.url.unwrap_or_default();
    if lists.len() == 1 && !url.is_empty() {
        return Ok(axum::response::Redirect::to(&format!(
            "/lists/{}/items/new?url={}",
            lists[0].id,
            util::urlencode(&url)
        ))
        .into_response());
    }
    render(AddPage { ctx, url, lists })
}

#[derive(Deserialize)]
pub struct ImportRequest {
    url: String,
}

#[derive(Serialize)]
pub struct ImportResponse {
    ok: bool,
    error: Option<String>,
    url: String,
    title: String,
    price: String,
    currency: String,
    image_url: String,
    store: String,
    import_source: String,
    llm_used: Option<String>,
    warnings: Vec<String>,
    found: Vec<&'static str>,
}

/// JSON import endpoint used by the item form to fill fields without a page reload.
pub async fn import_api(
    State(state): State<AppState>,
    ctx: Ctx,
    Json(req): Json<ImportRequest>,
) -> AppResult<Json<ImportResponse>> {
    ctx.require_user()?;
    Ok(Json(match state.importer.import(&req.url).await {
        Ok(outcome) => {
            let v = values_from_import(&outcome);
            let mut found = vec![];
            if outcome.info.title.is_some() {
                found.push("name");
            }
            if outcome.info.price_cents.is_some() {
                found.push("price");
            }
            if outcome.info.image_url.is_some() {
                found.push("picture");
            }
            if outcome.info.store.is_some() {
                found.push("store");
            }
            ImportResponse {
                ok: true,
                error: None,
                url: v.url,
                title: v.title,
                price: v.price,
                currency: v.currency,
                image_url: v.image_url,
                store: v.store,
                import_source: v.import_source,
                llm_used: outcome.llm_used,
                warnings: outcome.warnings,
                found,
            }
        }
        Err(e) => ImportResponse {
            ok: false,
            error: Some(e.to_string()),
            url: req.url,
            title: String::new(),
            price: String::new(),
            currency: String::new(),
            image_url: String::new(),
            store: String::new(),
            import_source: String::new(),
            llm_used: None,
            warnings: vec![],
            found: vec![],
        },
    }))
}
