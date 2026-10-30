//! End-to-end tests that drive the HTTP app in-process against an in-memory database.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;
use wishful_thinking::{app, AppState, Config};

struct Client {
    app: Router,
    cookies: Vec<(String, String)>,
}

struct Resp {
    status: StatusCode,
    location: Option<String>,
    body: String,
}

impl Client {
    fn new(app: &Router) -> Self {
        Client {
            app: app.clone(),
            cookies: vec![],
        }
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    async fn send(&mut self, req: Request<Body>) -> Resp {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        for sc in resp.headers().get_all(header::SET_COOKIE) {
            let sc = sc.to_str().unwrap();
            let pair = sc.split(';').next().unwrap();
            let (k, v) = pair.split_once('=').unwrap();
            self.cookies.retain(|(ck, _)| ck != k);
            if !v.is_empty() && !sc.contains("Max-Age=0") {
                self.cookies.push((k.to_string(), v.to_string()));
            }
        }
        let status = resp.status();
        let location = resp
            .headers()
            .get(header::LOCATION)
            .map(|l| l.to_str().unwrap().to_string());
        let body = String::from_utf8(
            resp.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        Resp {
            status,
            location,
            body,
        }
    }

    async fn get(&mut self, path: &str) -> Resp {
        let req = Request::get(path)
            .header(header::COOKIE, self.cookie_header())
            .body(Body::empty())
            .unwrap();
        self.send(req).await
    }

    async fn post(&mut self, path: &str, form: &[(&str, &str)]) -> Resp {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let req = Request::post(path)
            .header(header::COOKIE, self.cookie_header())
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        self.send(req).await
    }

    async fn signup(&mut self, name: &str, email: &str) {
        let r = self
            .post(
                "/signup",
                &[
                    ("display_name", name),
                    ("email", email),
                    ("password", "correct horse"),
                ],
            )
            .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "signup failed: {}", r.body);
    }
}

async fn test_app() -> Router {
    let state = AppState::new(Config::for_tests()).await.unwrap();
    app(state)
}

fn id_from_location(loc: &str, prefix: &str) -> i64 {
    loc.trim_start_matches(prefix)
        .split(['/', '#', '?'])
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// Pull the invite token out of a family page.
/// "YYYY" for a year this many years from now.
fn year_from_now(years: i32) -> i32 {
    time::OffsetDateTime::now_utc().date().year() + years
}

fn invite_token(body: &str) -> String {
    let start = body.find("/join/").expect("invite link on page") + "/join/".len();
    body[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

#[tokio::test]
async fn signup_login_logout() {
    let app = test_app().await;
    let mut c = Client::new(&app);
    assert!(c
        .get("/")
        .await
        .body
        .contains("Wish lists for the whole family"));
    c.signup("Sarah Lee", "sarah@example.com").await;
    let home = c.get("/").await;
    assert!(home.body.contains("Hi Sarah Lee"));
    // Flash message shown once.
    assert!(home.body.contains("Welcome to Wishful Thinking"));
    assert!(!c
        .get("/")
        .await
        .body
        .contains("Welcome to Wishful Thinking"));

    c.post("/logout", &[]).await;
    assert!(c.get("/").await.body.contains("Start a wish list"));
    let bad = c
        .post(
            "/login",
            &[
                ("email", "sarah@example.com"),
                ("password", "wrong password"),
            ],
        )
        .await;
    assert!(bad.body.contains("don&#x27;t match") || bad.body.contains("don't match"));
    let ok = c
        .post(
            "/login",
            &[
                ("email", "SARAH@example.com"),
                ("password", "correct horse"),
                ("next", "/families"),
            ],
        )
        .await;
    assert_eq!(ok.location.as_deref(), Some("/families"));

    // Duplicate email rejected.
    let mut other = Client::new(&app);
    let dup = other
        .post(
            "/signup",
            &[
                ("display_name", "X"),
                ("email", "sarah@example.com"),
                ("password", "12345678"),
            ],
        )
        .await;
    assert_eq!(dup.status, StatusCode::OK);
    assert!(dup.body.contains("already exists"));
}

#[tokio::test]
async fn protected_pages_redirect_to_login() {
    let app = test_app().await;
    let mut c = Client::new(&app);
    let r = c.get("/lists/new").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location.as_deref(), Some("/login?next=%2Flists%2Fnew"));
}

#[tokio::test]
async fn family_sharing_and_secret_reservations() {
    let app = test_app().await;

    // Mom creates a family and a list for her kid.
    let mut mom = Client::new(&app);
    mom.signup("Mom", "mom@example.com").await;
    let r = mom.post("/families", &[("name", "The Lees")]).await;
    let family_id = id_from_location(r.location.as_deref().unwrap(), "/families/");
    mom.post(
        &format!("/families/{family_id}/invites"),
        &[("expires_days", "30"), ("max_uses", "0")],
    )
    .await;
    let token = invite_token(&mom.get(&format!("/families/{family_id}")).await.body);

    let next_year = year_from_now(1);
    let event_date = format!("{next_year}-05-01");
    let june = format!("{next_year}-06");
    let r = mom
        .post(
            "/lists",
            &[
                ("title", "Maya's Birthday"),
                ("recipient_name", "Maya"),
                ("event_date", &event_date),
                ("families", &family_id.to_string()),
            ],
        )
        .await;
    let list_id = id_from_location(r.location.as_deref().unwrap(), "/lists/");
    let r = mom
        .post(
            &format!("/lists/{list_id}/items"),
            &[
                ("title", "Robot kit"),
                ("price", "49.99"),
                ("currency", "USD"),
                ("priority", "1"),
                ("quantity", "1"),
                ("url", "https://shop.example.com/robot"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    mom.post(
        &format!("/lists/{list_id}/items"),
        &[("title", "Socks"), ("quantity", "3")],
    )
    .await;
    let page = mom.get(&format!("/lists/{list_id}")).await.body;
    assert!(page.contains("Robot kit") && page.contains("$49.99") && page.contains("Most wanted"));

    // A stranger can't see the list.
    let mut stranger = Client::new(&app);
    stranger.signup("Stranger", "stranger@example.com").await;
    assert_eq!(
        stranger.get(&format!("/lists/{list_id}")).await.status,
        StatusCode::NOT_FOUND
    );

    // Grandma follows the invite link, signs up from it, and lands in the family.
    let mut grandma = Client::new(&app);
    let join = grandma.get(&format!("/join/{token}")).await;
    assert!(join.body.contains("Join The Lees"));
    let r = grandma
        .post(
            "/signup",
            &[
                ("display_name", "Grandma"),
                ("email", "grandma@example.com"),
                ("password", "cookies123"),
                ("next", &format!("/join/{token}")),
            ],
        )
        .await;
    assert_eq!(
        r.location.as_deref(),
        Some(format!("/join/{token}").as_str())
    );
    let r = grandma.post(&format!("/join/{token}"), &[]).await;
    assert_eq!(
        r.location.as_deref(),
        Some(format!("/families/{family_id}").as_str())
    );

    // Grandma sees the list on her home page and reserves the robot kit.
    assert!(
        grandma.get("/").await.body.contains("Maya&#x27;s Birthday")
            || grandma.get("/").await.body.contains("Maya's Birthday")
    );
    let page = grandma.get(&format!("/lists/{list_id}")).await.body;
    assert!(page.contains("I&#x27;ll get this") || page.contains("I'll get this"));
    let robot_id: i64 = {
        let idx = page.find("/items/").unwrap() + "/items/".len();
        page[idx..].split('/').next().unwrap().parse().unwrap()
    };
    // The picker suggests the month after the occasion (the list's date is May 1).
    let june_option = format!(r#"<option value="{june}" selected>June {next_year}</option>"#);
    assert!(page.contains(&june_option));
    // Reserving without saying when Mom may find out is refused.
    let r = grandma.post(&format!("/items/{robot_id}/claim"), &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(grandma
        .get(&format!("/lists/{list_id}"))
        .await
        .body
        .contains("Please pick the month"));
    let r = grandma
        .post(
            &format!("/items/{robot_id}/claim"),
            &[("reveal_month", &june)],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let shopping = grandma.get("/reservations").await.body;
    assert!(shopping.contains("Robot kit"));
    assert!(shopping.contains(&june_option));

    // Mom does not see the reservation (surprise preserved)...
    let page = mom.get(&format!("/lists/{list_id}")).await.body;
    assert!(!page.contains("Grandma"));
    assert!(page.contains("hidden from you"));
    // ...until she opts in, as a parent managing a child's list would.
    mom.post(
        &format!("/lists/{list_id}/edit"),
        &[
            ("title", "Maya's Birthday"),
            ("recipient_name", "Maya"),
            ("show_claims_to_owner", "1"),
        ],
    )
    .await;
    assert!(mom
        .get(&format!("/lists/{list_id}"))
        .await
        .body
        .contains("Reserved by Grandma"));

    // Once she turns it back off, nothing is revealed before June.
    mom.post(
        &format!("/lists/{list_id}/edit"),
        &[("title", "Maya's Birthday"), ("recipient_name", "Maya")],
    )
    .await;
    assert!(!mom
        .get(&format!("/lists/{list_id}"))
        .await
        .body
        .contains("Grandma"));
    // Grandma can change the month from her shopping list; once it arrives, Mom sees who gave it.
    let claim_id: i64 = {
        let idx = shopping.find("/claims/").unwrap() + "/claims/".len();
        shopping[idx..].split('/').next().unwrap().parse().unwrap()
    };
    let today = time::OffsetDateTime::now_utc().date();
    let this_month = format!("{:04}-{:02}", today.year(), u8::from(today.month()));
    let r = grandma
        .post(
            &format!("/claims/{claim_id}/reveal"),
            &[("reveal_month", &this_month), ("back", "/reservations")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(grandma
        .get("/reservations")
        .await
        .body
        .contains("Maya can see this now"));
    let page = mom.get(&format!("/lists/{list_id}")).await.body;
    assert!(page.contains("From Grandma"));
    // Nobody else can change Grandma's reservation.
    assert_eq!(
        mom.post(
            &format!("/claims/{claim_id}/reveal"),
            &[("reveal_month", &this_month)]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    mom.post(
        &format!("/lists/{list_id}/edit"),
        &[
            ("title", "Maya's Birthday"),
            ("recipient_name", "Maya"),
            ("show_claims_to_owner", "1"),
        ],
    )
    .await;

    // A second claim on a single-quantity item is refused.
    let r = mom.post(&format!("/items/{robot_id}/claim"), &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = mom.get(&format!("/lists/{list_id}")).await.body;
    assert!(page.contains("already reserved"));

    // Grandma can't edit Mom's items.
    assert_eq!(
        grandma.get(&format!("/items/{robot_id}/edit")).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn public_link_guest_reservation() {
    let app = test_app().await;
    let mut owner = Client::new(&app);
    owner.signup("Alex", "alex@example.com").await;
    let r = owner.post("/lists", &[("title", "Wedding registry")]).await;
    let list_id = id_from_location(r.location.as_deref().unwrap(), "/lists/");
    owner
        .post(
            &format!("/lists/{list_id}/items"),
            &[("title", "Teapot"), ("price", "35"), ("quantity", "2")],
        )
        .await;

    // No public link yet: the list is private.
    let share = owner.get(&format!("/lists/{list_id}/share")).await.body;
    assert!(share.contains("Create public link"));
    owner
        .post(&format!("/lists/{list_id}/public"), &[("action", "enable")])
        .await;
    let share = owner.get(&format!("/lists/{list_id}/share")).await.body;
    let start = share.find("/p/").unwrap() + 3;
    let token: String = share[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    assert!(
        token.len() >= 24,
        "public token should be long and unguessable"
    );

    // An anonymous guest views and reserves one of two teapots.
    let mut guest = Client::new(&app);
    let page = guest.get(&format!("/p/{token}")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Teapot") && page.body.contains("Your name"));
    let item_id: i64 = {
        let idx = page.body.find("/items/").unwrap() + "/items/".len();
        page.body[idx..].split('/').next().unwrap().parse().unwrap()
    };
    let r = guest
        .post(
            &format!("/p/{token}/items/{item_id}/claim"),
            &[
                ("guest_name", "Jamie"),
                ("quantity", "1"),
                ("reveal_month", &format!("{}-01", year_from_now(1))),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = guest.get(&format!("/p/{token}")).await.body;
    assert!(page.contains("You&#x27;re getting this") || page.contains("You're getting this"));
    // Guests don't see other people's names, only that something is reserved.
    let mut guest2 = Client::new(&app);
    let page2 = guest2.get(&format!("/p/{token}")).await.body;
    assert!(page2.contains("Reserved (1 of 2)"));
    assert!(!page2.contains("Jamie"));

    // Owner turns the link off; it stops working.
    owner
        .post(
            &format!("/lists/{list_id}/public"),
            &[("action", "disable")],
        )
        .await;
    assert_eq!(
        guest.get(&format!("/p/{token}")).await.status,
        StatusCode::NOT_FOUND
    );
    // A made-up token never works.
    assert_eq!(
        guest.get("/p/not-a-real-token").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn invites_can_be_revoked_and_limited() {
    let app = test_app().await;
    let mut a = Client::new(&app);
    a.signup("A", "a@example.com").await;
    let r = a.post("/families", &[("name", "Fam")]).await;
    let fid = id_from_location(r.location.as_deref().unwrap(), "/families/");
    a.post(
        &format!("/families/{fid}/invites"),
        &[("expires_days", "7"), ("max_uses", "1")],
    )
    .await;
    let token = invite_token(&a.get(&format!("/families/{fid}")).await.body);

    let mut b = Client::new(&app);
    b.signup("B", "b@example.com").await;
    b.post(&format!("/join/{token}"), &[]).await;
    assert_eq!(
        b.get(&format!("/families/{fid}")).await.status,
        StatusCode::OK
    );

    // Single-use link is now spent.
    let mut c = Client::new(&app);
    c.signup("C", "c@example.com").await;
    assert_eq!(
        c.get(&format!("/join/{token}")).await.status,
        StatusCode::BAD_REQUEST
    );

    // Owner can remove a member.
    let bid = 2;
    a.post(&format!("/families/{fid}/members/{bid}/remove"), &[])
        .await;
    assert_eq!(
        b.get(&format!("/families/{fid}")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn cross_site_posts_are_blocked() {
    let app = test_app().await;
    let mut c = Client::new(&app);
    c.signup("Pat", "pat@example.com").await;
    let req = Request::post("/lists")
        .header(header::COOKIE, c.cookie_header())
        .header(header::HOST, "wishful.test")
        .header(header::ORIGIN, "https://evil.example")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("title=Hacked"))
        .unwrap();
    assert_eq!(c.send(req).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn reservations_are_revealed_in_the_chosen_month() {
    let app = test_app().await;
    let mut alex = Client::new(&app);
    alex.signup("Alex Kim", "alex@example.com").await;
    let r = alex.post("/lists", &[("title", "Christmas")]).await;
    let list_id = id_from_location(r.location.as_deref().unwrap(), "/lists/");
    for title in ["Scarf", "Board game"] {
        alex.post(&format!("/lists/{list_id}/items"), &[("title", title)])
            .await;
    }
    alex.post(&format!("/lists/{list_id}/public"), &[("action", "enable")])
        .await;
    let share = alex.get(&format!("/lists/{list_id}/share")).await.body;
    let start = share.find("/p/").unwrap() + 3;
    let token: String = share[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();

    let mut guest = Client::new(&app);
    let page = guest.get(&format!("/p/{token}")).await.body;
    assert!(page.contains("Alex can see it"));
    let ids: Vec<i64> = page
        .match_indices("/items/")
        .map(|(i, _)| page[i + 7..].split('/').next().unwrap().parse().unwrap())
        .collect();
    let this_month = {
        let today = time::OffsetDateTime::now_utc().date();
        format!("{:04}-{:02}", today.year(), u8::from(today.month()))
    };
    // A gift given already (reveal this month), and one still secret.
    let next_january = format!("{}-01", year_from_now(1));
    for (id, month) in [(ids[0], &this_month), (ids[1], &next_january)] {
        let r = guest
            .post(
                &format!("/p/{token}/items/{id}/claim"),
                &[("guest_name", "Jamie"), ("reveal_month", month)],
            )
            .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER);
    }
    let page = guest.get(&format!("/p/{token}")).await.body;
    assert_eq!(page.matches("You're getting this").count(), 2);
    // A month in the past is refused.
    let mut guest2 = Client::new(&app);
    guest2
        .post(
            &format!("/p/{token}/items/{}/claim", ids[1]),
            &[("guest_name", "Sam"), ("reveal_month", "2001-01")],
        )
        .await;
    assert!(guest2
        .get(&format!("/p/{token}"))
        .await
        .body
        .contains("pick a month from the list"));

    let page = alex.get(&format!("/lists/{list_id}")).await.body;
    assert!(page.contains("From Jamie"));
    assert_eq!(page.matches("From Jamie").count(), 1);
    assert!(page.contains("1 wish has been given to you"));
}

/// Sign up, create a list with one priced item, share it into a new family and bring in a second
/// person who can reserve from it. Returns (owner, buyer, list id, item id).
async fn family_with_item(app: &Router) -> (Client, Client, i64, i64) {
    let mut owner = Client::new(app);
    owner.signup("Grandpa Joe", "joe@example.com").await;
    let fam = owner.post("/families", &[("name", "The Joes")]).await;
    let fam_id = id_from_location(&fam.location.unwrap(), "/families/");
    owner
        .post(&format!("/families/{fam_id}/invites"), &[("expires", "")])
        .await;
    let token = invite_token(&owner.get(&format!("/families/{fam_id}")).await.body);
    let list = owner
        .post(
            "/lists",
            &[
                ("title", "Birthday"),
                ("families", fam_id.to_string().as_str()),
            ],
        )
        .await;
    let list_id = id_from_location(&list.location.unwrap(), "/lists/");
    owner
        .post(
            &format!("/lists/{list_id}/items"),
            &[("title", "Fishing rod"), ("price", "40.00")],
        )
        .await;
    let page = owner.get(&format!("/lists/{list_id}")).await.body;
    let start = page.find("id=\"item-").unwrap() + "id=\"item-".len();
    let item_id: i64 = page[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap();
    let mut buyer = Client::new(app);
    buyer.signup("Mom", "mom@example.com").await;
    buyer.post(&format!("/join/{token}"), &[]).await;
    (owner, buyer, list_id, item_id)
}

/// Pull a claim id for one of the viewer's reservations out of the shopping list.
fn claim_id(body: &str) -> i64 {
    let start = body.find("/claims/").expect("claim on page") + "/claims/".len();
    body[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn gift_history_outlives_the_list_and_stays_private() {
    let app = test_app().await;
    let (mut owner, mut buyer, list_id, item_id) = family_with_item(&app).await;
    buyer
        .post(
            &format!("/items/{item_id}/claim"),
            &[("reveal_month", &format!("{}-01", year_from_now(1)))],
        )
        .await;

    // Reserving alone isn't buying.
    assert!(buyer
        .get("/history")
        .await
        .body
        .contains("Nothing here yet"));

    let claim = claim_id(&buyer.get("/reservations").await.body);
    buyer.post(&format!("/claims/{claim}/purchased"), &[]).await;
    let history = buyer.get("/history").await.body;
    assert!(history.contains("Fishing rod"));
    assert!(history.contains("For Grandpa Joe"));
    assert!(history.contains("Birthday"));

    // Undoing the purchase takes it back out.
    buyer.post(&format!("/claims/{claim}/purchased"), &[]).await;
    assert!(!buyer.get("/history").await.body.contains("Fishing rod"));
    buyer.post(&format!("/claims/{claim}/purchased"), &[]).await;

    // The list owner can't see it, on the history page or by guessing a delete.
    assert!(!owner.get("/history").await.body.contains("Fishing rod"));
    let page = buyer.get("/history").await.body;
    let start = page.find("/history/").unwrap() + "/history/".len();
    let gift: String = page[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let r = owner.post(&format!("/history/{gift}/delete"), &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Search finds gifts by name and by person.
    assert!(buyer
        .get("/history?q=FISHING")
        .await
        .body
        .contains("Fishing rod"));
    assert!(buyer
        .get("/history?q=grandpa")
        .await
        .body
        .contains("Fishing rod"));
    assert!(buyer
        .get("/history?q=socks")
        .await
        .body
        .contains("No gifts match"));

    // It survives the owner renaming the item and then deleting the whole list.
    owner
        .post(&format!("/items/{item_id}/edit"), &[("title", "Renamed")])
        .await;
    owner.post(&format!("/lists/{list_id}/delete"), &[]).await;
    let history = buyer.get("/history").await.body;
    assert!(history.contains("Fishing rod"));
    assert!(!history.contains("Renamed"));

    // Buyers can tidy up detached entries themselves.
    buyer.post(&format!("/history/{gift}/delete"), &[]).await;
    assert!(!buyer.get("/history").await.body.contains("Fishing rod"));
}

#[tokio::test]
async fn releasing_a_bought_gift_removes_it_from_history() {
    let app = test_app().await;
    let (_owner, mut buyer, _list_id, item_id) = family_with_item(&app).await;
    buyer
        .post(
            &format!("/items/{item_id}/claim"),
            &[("reveal_month", &format!("{}-01", year_from_now(1)))],
        )
        .await;
    let claim = claim_id(&buyer.get("/reservations").await.body);
    buyer.post(&format!("/claims/{claim}/purchased"), &[]).await;
    assert!(buyer.get("/history").await.body.contains("Fishing rod"));
    buyer.post(&format!("/claims/{claim}/delete"), &[]).await;
    assert!(!buyer.get("/history").await.body.contains("Fishing rod"));
}

const AMAZON_PAGE_1: &str = r#"<html><head><title>Amazon.com</title></head><body>
  <span id="profile-list-name">Birthday ideas</span>
  <ul id="g-items">
    <li data-itemId="I1" data-price="12.50" data-reposition-action-params="{&quot;itemExternalId&quot;:&quot;ASIN:B000000001|X&quot;}">
      <a id="itemName_I1" title="Kite" href="/dp/B000000001/?coliid=I1"></a>
      <span id="itemPrice_I1" class="a-price"><span class="a-offscreen">$12.50</span></span>
      <span id="itemPriority_I1">2</span><span id="itemRequested_I1">2</span><span id="itemPurchased_I1">0</span>
    </li>
    <li data-itemId="I2" data-reposition-action-params="{&quot;itemExternalId&quot;:&quot;ASIN:B000000002|X&quot;}">
      <a id="itemName_I2" title="Already bought" href="/dp/B000000002/"></a>
      <span id="itemRequested_I2">1</span><span id="itemPurchased_I2">1</span>
    </li>
  </ul>
  <a class="wl-see-more" href="/hz/wishlist/slv/items?paginationToken=2">See more</a>
</body></html>"#;

const AMAZON_PAGE_2: &str = r#"<li data-itemId="I3" data-reposition-action-params="{&quot;itemExternalId&quot;:&quot;ASIN:B000000003|X&quot;}">
    <a aria-label="Yo-yo" href="/dp/B000000003/"><img alt="Yo-yo" src="/images/I/yoyo._SS135_.jpg"></a>
    <span class="a-price"><span class="a-offscreen">$3.00</span></span>
  </li><div id="endOfListMarker"></div>"#;

async fn fake_amazon() -> String {
    let app = Router::new()
        .route(
            "/hz/wishlist/ls/L1",
            axum::routing::get(|| async { axum::response::Html(AMAZON_PAGE_1) }),
        )
        .route(
            "/hz/wishlist/slv/items",
            axum::routing::get(|| async { axum::response::Html(AMAZON_PAGE_2) }),
        )
        .route(
            "/hz/wishlist/ls/dead",
            axum::routing::get(|| async {
                axum::response::Html("<title>Amazon.com</title>not a functioning page")
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn amazon_list_import_walks_pages_and_skips_duplicates() {
    let app = test_app().await;
    let amazon = fake_amazon().await;
    let mut c = Client::new(&app);
    c.signup("Sam", "sam@example.com").await;
    let list = c.post("/lists", &[("title", "Wishes")]).await;
    let list_id = id_from_location(&list.location.unwrap(), "/lists/");
    let import = format!("/lists/{list_id}/import");
    let link = format!("{amazon}/hz/wishlist/ls/L1");

    let r = c.post(&import, &[("url", &link)]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let page = c.get(&format!("/lists/{list_id}")).await.body;
    assert!(page.contains("Added 2 wishes from Amazon."), "{page}");
    assert!(page.contains("Left out 1 already bought"));
    assert!(page.contains("Kite") && page.contains("Yo-yo"));
    assert!(!page.contains("Already bought"));
    assert!(page.contains("wants 2"));
    assert!(page.contains("Most wanted"));
    assert!(!page.contains("coliid"), "tracking parameters are stripped");

    // Importing again adds nothing.
    c.post(&import, &[("url", &link)]).await;
    let page = c.get(&format!("/lists/{list_id}")).await.body;
    assert!(page.contains("Added 0 wishes from Amazon. 2 already on this list."));
    assert_eq!(page.matches("id=\"item-").count(), 2);

    // A dead or private list explains itself and adds nothing.
    let r = c
        .post(
            &import,
            &[("url", &format!("{amazon}/hz/wishlist/ls/dead"))],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Public or Shared"));
}

#[tokio::test]
async fn amazon_list_import_only_fetches_amazon() {
    let config = Config {
        allow_private_fetch: false,
        ..Config::for_tests()
    };
    let app = app(AppState::new(config).await.unwrap());
    let mut c = Client::new(&app);
    c.signup("Sam", "sam@example.com").await;
    let list = c.post("/lists", &[("title", "Wishes")]).await;
    let list_id = id_from_location(&list.location.unwrap(), "/lists/");
    let import = format!("/lists/{list_id}/import");
    for bad in [
        "https://example.com/hz/wishlist/ls/L1",
        "https://amazon.evil.example/hz/wishlist/ls/L1",
        "not a link",
    ] {
        let r = c.post(&import, &[("url", bad)]).await;
        assert!(
            r.body.contains("look like an Amazon wish list"),
            "{bad}: {}",
            r.body
        );
    }

    // Other people can't import into your list.
    let mut other = Client::new(&app);
    other.signup("Pat", "pat@example.com").await;
    let r = other.post(&import, &[("url", "https://a.co/d/x")]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}
