use std::sync::Arc;

use axum::{routing::get, Json, Router};
use serde::Serialize;

use super::{AuthUser, SyncServerState};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserTeam {
    pub id: String,
    pub name: String,
    pub plan: String,
    pub subscription_status: String,
    pub created_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfoResponse {
    pub id: String,
    pub email: String,
    pub full_name: Option<String>,
    pub team_id: String,
    pub team_role: String,
    pub team: UserTeam,
}

#[derive(Serialize)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub features: Vec<String>,
}

#[derive(Serialize)]
pub struct PlansResponse {
    pub plans: Vec<Plan>,
}

async fn get_user_me(auth: AuthUser) -> Json<UserInfoResponse> {
    Json(UserInfoResponse {
        id: auth.user_id.clone(),
        email: auth.email.clone(),
        full_name: None,
        team_id: auth.team_id.clone(),
        team_role: "owner".to_string(),
        team: UserTeam {
            id: auth.team_id.clone(),
            name: "My Team".to_string(),
            plan: "pro".to_string(),
            subscription_status: "active".to_string(),
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        },
    })
}

async fn get_subscription_plans() -> Json<PlansResponse> {
    Json(PlansResponse {
        plans: vec![Plan {
            id: "plan_pro".to_string(),
            name: "Pro".to_string(),
            slug: "pro".to_string(),
            features: vec!["broker_sync".to_string(), "device_sync".to_string()],
        }],
    })
}

pub fn user_router(state: Arc<SyncServerState>) -> Router {
    Router::new()
        .route("/api/v1/user/me", get(get_user_me))
        .route("/api/v1/subscription/plans", get(get_subscription_plans))
        .with_state(state)
}
