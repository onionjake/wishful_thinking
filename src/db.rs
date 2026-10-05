//! Database connection, models and shared queries.

use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};

use crate::importer::price;
use crate::util;

pub type Db = SqlitePool;

pub async fn connect(database_url: &str) -> anyhow::Result<Db> {
    let in_memory = database_url.contains(":memory:");
    let mut opts = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    if !in_memory {
        opts = opts.journal_mode(SqliteJournalMode::Wal);
    }
    let pool = SqlitePoolOptions::new()
        // Every connection to ":memory:" would be a separate database.
        .max_connections(if in_memory { 1 } else { 8 })
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

// ---------------------------------------------------------------- models

#[derive(Clone, Debug, FromRow)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub display_name: String,
}

impl User {
    pub fn first_name(&self) -> &str {
        self.display_name
            .split_whitespace()
            .next()
            .unwrap_or(&self.display_name)
    }
}

#[derive(Clone, Debug, FromRow)]
pub struct Wishlist {
    pub id: i64,
    pub owner_id: i64,
    pub title: String,
    pub recipient_name: Option<String>,
    pub description: Option<String>,
    pub event_date: Option<String>,
    pub public_token: Option<String>,
    pub show_claims_to_owner: bool,
    pub archived: bool,
}

pub trait EventDate {
    fn event_date_str(&self) -> Option<&str>;
    fn countdown(&self) -> Option<String> {
        self.event_date_str().and_then(util::countdown)
    }
    fn pretty_event_date(&self) -> Option<String> {
        self.event_date_str().map(util::pretty_date)
    }
    fn event_is_past(&self) -> bool {
        self.event_date_str()
            .and_then(util::parse_date)
            .is_some_and(|d| d < util::today())
    }
}

impl EventDate for Wishlist {
    fn event_date_str(&self) -> Option<&str> {
        self.event_date.as_deref()
    }
}

pub const WISHLIST_COLS: &str =
    "id, owner_id, title, recipient_name, description, event_date, public_token, show_claims_to_owner, archived";

#[derive(Clone, Debug, FromRow)]
pub struct ListSummary {
    pub id: i64,
    pub title: String,
    pub recipient_name: Option<String>,
    pub event_date: Option<String>,
    pub owner_id: i64,
    pub owner_name: String,
    pub item_count: i64,
    pub public_token: Option<String>,
    pub family_names: Option<String>,
    pub archived: bool,
    pub images: Option<String>,
}

impl EventDate for ListSummary {
    fn event_date_str(&self) -> Option<&str> {
        self.event_date.as_deref()
    }
}

impl ListSummary {
    pub fn preview_images(&self) -> Vec<&str> {
        self.images
            .as_deref()
            .map(|s| s.lines().filter(|l| !l.is_empty()).take(4).collect())
            .unwrap_or_default()
    }
    pub fn is_shared(&self) -> bool {
        self.public_token.is_some() || self.family_names.is_some()
    }
}

const SUMMARY_SELECT: &str = "SELECT w.id, w.title, w.recipient_name, w.event_date, w.owner_id, u.display_name AS owner_name,
    (SELECT COUNT(*) FROM items i WHERE i.wishlist_id = w.id AND i.received = 0) AS item_count,
    w.public_token,
    (SELECT group_concat(f.name, ', ') FROM wishlist_families wf JOIN families f ON f.id = wf.family_id WHERE wf.wishlist_id = w.id) AS family_names,
    w.archived,
    (SELECT group_concat(i.image_url, char(10)) FROM items i WHERE i.wishlist_id = w.id AND i.image_url IS NOT NULL AND i.received = 0) AS images
  FROM wishlists w JOIN users u ON u.id = w.owner_id";

