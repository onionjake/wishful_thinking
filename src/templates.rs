//! Template contexts. Each page struct pairs with a file in `templates/`.

use askama::Template;
use axum::response::{Html, IntoResponse, Response};

use crate::db::{Claim, EventDate, FamilySummary, Gift, Item, ListSummary, Member, User, Wishlist};
use crate::error::AppError;
use crate::importer::ImportOutcome;

/// Data every page needs: who is signed in, a pending flash message, etc.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    /// Who pages act as: the signed-in user, or the person an admin is acting on behalf of.
    pub user: Option<User>,
    /// The signed-in user, when they're an admin (whoever they're acting as).
    pub admin: Option<User>,
    pub flash: Option<Flash>,
    pub path: String,
    pub base_url: String,
    pub llm_label: Option<String>,
}

impl Ctx {
    pub fn anonymous() -> Self {
        Ctx {
            path: "/".into(),
            ..Default::default()
        }
    }
    /// An admin is acting as someone else.
    pub fn is_acting(&self) -> bool {
        match (&self.user, &self.admin) {
            (Some(u), Some(a)) => u.id != a.id,
            _ => false,
        }
    }
    /// The admin, while they're acting as someone else. Pages they see as that person still keep
    /// the admin's own surprises (reservations on lists the admin manages) hidden from them.
    pub fn acting_admin(&self) -> Option<&User> {
        self.admin.as_ref().filter(|_| self.is_acting())
    }
    pub fn is_signed_in(&self) -> bool {
        self.user.is_some()
    }
    pub fn user_name(&self) -> &str {
        self.user
            .as_ref()
            .map(|u| u.display_name.as_str())
            .unwrap_or("")
    }
    pub fn initials(&self) -> String {
        self.user
            .as_ref()
            .map(|u| {
                u.display_name
                    .split_whitespace()
                    .filter_map(|w| w.chars().next())
                    .take(2)
                    .collect::<String>()
                    .to_uppercase()
            })
            .unwrap_or_default()
    }
    pub fn is_user(&self, id: &i64) -> bool {
        self.user.as_ref().is_some_and(|u| u.id == *id)
    }
    pub fn nav_active(&self, prefix: &str) -> bool {
        if prefix == "/" {
            self.path == "/" || self.path.starts_with("/lists")
        } else {
            self.path.starts_with(prefix)
        }
    }
}

#[derive(Clone, Debug)]
pub struct Flash {
    pub kind: String,
    pub message: String,
}

impl Flash {
    pub fn encode(&self) -> String {
        crate::util::urlencode(&format!("{}|{}", self.kind, self.message))
    }
    pub fn decode(raw: &str) -> Option<Flash> {
        let decoded: String = url::form_urlencoded::parse(format!("v={raw}").as_bytes())
            .next()?
            .1
            .into_owned();
        let (kind, message) = decoded.split_once('|')?;
        let kind = if kind == "error" { "error" } else { "ok" };
        Some(Flash {
            kind: kind.into(),
            message: message.chars().take(300).collect(),
        })
    }
}

/// Render a template into an HTML response.
pub fn render<T: Template>(t: T) -> Result<Response, AppError> {
    Ok(Html(t.render()?).into_response())
}

// ---------------------------------------------------------------- pages

