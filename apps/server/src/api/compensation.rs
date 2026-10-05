use std::sync::Arc;

use axum::{routing::post, Extension, Json, Router};
use wealthfolio_core::compensation::{CompensationEvidence, CompensationEvidenceRequest};
use wealthfolio_core::Error;

use crate::{
    error::{ApiError, ApiResult},
    main_lib::AppState,
};

async fn get_compensation_evidence(
    Extension(state): Extension<Arc<AppState>>,
    Json(request): Json<CompensationEvidenceRequest>,
) -> ApiResult<Json<CompensationEvidence>> {
    state
        .compensation_evidence_service
        .get(request)
        .await
        .map(Json)
        .map_err(|error| match error {
            Error::Validation(_) => ApiError::Core(error),
            _ => ApiError::Internal("Unable to read compensation evidence".into()),
        })
}

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route(
        "/spending/compensation-evidence",
        post(get_compensation_evidence),
    )
}
