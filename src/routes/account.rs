use axum::extract::{Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;

use crate::auth::{self, redirect_flash};
use crate::db::User;
use crate::error::AppResult;
use crate::templates::{render, AccountPage, Ctx, LoginPage, SignupPage};
use crate::util;
use crate::AppState;

#[derive(Deserialize, Default)]
pub struct NextQuery {
    next: Option<String>,
}

/// When the visitor is on their way to accept a family invite, show which family.
async fn invite_family_for(state: &AppState, next: &str) -> Option<String> {
    let token = next.strip_prefix("/join/")?;
    sqlx::query_scalar(
        "SELECT f.name FROM family_invites i JOIN families f ON f.id = i.family_id WHERE i.token = ? AND i.revoked = 0",
    )
    .bind(token)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
}

pub async fn signup_page(State(state): State<AppState>, ctx: Ctx, Query(q): Query<NextQuery>) -> AppResult<Response> {
    let next = util::safe_next(q.next.as_deref());
    if ctx.user.is_some() {
        return Ok(Redirect::to(&next).into_response());
    }
    let invite_family = invite_family_for(&state, &next).await;
    render(SignupPage { ctx, next, email: String::new(), display_name: String::new(), error: None, invite_family })
}

#[derive(Deserialize)]
pub struct SignupForm {
    email: String,
    display_name: String,
    password: String,
    next: Option<String>,
}

pub async fn signup(State(state): State<AppState>, ctx: Ctx, Form(f): Form<SignupForm>) -> AppResult<Response> {
    let next = util::safe_next(f.next.as_deref());
    let email = f.email.trim().to_lowercase();
    let display_name = f.display_name.trim().to_string();
    let error = if !email.contains('@') || email.len() < 3 || email.len() > 254 {
        Some("Please enter a valid email address.")
    } else if display_name.is_empty() || display_name.chars().count() > 60 {
        Some("Please enter your name (up to 60 characters).")
    } else if f.password.chars().count() < 8 {
        Some("Passwords need at least 8 characters.")
    } else {
        None
    };
    let render_error = |msg: &str, ctx: Ctx, invite_family| {
        render(SignupPage { ctx, next: next.clone(), email: email.clone(), display_name: display_name.clone(), error: Some(msg.into()), invite_family })
    };
    if let Some(msg) = error {
        let fam = invite_family_for(&state, &next).await;
        return render_error(msg, ctx, fam);
    }
    let hash = auth::hash_password(&f.password).await?;
    let inserted = sqlx::query_scalar::<_, i64>(
        "INSERT INTO users (email, display_name, password_hash) VALUES (?, ?, ?) RETURNING id",
    )
    .bind(&email)
    .bind(&display_name)
    .bind(hash)
    .fetch_one(&state.db)
    .await;
    let user_id = match inserted {
        Ok(id) => id,
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            let fam = invite_family_for(&state, &next).await;
            return render_error("An account with that email already exists — try signing in.", ctx, fam);
        }
        Err(e) => return Err(e.into()),
    };
    let cookie = auth::start_session(&state, user_id).await?;
    let mut resp = redirect_flash(&next, "ok", &format!("Welcome to Wishful Thinking, {display_name}!"));
    resp.headers_mut().append(SET_COOKIE, cookie);
    Ok(resp)
}

pub async fn login_page(State(state): State<AppState>, ctx: Ctx, Query(q): Query<NextQuery>) -> AppResult<Response> {
    let next = util::safe_next(q.next.as_deref());
    if ctx.user.is_some() {
        return Ok(Redirect::to(&next).into_response());
    }
    let invite_family = invite_family_for(&state, &next).await;
    render(LoginPage { ctx, next, email: String::new(), error: None, invite_family })
}

#[derive(Deserialize)]
pub struct LoginForm {
    email: String,
    password: String,
    next: Option<String>,
}

pub async fn login(State(state): State<AppState>, ctx: Ctx, Form(f): Form<LoginForm>) -> AppResult<Response> {
    let next = util::safe_next(f.next.as_deref());
    let email = f.email.trim().to_lowercase();
    let row: Option<(i64, String)> = sqlx::query_as("SELECT id, password_hash FROM users WHERE email = ?")
        .bind(&email)
        .fetch_optional(&state.db)
        .await?;
    let ok = match &row {
        Some((_, hash)) => auth::verify_password(&f.password, hash).await,
        None => {
            // Spend comparable time for unknown emails to avoid account enumeration by timing.
            let _ = auth::hash_password(&f.password).await;
            false
        }
    };
    if !ok {
        let invite_family = invite_family_for(&state, &next).await;
        return render(LoginPage { ctx, next, email, error: Some("That email and password don't match.".into()), invite_family });
    }
    let cookie = auth::start_session(&state, row.unwrap().0).await?;
    let mut resp = Redirect::to(&next).into_response();
    resp.headers_mut().append(SET_COOKIE, cookie);
    Ok(resp)
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    let cookie = auth::end_session(&state, &headers).await?;
    let mut resp = redirect_flash("/", "ok", "You've been signed out.");
    resp.headers_mut().append(SET_COOKIE, cookie);
    Ok(resp)
}

pub async fn account_page(ctx: Ctx) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    render(AccountPage { ctx, user, error: None })
}

#[derive(Deserialize)]
pub struct AccountForm {
    display_name: String,
    email: String,
}

pub async fn update_account(State(state): State<AppState>, ctx: Ctx, Form(f): Form<AccountForm>) -> AppResult<Response> {
    let user = ctx.require_user()?.clone();
    let name = f.display_name.trim();
    let email = f.email.trim().to_lowercase();
    if name.is_empty() || !email.contains('@') {
        return render(AccountPage { ctx, user, error: Some("Name and a valid email are required.".into()) });
    }
    let res = sqlx::query("UPDATE users SET display_name = ?, email = ? WHERE id = ?")
        .bind(name)
        .bind(&email)
        .bind(user.id)
        .execute(&state.db)
        .await;
    match res {
        Ok(_) => Ok(redirect_flash("/account", "ok", "Profile updated.")),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            render(AccountPage { ctx, user, error: Some("Another account already uses that email.".into()) })
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
pub struct PasswordForm {
    current: String,
    new_password: String,
}

pub async fn change_password(State(state): State<AppState>, ctx: Ctx, Form(f): Form<PasswordForm>) -> AppResult<Response> {
    let user: User = ctx.require_user()?.clone();
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;
    if !auth::verify_password(&f.current, &hash).await {
        return render(AccountPage { ctx, user, error: Some("Your current password is incorrect.".into()) });
    }
    if f.new_password.chars().count() < 8 {
        return render(AccountPage { ctx, user, error: Some("New passwords need at least 8 characters.".into()) });
    }
    let new_hash = auth::hash_password(&f.new_password).await?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(new_hash)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok(redirect_flash("/account", "ok", "Password changed."))
}
