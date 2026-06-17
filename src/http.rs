//! axum HTTP surface. Handlers only touch `Send` data (channel senders + plain
//! request/response types); the `!Send` miden `Client` lives entirely in the
//! worker threads, reached over `mpsc`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tower_http::services::ServeDir;

use crate::mint::{parse_address, parse_note_type, MintJob, MintOutcome};

/// Token metadata exposed to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct TokenMeta {
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
}

/// Shared HTTP state. Cheap to clone (everything is `Arc`).
#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<Vec<TokenMeta>>,
    /// symbol -> the faucet worker's request queue.
    pub senders: Arc<HashMap<String, mpsc::Sender<MintJob>>>,
}

/// Build the router: JSON API + health/readiness + static frontend fallback.
pub fn router(state: AppState, static_dir: &str) -> Router {
    Router::new()
        .route("/api/tokens", get(list_tokens))
        .route("/api/mint", post(mint))
        .route("/health", get(health))
        .route("/readyz", get(readyz))
        .fallback_service(ServeDir::new(static_dir))
        .with_state(state)
}

async fn list_tokens(State(state): State<AppState>) -> Json<Vec<TokenMeta>> {
    Json((*state.tokens).clone())
}

#[derive(Debug, Deserialize)]
struct MintRequest {
    token: String,
    address: String,
    amount: u64,
    #[serde(default = "default_note_type")]
    note_type: String,
}

fn default_note_type() -> String {
    "public".to_string()
}

async fn mint(State(state): State<AppState>, Json(req): Json<MintRequest>) -> Response {
    let Some(sender) = state.senders.get(&req.token) else {
        return (StatusCode::NOT_FOUND, format!("unknown token {:?}", req.token)).into_response();
    };
    if req.amount == 0 {
        return (StatusCode::BAD_REQUEST, "amount must be greater than 0".to_string()).into_response();
    }
    let target = match parse_address(&req.address) {
        Ok(target) => target,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let note_type = match parse_note_type(&req.note_type) {
        Ok(note_type) => note_type,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let job = MintJob { target, amount: req.amount, note_type, reply: reply_tx };
    if sender.send(job).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "faucet worker unavailable".to_string())
            .into_response();
    }

    match reply_rx.await {
        Ok(Ok(outcome)) => (StatusCode::OK, Json::<MintOutcome>(outcome)).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Err(_) => {
            (StatusCode::BAD_GATEWAY, "no response from faucet worker".to_string()).into_response()
        }
    }
}

async fn health() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<AppState>) -> (StatusCode, &'static str) {
    if state.senders.values().any(|s| s.is_closed()) {
        (StatusCode::SERVICE_UNAVAILABLE, "a faucet worker is down")
    } else {
        (StatusCode::OK, "ready")
    }
}
