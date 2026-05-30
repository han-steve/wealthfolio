use axum::{routing::get, Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;

use super::SyncServerState;

async fn list_connections() -> Json<Value> {
    Json(json!({ "connections": [] }))
}

async fn list_accounts() -> Json<Value> {
    Json(json!({ "accounts": [] }))
}

async fn get_activities() -> Json<Value> {
    Json(json!({ "data": [], "pagination": null }))
}

async fn get_holdings() -> Json<Value> {
    Json(json!({}))
}

pub fn brokerage_router(state: Arc<SyncServerState>) -> Router {
    Router::new()
        .route("/api/v1/sync/brokerage/connections", get(list_connections))
        .route("/api/v1/sync/brokerage/accounts", get(list_accounts))
        .route(
            "/api/v1/sync/brokerage/accounts/{account_id}/activities",
            get(get_activities),
        )
        .route(
            "/api/v1/sync/brokerage/accounts/{account_id}/holdings",
            get(get_holdings),
        )
        .with_state(state)
}