/// Lists the user owns or co-manages.
pub async fn lists_managed_by(db: &Db, user_id: i64) -> sqlx::Result<Vec<ListSummary>> {
    sqlx::query_as::<_, ListSummary>(&format!(
        "{SUMMARY_SELECT} WHERE w.owner_id = ?1
            OR w.id IN (SELECT wishlist_id FROM wishlist_managers WHERE user_id = ?1)
         ORDER BY w.archived, COALESCE(w.event_date, '9999'), w.title"
    ))
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Lists shared with the user through their families (excluding ones they manage).
pub async fn lists_shared_with(db: &Db, user_id: i64) -> sqlx::Result<Vec<ListSummary>> {
    sqlx::query_as::<_, ListSummary>(&format!(
        "{SUMMARY_SELECT} WHERE w.archived = 0 AND w.owner_id != ?1
            AND w.id NOT IN (SELECT wishlist_id FROM wishlist_managers WHERE user_id = ?1)
            AND w.id IN (SELECT wf.wishlist_id FROM wishlist_families wf
                         JOIN family_members fm ON fm.family_id = wf.family_id WHERE fm.user_id = ?1)
         ORDER BY COALESCE(w.event_date, '9999'), u.display_name, w.title"
    ))
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Lists shared into a particular family.
pub async fn lists_in_family(db: &Db, family_id: i64) -> sqlx::Result<Vec<ListSummary>> {
    sqlx::query_as::<_, ListSummary>(&format!(
        "{SUMMARY_SELECT} WHERE w.archived = 0
            AND w.id IN (SELECT wishlist_id FROM wishlist_families WHERE family_id = ?1)
         ORDER BY COALESCE(w.event_date, '9999'), u.display_name, w.title"
    ))
    .bind(family_id)
    .fetch_all(db)
    .await
}

#[derive(Clone, Debug, FromRow)]
pub struct Item {
    pub id: i64,
    pub wishlist_id: i64,
    pub title: String,
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub price_cents: Option<i64>,
    pub currency: String,
    pub store: Option<String>,
    pub notes: Option<String>,
    pub priority: i64,
    pub quantity: i64,
    pub received: bool,
    pub import_source: Option<String>,
    pub claimed_qty: i64,
}

impl Item {
    pub fn price_display(&self) -> Option<String> {
        self.price_cents.map(|c| price::format(c, &self.currency))
    }
    pub fn price_input(&self) -> String {
        self.price_cents
            .map(|c| price::to_input(c, &self.currency))
            .unwrap_or_default()
    }
    pub fn priority_label(&self) -> &'static str {
        priority_label(self.priority)
    }
    pub fn remaining(&self) -> i64 {
        (self.quantity - self.claimed_qty).max(0)
    }
    pub fn fully_claimed(&self) -> bool {
        self.claimed_qty >= self.quantity
    }
    pub fn host(&self) -> Option<String> {
        self.url
            .as_deref()
            .and_then(|u| url::Url::parse(u).ok())
            .and_then(|u| {
                u.host_str()
                    .map(|h| h.trim_start_matches("www.").to_string())
            })
    }
}

pub fn priority_label(p: i64) -> &'static str {
    match p {
        1 => "Most wanted",
        3 => "Nice to have",
        _ => "Would love",
    }
}

pub const ITEM_SELECT: &str =
    "SELECT i.id, i.wishlist_id, i.title, i.url, i.image_url, i.price_cents, i.currency, i.store,
    i.notes, i.priority, i.quantity, i.received, i.import_source,
    COALESCE((SELECT SUM(c.quantity) FROM claims c WHERE c.item_id = i.id), 0) AS claimed_qty
  FROM items i";

pub async fn items_for_list(db: &Db, list_id: i64) -> sqlx::Result<Vec<Item>> {
    sqlx::query_as::<_, Item>(&format!(
        "{ITEM_SELECT} WHERE i.wishlist_id = ? ORDER BY i.received, i.priority, i.id"
    ))
    .bind(list_id)
    .fetch_all(db)
    .await
}

pub async fn get_item(db: &Db, item_id: i64) -> sqlx::Result<Option<Item>> {
    sqlx::query_as::<_, Item>(&format!("{ITEM_SELECT} WHERE i.id = ?"))
        .bind(item_id)
        .fetch_optional(db)
        .await
}

#[derive(Clone, Debug, FromRow)]
pub struct Claim {
    pub id: i64,
    pub item_id: i64,
    pub user_id: Option<i64>,
    pub claimer_name: String,
    pub guest_token: Option<String>,
    pub quantity: i64,
    pub purchased: bool,
}

