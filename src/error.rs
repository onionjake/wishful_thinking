use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("forbidden")]
    Forbidden,
    /// The visitor must sign in; carries the path to return to.
    #[error("login required")]
    LoginRequired(String),
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Template(#[from] askama::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage {
    ctx: crate::templates::Ctx,
    status: u16,
    heading: String,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, heading, message) = match &self {
            AppError::LoginRequired(next) => {
                return Redirect::to(&format!("/login?next={}", crate::util::urlencode(next))).into_response();
            }
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found", "We couldn't find that page. It may have been deleted, or the link may be wrong.".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "No access", "You don't have access to this. If someone shared it with you, ask them to add you to their family.".to_string()),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, "That didn't work", m.clone()),
            other => {
                tracing::error!(error = %other, "request failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", "An unexpected error occurred. Please try again.".to_string())
            }
        };
        let page = ErrorPage {
            ctx: crate::templates::Ctx::anonymous(),
            status: status.as_u16(),
            heading: heading.to_string(),
            message,
        };
        match page.render() {
            Ok(html) => (status, Html(html)).into_response(),
            Err(_) => (status, heading.to_string()).into_response(),
        }
    }
}