#[derive(Template)]
#[template(path = "home.html")]
pub struct HomePage {
    pub ctx: Ctx,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardPage {
    pub ctx: Ctx,
    pub my_lists: Vec<ListSummary>,
    pub shared_lists: Vec<ListSummary>,
    pub families: Vec<FamilySummary>,
    pub reservation_count: i64,
}

impl DashboardPage {
    pub fn active_lists(&self) -> Vec<&ListSummary> {
        self.my_lists.iter().filter(|l| !l.archived).collect()
    }
    pub fn archived_lists(&self) -> Vec<&ListSummary> {
        self.my_lists.iter().filter(|l| l.archived).collect()
    }
}

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginPage {
    pub ctx: Ctx,
    pub next: String,
    pub email: String,
    pub error: Option<String>,
    pub invite_family: Option<String>,
}

#[derive(Template)]
#[template(path = "signup.html")]
pub struct SignupPage {
    pub ctx: Ctx,
    pub next: String,
    pub email: String,
    pub display_name: String,
    pub error: Option<String>,
    pub invite_family: Option<String>,
}

#[derive(Template)]
#[template(path = "account.html")]
pub struct AccountPage {
    pub ctx: Ctx,
    pub user: User,
    pub error: Option<String>,
}

impl AccountPage {
    /// The "Add to Wishful Thinking" bookmarklet as a `javascript:` URL.
    pub fn bookmarklet(&self) -> String {
        bookmarklet(&self.ctx.base_url)
    }
}

/// Build the bookmarklet from `static/bookmarklet.js`: drop comments and line breaks, and
/// point it at this site.
pub fn bookmarklet(base_url: &str) -> String {
    let src = include_str!("../static/bookmarklet.js");
    let code: Vec<&str> = src
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect();
    let base = serde_json::to_string(base_url.trim_end_matches('/')).unwrap_or_default();
    format!("javascript:{}", code.join(" ").replace("__BASE__", &base))
}

#[derive(Template)]
#[template(path = "list_form.html")]
pub struct ListFormPage {
    pub ctx: Ctx,
    pub list: Option<Wishlist>,
    pub title: String,
    pub recipient_name: String,
    pub description: String,
    pub event_date: String,
    pub show_claims_to_owner: bool,
    /// Only the owner chooses "Show me what's been reserved"; co-managers have their own setting.
    pub can_set_claims: bool,
    /// Families to share a new list with (family, pre-checked).
    pub families: Vec<(FamilySummary, bool)>,
    pub error: Option<String>,
}

/// One option of a month picker.
pub struct MonthOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

/// Two years of reveal months from the earliest a gift-giver may pick (further if needed to
/// include `selected`). A `selected` month before that, saved before the limit existed, is kept
/// at the top so the picker still shows it.
pub fn month_options(selected: Option<time::Date>) -> Vec<MonthOption> {
    let option = |d: time::Date| MonthOption {
        value: crate::util::month_value(d),
        label: crate::util::pretty_month(d),
        selected: Some(d) == selected,
    };
    let mut d = crate::util::earliest_reveal_month();
    let mut out = Vec::new();
    if let Some(s) = selected.filter(|s| *s < d) {
        out.push(option(s));
    }
    while out.len() < 24 || selected.is_some_and(|s| d <= s) {
        out.push(option(d));
        d = crate::util::next_month(d);
    }
    out
}

/// One item as shown on a list page, with the reservation details the viewer may see.
pub struct ItemView {
    pub item: Item,
    /// Reservations visible to this viewer. For editors of a surprise list, only the ones
    /// whose reveal month has arrived.
    pub claims: Vec<Claim>,
    /// The viewer's own reservation, if any.
    pub my_claim: Option<Claim>,
}

impl ItemView {
    pub fn other_claims(&self) -> Vec<&Claim> {
        let mine = self.my_claim.as_ref().map(|c| c.id);
        self.claims.iter().filter(|c| Some(c.id) != mine).collect()
    }
}

#[derive(Template)]
#[template(path = "list_view.html")]
pub struct ListPage {
    pub ctx: Ctx,
    pub list: Wishlist,
    pub owner_name: String,
    pub is_editor: bool,
    pub is_owner: bool,
    /// Viewing through the public link rather than as a family member.
    pub public_view: bool,
    /// Whether reservation status is shown at all.
    pub claims_visible: bool,
    /// Whether claimer names are shown (family members) or just "Reserved" (public).
    pub claim_names_visible: bool,
    pub can_claim: bool,
    /// Wishes still open to reserve (or, when reservations are hidden, every unreceived wish).
    pub items: Vec<ItemView>,
    /// Wishes already fully reserved by gift-givers; only filled when reservations are shown.
    pub taken: Vec<ItemView>,
    pub received: Vec<ItemView>,
    pub family_names: Vec<String>,
    pub public_url: Option<String>,
    pub guest_name: String,
    /// The viewer's own lists (id, title) they can save a wish to; empty for editors and guests.
    pub my_lists: Vec<(i64, String)>,
    pub sort: String,
    /// Months a gift-giver can pick for when the owner may see the reservation.
    pub reveal_options: Vec<MonthOption>,
    /// Reservations are hidden from the owner or a co-manager, so givers pick a reveal month.
    pub secret_reservations: bool,
}

impl ListPage {
    /// Gift-givers choose a reveal month unless everyone managing the list sees reservations.
    pub fn asks_reveal(&self) -> bool {
        self.secret_reservations
    }
    /// Who the gifts are for, as gift-givers would say it.
    pub fn recipient_label(&self) -> &str {
        self.list.recipient_name.as_deref().unwrap_or_else(|| {
            self.owner_name
                .split_whitespace()
                .next()
                .unwrap_or(&self.owner_name)
        })
    }
    /// Wishes whose giver's reveal month has passed, shown to an editor of a surprise list.
    pub fn revealed_count(&self) -> usize {
        if self.claims_visible {
            return 0;
        }
        self.items.iter().filter(|iv| !iv.claims.is_empty()).count()
    }
    pub fn countdown(&self) -> Option<String> {
        self.list.countdown()
    }
    pub fn pretty_date(&self) -> Option<String> {
        self.list.pretty_event_date()
    }
    pub fn claim_action(&self, item_id: &i64) -> String {
        match (&self.public_view, &self.list.public_token) {
            (true, Some(tok)) => format!("/p/{tok}/items/{item_id}/claim"),
            _ => format!("/items/{item_id}/claim"),
        }
    }
    pub fn save_action(&self, item_id: &i64) -> String {
        match (&self.public_view, &self.list.public_token) {
            (true, Some(tok)) => format!("/p/{tok}/items/{item_id}/save"),
            _ => format!("/items/{item_id}/save"),
        }
    }
    pub fn unclaim_action(&self, claim_id: &i64) -> String {
        match (&self.public_view, &self.list.public_token) {
            (true, Some(tok)) => format!("/p/{tok}/claims/{claim_id}/delete"),
            _ => format!("/claims/{claim_id}/delete"),
        }
    }
    pub fn needs_guest_name(&self) -> bool {
        self.public_view && self.ctx.user.is_none()
    }
}

#[derive(Template)]
#[template(path = "list_share.html")]
pub struct SharePage {
    pub ctx: Ctx,
    pub list: Wishlist,
    pub is_owner: bool,
    /// (family, currently shared)
    pub families: Vec<(FamilySummary, bool)>,
    pub public_url: Option<String>,
    pub owner: User,
    pub managers: Vec<crate::db::Manager>,
    /// Family members who could be made co-managers.
    pub candidates: Vec<User>,
}

#[derive(Template)]
#[template(path = "item_form.html")]
pub struct ItemFormPage {
    pub ctx: Ctx,
    pub list: Wishlist,
    pub item_id: Option<i64>,
    pub form: ItemFormValues,
    pub import: Option<ImportOutcome>,
    pub import_error: Option<String>,
    pub error: Option<String>,
    /// Other lists the editor manages, for moving the item.
    pub other_lists: Vec<(i64, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct ItemFormValues {
    pub url: String,
    pub title: String,
    pub price: String,
    pub currency: String,
    pub image_url: String,
    pub store: String,
    pub notes: String,
    pub priority: i64,
    pub quantity: i64,
    pub import_source: String,
}

impl ItemFormValues {
    pub fn priority_is(&self, p: i64) -> bool {
        self.priority == p
    }
    pub fn currency_is(&self, c: &str) -> bool {
        self.currency == c
    }
}

impl ItemFormPage {
    pub fn currencies(&self) -> Vec<&'static str> {
        vec![
            "USD", "EUR", "GBP", "CAD", "AUD", "NZD", "JPY", "CHF", "SEK", "NOK", "DKK", "INR",
            "MXN", "BRL",
        ]
    }
    pub fn currency_known(&self) -> bool {
        self.currencies().contains(&self.form.currency.as_str())
    }
    pub fn import_fields_found(&self) -> Vec<&'static str> {
        let Some(imp) = &self.import else {
            return vec![];
        };
        let mut found = vec![];
        if imp.info.title.is_some() {
            found.push("name");
        }
        if imp.info.price_cents.is_some() {
            found.push("price");
        }
        if imp.info.image_url.is_some() {
            found.push("picture");
        }
        if imp.info.store.is_some() {
            found.push("store");
        }
        found
    }
}

#[derive(Template)]
#[template(path = "families.html")]
pub struct FamiliesPage {
    pub ctx: Ctx,
    pub families: Vec<FamilySummary>,
    pub error: Option<String>,
}

pub struct InviteView {
    pub id: i64,
    pub url: String,
    pub created_by_name: String,
    pub created_by: i64,
    pub expires_at: Option<String>,
    pub uses: i64,
    pub max_uses: Option<i64>,
}

#[derive(Template)]
#[template(path = "family_view.html")]
pub struct FamilyPage {
    pub ctx: Ctx,
    pub family: FamilySummary,
    pub members: Vec<Member>,
    pub lists: Vec<ListSummary>,
    pub invites: Vec<InviteView>,
    pub is_owner: bool,
    pub me: i64,
}

#[derive(Template)]
#[template(path = "join.html")]
pub struct JoinPage {
    pub ctx: Ctx,
    pub token: String,
    pub family_name: String,
    pub inviter_name: String,
    pub member_count: i64,
    pub already_member: bool,
    pub family_id: i64,
}

pub struct ReservationView {
    pub claim_id: i64,
    pub quantity: i64,
    pub purchased: bool,
    /// The list hides reservations from its owner or a co-manager, so the reveal month matters.
    pub surprise: bool,
    pub reveal_on: Option<String>,
    pub reveal_options: Vec<MonthOption>,
    pub item: Item,
    pub list_id: i64,
    pub list_title: String,
    pub recipient: String,
    pub event_date: Option<String>,
}

impl ReservationView {
    pub fn revealed(&self) -> bool {
        crate::util::is_revealed(self.reveal_on.as_deref())
    }
    pub fn countdown(&self) -> Option<String> {
        self.event_date.as_deref().and_then(crate::util::countdown)
    }
}

#[derive(Template)]
#[template(path = "reservations.html")]
pub struct ReservationsPage {
    pub ctx: Ctx,
    pub to_buy: Vec<ReservationView>,
    pub purchased: Vec<ReservationView>,
    pub total_to_buy: Option<String>,
}

#[derive(Template)]
#[template(path = "history.html")]
pub struct HistoryPage {
    pub ctx: Ctx,
    pub q: String,
    /// Gifts in the whole history, before searching.
    pub total: usize,
    /// (recipient, their gifts, newest first)
    pub groups: Vec<(String, Vec<Gift>)>,
}

#[derive(Template)]
#[template(path = "list_import.html")]
pub struct ListImportPage {
    pub ctx: Ctx,
    pub list: Wishlist,
    pub url: String,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "add.html")]
