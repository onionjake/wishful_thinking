//! Template contexts. Each page struct pairs with a file in `templates/`.

use askama::Template;
use axum::response::{Html, IntoResponse, Response};

use crate::db::{Claim, EventDate, FamilySummary, Item, ListSummary, Member, User, Wishlist};
use crate::error::AppError;
use crate::importer::ImportOutcome;

/// Data every page needs: who is signed in, a pending flash message, etc.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub user: Option<User>,
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
    /// Families to share a new list with (family, pre-checked).
    pub families: Vec<(FamilySummary, bool)>,
    pub error: Option<String>,
}

/// One item as shown on a list page, with the reservation details the viewer may see.
pub struct ItemView {
    pub item: Item,
    /// Reservations visible to this viewer (empty when hidden to keep the surprise).
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
    pub items: Vec<ItemView>,
    pub received: Vec<ItemView>,
    pub family_names: Vec<String>,
    pub public_url: Option<String>,
    pub guest_name: String,
    pub sort: String,
}

impl ListPage {
    pub fn countdown(&self) -> Option<String> {
        self.list.countdown()
    }
    pub fn pretty_date(&self) -> Option<String> {
        self.list.pretty_event_date()
    }
    pub fn total_display(&self) -> Option<String> {
        let mut by_currency: std::collections::BTreeMap<&str, i64> = Default::default();
        for iv in &self.items {
            if let Some(c) = iv.item.price_cents {
                *by_currency.entry(iv.item.currency.as_str()).or_default() += c * iv.item.quantity;
            }
        }
        if by_currency.is_empty() {
            return None;
        }
        Some(
            by_currency
                .into_iter()
                .map(|(cur, cents)| crate::importer::price::format(cents, cur))
                .collect::<Vec<_>>()
                .join(" + "),
        )
    }
    pub fn claim_action(&self, item_id: &i64) -> String {
        match (&self.public_view, &self.list.public_token) {
            (true, Some(tok)) => format!("/p/{tok}/items/{item_id}/claim"),
            _ => format!("/items/{item_id}/claim"),
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
    pub managers: Vec<User>,
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
    pub item: Item,
    pub list_id: i64,
    pub list_title: String,
    pub recipient: String,
    pub event_date: Option<String>,
}

impl ReservationView {
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
#[template(path = "add.html")]
pub struct AddPage {
    pub ctx: Ctx,
    pub url: String,
    pub lists: Vec<ListSummary>,
}
