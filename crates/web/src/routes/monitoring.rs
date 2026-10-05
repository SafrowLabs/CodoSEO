//! `/monitoring/resume/{token}`: the link in the "Keep monitoring?" email. A GET only shows a
//! "Keep monitoring" button (mail scanners and link previews open every link, so a GET must
//! change nothing); the button POSTs back, and that turns monitoring back on. It does not sign
//! the visitor in; the token is the only credential, it works once, and all it can do is
//! un-pause the account it was issued for.

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use codoseo_store::auth::{TokenPurpose, consume_token, token_is_live};

use crate::auth::session;
use crate::error::AppError;
use crate::render::html;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/monitoring/resume/{token}", get(confirm).post(resume))
}

#[derive(Template)]
#[template(path = "monitoring/resume.html")]
struct ResumePage {
    /// `Confirm` (the button), `Resumed` (done) or `Expired`.
    view: ResumeView,
    token: String,
}

enum ResumeView {
    Confirm,
    Resumed,
    Expired,
}

/// The page behind the emailed link: a button when the token is still good, a friendly dead
/// end otherwise. Reads only.
async fn confirm(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let live = token_is_live(
        &state.pool,
        TokenPurpose::ResumeMonitoring,
        &session::hash(&token),
    )
    .await?;
    if live {
        return Ok(html(&ResumePage {
            view: ResumeView::Confirm,
            token,
        })?
        .into_response());
    }
    Ok((
        StatusCode::GONE,
        html(&ResumePage {
            view: ResumeView::Expired,
            token,
        })?,
    )
        .into_response())
}

async fn resume(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    // Using the token and lifting the pause happen together or not at all.
    let mut tx = state.pool.begin().await?;
    let account = consume_token(
        &mut *tx,
        TokenPurpose::ResumeMonitoring,
        &session::hash(&token),
    )
    .await?
    .and_then(|used| used.account_id);
    let Some(account) = account else {
        return Ok((
            StatusCode::GONE,
            html(&ResumePage {
                view: ResumeView::Expired,
                token,
            })?,
        )
            .into_response());
    };
    codoseo_store::accounts::resume_monitoring(&mut *tx, account).await?;
    tx.commit().await?;
    Ok(html(&ResumePage {
        view: ResumeView::Resumed,
        token,
    })?
    .into_response())
}
