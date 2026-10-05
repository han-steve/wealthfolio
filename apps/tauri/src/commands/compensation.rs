use wealthfolio_core::compensation::{CompensationEvidence, CompensationEvidenceRequest};
use wealthfolio_core::Error;

use crate::profiles::ProfileAccess;

#[tauri::command]
pub async fn get_compensation_evidence(
    request: CompensationEvidenceRequest,
    state: ProfileAccess,
) -> Result<CompensationEvidence, String> {
    let context = state.context()?;
    context
        .compensation_evidence_service()
        .get(request)
        .await
        .map_err(|error| match error {
            Error::Validation(_) => error.to_string(),
            _ => "Unable to read compensation evidence".into(),
        })
}
