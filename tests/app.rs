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

    let r = mom
        .post(
            "/lists",
            &[
                ("title", "Maya's Birthday"),
                ("recipient_name", "Maya"),
                ("event_date", "2030-05-01"),
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
    let r = grandma.post(&format!("/items/{robot_id}/claim"), &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(grandma
        .get("/reservations")
        .await
        .body
        .contains("Robot kit"));

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
            &[("guest_name", "Jamie"), ("quantity", "1")],
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