pub struct AddPage {
    pub ctx: Ctx,
    pub url: String,
    /// Came from the bookmarklet with page data in the URL fragment.
    pub clip: bool,
    pub lists: Vec<ListSummary>,
}

// ---------------------------------------------------------------- admin

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct AdminUser {
    pub id: i64,
    pub email: String,
    pub display_name: String,
    pub is_admin: bool,
    pub created_at: String,
    pub list_count: i64,
    pub wish_count: i64,
    pub family_count: i64,
    pub session_count: i64,
    pub last_sign_in: Option<String>,
}

impl AdminUser {
    pub fn joined(&self) -> String {
        crate::util::pretty_date(self.created_at.get(..10).unwrap_or(&self.created_at))
    }
    pub fn last_sign_in_date(&self) -> Option<String> {
        self.last_sign_in
            .as_deref()
            .map(|d| crate::util::pretty_date(d.get(..10).unwrap_or(d)))
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct AdminList {
    pub id: i64,
    pub title: String,
    pub recipient_name: Option<String>,
    pub event_date: Option<String>,
    pub archived: bool,
    pub public_token: Option<String>,
    #[sqlx(skip)]
    pub public_url: Option<String>,
    pub show_claims_to_owner: bool,
    pub owner_id: i64,
    pub owner_name: String,
    pub created_at: String,
    pub open_count: i64,
    pub received_count: i64,
    pub family_names: Option<String>,
    pub manager_names: Option<String>,
}

impl EventDate for AdminList {
    fn event_date_str(&self) -> Option<&str> {
        self.event_date.as_deref()
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct AdminInvite {
    pub id: i64,
    pub family_id: i64,
    pub family_name: String,
    pub token: String,
    #[sqlx(skip)]
    pub url: String,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub uses: i64,
    pub max_uses: Option<i64>,
    pub revoked: bool,
    pub active: bool,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct AdminReservation {
    pub id: i64,
    pub item_title: String,
    pub list_id: i64,
    pub list_title: String,
    pub recipient: String,
    pub quantity: i64,
    pub purchased: bool,
    pub reveal_on: Option<String>,
    pub created_at: String,
}

impl AdminInvite {
    pub fn expires(&self) -> Option<String> {
        self.expires_at
            .as_deref()
            .map(|d| crate::util::pretty_date(d.get(..10).unwrap_or(d)))
    }
}

impl AdminReservation {
    pub fn reveal_month(&self) -> Option<String> {
        self.reveal_on
            .as_deref()
            .and_then(crate::util::parse_date)
            .map(crate::util::pretty_month)
    }
}

#[derive(Template)]
#[template(path = "admin_users.html")]
pub struct AdminUsersPage {
    pub ctx: Ctx,
    pub users: Vec<AdminUser>,
    pub q: String,
    pub total: i64,
}

#[derive(Template)]
#[template(path = "admin_user.html")]
pub struct AdminUserPage {
    pub ctx: Ctx,
    pub user: AdminUser,
    pub lists: Vec<AdminList>,
    pub families: Vec<FamilySummary>,
    pub invites: Vec<AdminInvite>,
    pub reservations: Vec<AdminReservation>,
    pub gift_count: i64,
}

impl AdminUserPage {
    pub fn is_me(&self) -> bool {
        self.ctx
            .admin
            .as_ref()
            .is_some_and(|a| a.id == self.user.id)
    }
    /// The admin's own account while acting as themselves: plain links will do.
    pub fn direct_links(&self) -> bool {
        self.is_me() && !self.ctx.is_acting()
    }
    pub fn acting_as_them(&self) -> bool {
        self.ctx.is_acting() && self.ctx.is_user(&self.user.id)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bookmarklet_is_a_safe_javascript_url() {
        let b = super::bookmarklet("https://wish.example.com/");
        assert!(b.starts_with("javascript:(function"));
        assert!(b.contains(r#""https://wish.example.com" + "/add?clip=1&url=""#));
        // Browsers percent-decode javascript: URLs and treat # as a fragment.
        assert!(!b.contains('%') && !b.contains('#'), "{b}");
        assert!(!b.contains('\n') && !b.contains("__BASE__"));
    }
}