pub async fn claims_for_list(db: &Db, list_id: i64) -> sqlx::Result<Vec<Claim>> {
    sqlx::query_as::<_, Claim>(
        "SELECT c.id, c.item_id, c.user_id, COALESCE(u.display_name, c.guest_name, 'Someone') AS claimer_name,
                c.guest_token, c.quantity, c.purchased
           FROM claims c JOIN items i ON i.id = c.item_id LEFT JOIN users u ON u.id = c.user_id
          WHERE i.wishlist_id = ? ORDER BY c.id",
    )
    .bind(list_id)
    .fetch_all(db)
    .await
}

#[derive(Clone, Debug, FromRow)]
pub struct Family {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
pub struct FamilySummary {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub member_count: i64,
    pub list_count: i64,
    pub role: String,
}

pub async fn families_for_user(db: &Db, user_id: i64) -> sqlx::Result<Vec<FamilySummary>> {
    sqlx::query_as::<_, FamilySummary>(
        "SELECT f.id, f.name, f.description,
                (SELECT COUNT(*) FROM family_members m WHERE m.family_id = f.id) AS member_count,
                (SELECT COUNT(*) FROM wishlist_families wf JOIN wishlists w ON w.id = wf.wishlist_id
                  WHERE wf.family_id = f.id AND w.archived = 0) AS list_count,
                fm.role
           FROM families f JOIN family_members fm ON fm.family_id = f.id
          WHERE fm.user_id = ? ORDER BY f.name",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

#[derive(Clone, Debug, FromRow)]
pub struct Member {
    pub user_id: i64,
    pub display_name: String,
    pub email: String,
    pub role: String,
}

pub async fn family_members(db: &Db, family_id: i64) -> sqlx::Result<Vec<Member>> {
    sqlx::query_as::<_, Member>(
        "SELECT u.id AS user_id, u.display_name, u.email, fm.role
           FROM family_members fm JOIN users u ON u.id = fm.user_id
          WHERE fm.family_id = ? ORDER BY fm.role DESC, u.display_name",
    )
    .bind(family_id)
    .fetch_all(db)
    .await
}

/// The user's role in a family, if they are a member.
pub async fn family_role(db: &Db, family_id: i64, user_id: i64) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT role FROM family_members WHERE family_id = ? AND user_id = ?")
        .bind(family_id)
        .bind(user_id)
        .fetch_optional(db)
        .await
}

// ---------------------------------------------------------------- access control

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Owner,
    Manager,
    /// Can see the list through a shared family.
    Family,
    None,
}

impl Access {
    pub fn can_edit(self) -> bool {
        matches!(self, Access::Owner | Access::Manager)
    }
    pub fn can_view(self) -> bool {
        self != Access::None
    }
}

pub async fn get_list(db: &Db, list_id: i64) -> sqlx::Result<Option<Wishlist>> {
    sqlx::query_as::<_, Wishlist>(&format!(
        "SELECT {WISHLIST_COLS} FROM wishlists WHERE id = ?"
    ))
    .bind(list_id)
    .fetch_optional(db)
    .await
}

pub async fn list_access(db: &Db, list: &Wishlist, user_id: Option<i64>) -> sqlx::Result<Access> {
    let Some(uid) = user_id else {
        return Ok(Access::None);
    };
    if list.owner_id == uid {
        return Ok(Access::Owner);
    }
    let manager: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM wishlist_managers WHERE wishlist_id = ? AND user_id = ?")
            .bind(list.id)
            .bind(uid)
            .fetch_optional(db)
            .await?;
    if manager.is_some() {
        return Ok(Access::Manager);
    }
    let family: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM wishlist_families wf JOIN family_members fm ON fm.family_id = wf.family_id
          WHERE wf.wishlist_id = ? AND fm.user_id = ? LIMIT 1",
    )
    .bind(list.id)
    .bind(uid)
    .fetch_optional(db)
    .await?;
    Ok(if family.is_some() {
        Access::Family
    } else {
        Access::None
    })
}
