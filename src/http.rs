//! axum HTTP surface. Handlers only touch `Send` data (channel senders + plain
//! request/response types); the `!Send` miden `Client` lives entirely in the
//! worker threads, reached over `mpsc`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use crate::mint::{parse_address, parse_note_type, MintJob, MintOutcome};

/// Upper bound on how long a mint request waits for its worker (local proving can
/// take tens of seconds to minutes). Past this we return 504 rather than hang.
const MINT_TIMEOUT: Duration = Duration::from_secs(300);

/// Token metadata exposed to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct TokenMeta {
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    /// Optional per-request mint cap (base units); enforced on `/api/mint`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_amount: Option<u64>,
}

/// Shared HTTP state. Cheap to clone (everything is `Arc`).
#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<Vec<TokenMeta>>,
    /// symbol -> the faucet worker's request queue.
    pub senders: Arc<HashMap<String, mpsc::Sender<MintJob>>>,
}

/// Build the router: JSON API + health/readiness + static frontend fallback.
pub fn router(state: AppState, static_dir: &str, cors_origins: &[String]) -> Router {
    Router::new()
        .route("/api/tokens", get(list_tokens))
        .route("/api/mint", post(mint))
        .route("/health", get(health))
        .route("/readyz", get(readyz))
        .fallback_service(ServeDir::new(static_dir))
        .layer(cors_layer(cors_origins))
        .with_state(state)
}

/// CORS for cross-origin frontends (e.g. the Amplify-hosted UI calling this API).
/// Empty `origins` adds no allowed origins — same-origin requests are unaffected.
fn cors_layer(origins: &[String]) -> CorsLayer {
    let layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    let parsed: Vec<HeaderValue> = origins.iter().filter_map(|o| o.parse().ok()).collect();
    if parsed.is_empty() {
        layer
    } else {
        layer.allow_origin(parsed)
    }
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
    if let Some(max) = state.tokens.iter().find(|t| t.symbol == req.token).and_then(|t| t.max_amount) {
        if req.amount > max {
            return (
                StatusCode::BAD_REQUEST,
                format!("amount {} exceeds the per-request cap of {} for {}", req.amount, max, req.token),
            )
                .into_response();
        }
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

    match tokio::time::timeout(MINT_TIMEOUT, reply_rx).await {
        Ok(Ok(Ok(outcome))) => (StatusCode::OK, Json::<MintOutcome>(outcome)).into_response(),
        Ok(Ok(Err(e))) => (StatusCode::BAD_GATEWAY, e).into_response(),
        Ok(Err(_)) => {
            (StatusCode::BAD_GATEWAY, "no response from faucet worker".to_string()).into_response()
        }
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            "mint timed out (proving took too long)".to_string(),
        )
            .into_response(),
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
