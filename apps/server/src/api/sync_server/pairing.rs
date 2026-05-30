use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::{new_id, AuthUser, SyncError, SyncResult, SyncServerState};
use rusqlite::OptionalExtension;

// ─── Request/Response Types ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreatePairingRequest {
    pub code_hash: String,
    pub ephemeral_public_key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePairingResponse {
    pub pairing_id: String,
    pub expires_at: String,
    pub key_version: i32,
    pub require_sas: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPairingResponse {
    pub pairing_id: String,
    pub status: String,
    pub claimer_device_id: Option<String>,
    pub claimer_ephemeral_pub: Option<String>,
    pub expires_at: String,
}

#[derive(Deserialize)]
pub struct CompletePairingRequest {
    pub encrypted_key_bundle: String,
    pub sas_proof: serde_json::Value,
    pub signature: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletePairingResponse {
    pub success: bool,
    pub remote_seed_present: Option<bool>,
}

#[derive(Deserialize)]
pub struct ClaimPairingRequest {
    pub code: String,
    pub ephemeral_public_key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimPairingResponse {
    pub session_id: String,
    pub issuer_ephemeral_pub: String,
    pub e2ee_key_version: i32,
    pub require_sas: bool,
    pub expires_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingMessage {
    pub id: String,
    pub payload_type: String,
    pub payload: String,
    pub created_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingMessagesResponse {
    pub session_status: String,
    pub messages: Vec<PairingMessage>,
}

#[derive(Deserialize)]
pub struct ConfirmPairingRequest {
    pub proof: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmPairingResponse {
    pub success: bool,
    pub key_version: i32,
    pub remote_seed_present: Option<bool>,
}

#[derive(Serialize)]
pub struct SuccessResponse {
    pub success: bool,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn create_pairing(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path(device_id): Path<String>,
    Json(body): Json<CreatePairingRequest>,
) -> SyncResult<Json<CreatePairingResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            // Verify issuer device is trusted
            let trust_state: Option<String> = conn
                .query_row(
                    "SELECT trust_state FROM sync_devices WHERE id = ?1 AND team_id = ?2",
                    rusqlite::params![device_id, team_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            if trust_state.as_deref() != Some("trusted") {
                return Err(SyncError::BadRequest("Device is not trusted".to_string()));
            }

            let key_version: i32 = conn
                .query_row(
                    "SELECT key_version FROM sync_team_keys WHERE team_id = ?1",
                    [&team_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?
                .unwrap_or(1);

            let pairing_id = new_id();
            let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(5))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

            conn.execute(
                "INSERT INTO sync_pairings \
                 (id, team_id, issuer_device_id, code_hash, ephemeral_public_key, \
                  status, key_version, require_sas, expires_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, 0, ?7)",
                rusqlite::params![
                    pairing_id, team_id, device_id, body.code_hash,
                    body.ephemeral_public_key, key_version, expires_at
                ],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            Ok(Json(CreatePairingResponse {
                pairing_id,
                expires_at,
                key_version,
                require_sas: false,
            }))
        })
        .await
}

async fn get_pairing(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path((_device_id, pairing_id)): Path<(String, String)>,
) -> SyncResult<Json<GetPairingResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            let row: Option<(String, Option<String>, Option<String>, String)> = conn
                .query_row(
                    "SELECT status, claimer_device_id, claimer_ephemeral_pub, expires_at \
                     FROM sync_pairings WHERE id = ?1 AND team_id = ?2",
                    rusqlite::params![pairing_id, team_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let (status, claimer_device_id, claimer_ephemeral_pub, expires_at) =
                row.ok_or_else(|| SyncError::NotFound("Pairing not found".to_string()))?;

            Ok(Json(GetPairingResponse {
                pairing_id,
                status,
                claimer_device_id,
                claimer_ephemeral_pub,
                expires_at,
            }))
        })
        .await
}

async fn approve_pairing(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path((_device_id, pairing_id)): Path<(String, String)>,
) -> SyncResult<Json<SuccessResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            conn.execute(
                "UPDATE sync_pairings SET status = 'approved' \
                 WHERE id = ?1 AND team_id = ?2 AND status = 'claimed'",
                rusqlite::params![pairing_id, team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;
            Ok(Json(SuccessResponse { success: true }))
        })
        .await
}

async fn complete_pairing(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path((_device_id, pairing_id)): Path<(String, String)>,
    Json(body): Json<CompletePairingRequest>,
) -> SyncResult<Json<CompletePairingResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            // Get pairing and claimer device ID
            let row: Option<(String, Option<String>)> = conn
                .query_row(
                    "SELECT status, claimer_device_id FROM sync_pairings \
                     WHERE id = ?1 AND team_id = ?2",
                    rusqlite::params![pairing_id, team_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let (status, claimer_device_id) =
                row.ok_or_else(|| SyncError::NotFound("Pairing not found".to_string()))?;

            if !matches!(status.as_str(), "claimed" | "approved") {
                return Err(SyncError::BadRequest(
                    "Pairing is not in claimed/approved state".to_string(),
                ));
            }

            let _claimer_id = claimer_device_id
                .ok_or_else(|| SyncError::BadRequest("No claimer device".to_string()))?;

            // Store the encrypted key bundle as a message for the claimer
            conn.execute(
                "INSERT INTO sync_pairing_messages (id, pairing_id, payload_type, payload) \
                 VALUES (?1, ?2, 'rk_transfer_v1', ?3)",
                rusqlite::params![new_id(), pairing_id, body.encrypted_key_bundle],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            conn.execute(
                "UPDATE sync_pairings SET status = 'completed' WHERE id = ?1",
                [&pairing_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            // Check if there's a snapshot
            let has_snapshot: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sync_snapshots WHERE team_id = ?1",
                    [&team_id],
                    |row| row.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);

            Ok(Json(CompletePairingResponse {
                success: true,
                remote_seed_present: Some(has_snapshot),
            }))
        })
        .await
}

async fn cancel_pairing(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path((_device_id, pairing_id)): Path<(String, String)>,
) -> SyncResult<Json<SuccessResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            conn.execute(
                "UPDATE sync_pairings SET status = 'cancelled' WHERE id = ?1 AND team_id = ?2",
                rusqlite::params![pairing_id, team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;
            Ok(Json(SuccessResponse { success: true }))
        })
        .await
}

async fn claim_pairing(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path(claimer_device_id): Path<String>,
    Json(body): Json<ClaimPairingRequest>,
) -> SyncResult<Json<ClaimPairingResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            // Find pairing by code hash (SHA-256 of the code)
            let code_hash = sha256_hex(body.code.as_bytes());

            let row: Option<(String, String, i32, String)> = conn
                .query_row(
                    "SELECT id, ephemeral_public_key, key_version, expires_at \
                     FROM sync_pairings \
                     WHERE code_hash = ?1 AND team_id = ?2 AND status = 'open' \
                     AND expires_at > strftime('%Y-%m-%dT%H:%M:%SZ','now') \
                     LIMIT 1",
                    rusqlite::params![code_hash, team_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let (pairing_id, issuer_ephemeral_pub, key_version, expires_at) =
                row.ok_or_else(|| SyncError::NotFound("Invalid or expired pairing code".to_string()))?;

            // Store claimer info and mark as claimed
            conn.execute(
                "UPDATE sync_pairings \
                 SET status = 'claimed', claimer_device_id = ?1, claimer_ephemeral_pub = ?2 \
                 WHERE id = ?3",
                rusqlite::params![claimer_device_id, body.ephemeral_public_key, pairing_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            Ok(Json(ClaimPairingResponse {
                session_id: pairing_id,
                issuer_ephemeral_pub,
                e2ee_key_version: key_version,
                require_sas: false,
                expires_at,
            }))
        })
        .await
}

async fn get_pairing_messages(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path((_claimer_device_id, pairing_id)): Path<(String, String)>,
) -> SyncResult<Json<PairingMessagesResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            let status: String = conn
                .query_row(
                    "SELECT status FROM sync_pairings WHERE id = ?1 AND team_id = ?2",
                    rusqlite::params![pairing_id, team_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?
                .ok_or_else(|| SyncError::NotFound("Pairing not found".to_string()))?;

            let mut stmt = conn
                .prepare(
                    "SELECT id, payload_type, payload, created_at \
                     FROM sync_pairing_messages WHERE pairing_id = ?1",
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let messages: Vec<PairingMessage> = stmt
                .query_map([&pairing_id], |row| {
                    Ok(PairingMessage {
                        id: row.get(0)?,
                        payload_type: row.get(1)?,
                        payload: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                })
                .map_err(|e| SyncError::Internal(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(Json(PairingMessagesResponse { session_status: status, messages }))
        })
        .await
}

async fn confirm_pairing(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path((claimer_device_id, pairing_id)): Path<(String, String)>,
    Json(_body): Json<ConfirmPairingRequest>,
) -> SyncResult<Json<ConfirmPairingResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            let (key_version, stored_claimer_id): (i32, Option<String>) = conn
                .query_row(
                    "SELECT key_version, claimer_device_id FROM sync_pairings WHERE id = ?1 AND team_id = ?2",
                    rusqlite::params![pairing_id, team_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?
                .ok_or_else(|| SyncError::NotFound("Pairing not found".to_string()))?;

            // Verify the URL device_id matches who claimed this pairing
            if stored_claimer_id.as_deref() != Some(claimer_device_id.as_str()) {
                return Err(SyncError::BadRequest("Device is not the claimer of this pairing".to_string()));
            }

            // Mark claimer device as trusted at this key version
            conn.execute(
                "UPDATE sync_devices SET trust_state = 'trusted', trusted_key_version = ?1 \
                 WHERE id = ?2 AND team_id = ?3",
                rusqlite::params![key_version as f64, claimer_device_id, team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            let has_snapshot: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sync_snapshots WHERE team_id = ?1",
                    [&team_id],
                    |row| row.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .unwrap_or(false);

            Ok(Json(ConfirmPairingResponse {
                success: true,
                key_version,
                remote_seed_present: Some(has_snapshot),
            }))
        })
        .await
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(data);
    format!("{:x}", hash)
}

pub fn pairing_router(state: Arc<SyncServerState>) -> Router {
    Router::new()
        .route(
            "/api/v1/sync/team/devices/{device_id}/pairings",
            post(create_pairing),
        )
        .route(
            "/api/v1/sync/team/devices/{device_id}/pairings/{pairing_id}",
            get(get_pairing),
        )
        .route(
            "/api/v1/sync/team/devices/{device_id}/pairings/{pairing_id}/approve",
            post(approve_pairing),
        )
        .route(
            "/api/v1/sync/team/devices/{device_id}/pairings/{pairing_id}/complete",
            post(complete_pairing),
        )
        .route(
            "/api/v1/sync/team/devices/{device_id}/pairings/{pairing_id}/cancel",
            post(cancel_pairing),
        )
        .route(
            "/api/v1/sync/team/devices/{device_id}/pairings/claim",
            post(claim_pairing),
        )
        .route(
            "/api/v1/sync/team/devices/{device_id}/pairings/{pairing_id}/messages",
            get(get_pairing_messages),
        )
        .route(
            "/api/v1/sync/team/devices/{device_id}/pairings/{pairing_id}/confirm",
            post(confirm_pairing),
        )
        .with_state(state)
}
