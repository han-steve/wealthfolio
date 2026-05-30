use std::sync::Arc;

use axum::{
    extract::State,
    http::HeaderMap,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use base64::Engine as _;
use super::{new_id, now_rfc3339, AuthUser, SyncError, SyncResult, SyncServerState};
use rusqlite::OptionalExtension;

// ─── Request/Response Types ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct InitializeKeysRequest {
    pub device_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustedDeviceSummary {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub last_seen_at: Option<String>,
}

#[derive(Serialize)]
pub struct BootstrapKeysData {
    pub challenge: String,
    pub nonce: String,
    pub key_version: i32,
}

#[derive(Serialize)]
pub struct PairingRequiredData {
    pub e2ee_key_version: i32,
    pub require_sas: bool,
    pub pairing_ttl_seconds: i32,
    pub trusted_devices: Vec<TrustedDeviceSummary>,
}

#[derive(Serialize)]
pub struct ReadyKeysData {
    pub e2ee_key_version: i32,
}

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InitializeKeysResult {
    Bootstrap(BootstrapKeysData),
    PairingRequired(PairingRequiredData),
    Ready(ReadyKeysData),
}

#[derive(Deserialize)]
pub struct CommitInitializeKeysRequest {
    pub device_id: String,
    pub key_version: i32,
    pub device_key_envelope: String,
    pub signature: String,
    pub challenge_response: Option<String>,
    pub recovery_envelope: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitInitializeKeysResponse {
    pub success: bool,
    pub key_state: String,
}

#[derive(Deserialize)]
pub struct RotateKeysRequest {
    pub initiator_device_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RotateKeysResponse {
    pub challenge: String,
    pub nonce: String,
    pub new_key_version: i32,
}

#[derive(Deserialize)]
pub struct DeviceKeyEnvelope {
    pub device_id: String,
    pub device_key_envelope: String,
}

#[derive(Deserialize)]
pub struct CommitRotateKeysRequest {
    pub new_key_version: i32,
    pub envelopes: Vec<DeviceKeyEnvelope>,
    pub signature: String,
    pub challenge_response: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitRotateKeysResponse {
    pub success: bool,
    pub key_version: i32,
}

#[derive(Deserialize)]
pub struct ResetTeamSyncRequest {
    pub reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetTeamSyncResponse {
    pub success: bool,
    pub key_version: i32,
    pub reset_at: Option<String>,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn initialize_keys(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> SyncResult<Json<InitializeKeysResult>> {
    let team_id = auth.team_id.clone();
    let device_id = headers
        .get("x-wf-device-id")
        .and_then(|v| v.to_str().ok())
        .or_else(|| body.get("device_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    state
        .with_db(move |conn| {
            // Check if E2EE is already initialized
            let team_key: Option<(i32, String)> = conn
                .query_row(
                    "SELECT key_version, key_state FROM sync_team_keys WHERE team_id = ?1",
                    [&team_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            if let Some((kv, ks)) = team_key {
                // If bootstrap is still pending (interrupted before commit), return the
                // existing challenge so the device can retry commit without getting stuck
                // in an unrecoverable ORPHANED state.
                if ks == "PENDING" {
                    let (ch, n) = conn
                        .query_row(
                            "SELECT challenge, nonce FROM sync_team_keys WHERE team_id = ?1",
                            [&team_id],
                            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                        )
                        .map_err(|e| SyncError::Internal(e.to_string()))?;
                    return Ok(Json(InitializeKeysResult::Bootstrap(BootstrapKeysData {
                        challenge: ch,
                        nonce: n,
                        key_version: kv,
                    })));
                }

                // Check if device is trusted
                if !device_id.is_empty() {
                    let trust_state: Option<String> = conn
                        .query_row(
                            "SELECT trust_state FROM sync_devices WHERE id = ?1 AND team_id = ?2",
                            rusqlite::params![device_id, team_id],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(|e| SyncError::Internal(e.to_string()))?;

                    if trust_state.as_deref() == Some("trusted") {
                        return Ok(Json(InitializeKeysResult::Ready(ReadyKeysData { e2ee_key_version: kv })));
                    }
                }

                // Pairing required
                let trusted_devices: Vec<TrustedDeviceSummary> = {
                    let mut stmt = conn
                        .prepare(
                            "SELECT id, display_name, platform, last_seen_at \
                             FROM sync_devices WHERE team_id = ?1 AND trust_state = 'trusted'",
                        )
                        .map_err(|e| SyncError::Internal(e.to_string()))?;
                    let mut rows = stmt.query(rusqlite::params![team_id])
                        .map_err(|e| SyncError::Internal(e.to_string()))?;
                    let mut result = Vec::new();
                    while let Some(row) = rows.next().map_err(|e| SyncError::Internal(e.to_string()))? {
                        if let (Ok(id), Ok(name), Ok(platform), Ok(last_seen_at)) = (
                            row.get::<_, String>(0), row.get::<_, String>(1),
                            row.get::<_, String>(2), row.get::<_, Option<String>>(3),
                        ) {
                            result.push(TrustedDeviceSummary { id, name, platform, last_seen_at });
                        }
                    }
                    result
                };

                return Ok(Json(InitializeKeysResult::PairingRequired(PairingRequiredData {
                    e2ee_key_version: kv,
                    require_sas: false,
                    pairing_ttl_seconds: 300,
                    trusted_devices,
                })));
            }

            // No keys yet - ready to bootstrap
            let challenge = base64::engine::general_purpose::STANDARD
                .encode(uuid::Uuid::new_v4().as_bytes());
            let nonce = base64::engine::general_purpose::STANDARD
                .encode(uuid::Uuid::new_v4().as_bytes());

            conn.execute(
                "INSERT OR REPLACE INTO sync_team_keys (team_id, key_version, key_state, challenge, nonce, updated_at) \
                 VALUES (?1, 1, 'PENDING', ?2, ?3, ?4)",
                rusqlite::params![team_id, challenge, nonce, now_rfc3339()],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            let (ch, n) = conn
                .query_row(
                    "SELECT challenge, nonce FROM sync_team_keys WHERE team_id = ?1",
                    [&team_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            Ok(Json(InitializeKeysResult::Bootstrap(BootstrapKeysData {
                challenge: ch,
                nonce: n,
                key_version: 1,
            })))
        })
        .await
}

async fn commit_initialize_keys(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Json(body): Json<CommitInitializeKeysRequest>,
) -> SyncResult<Json<CommitInitializeKeysResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            // Store the device key envelope
            conn.execute(
                "INSERT OR REPLACE INTO sync_device_key_envelopes \
                 (id, team_id, device_id, key_version, device_key_envelope, signature) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    new_id(), team_id, body.device_id, body.key_version,
                    body.device_key_envelope, body.signature
                ],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            // Mark the key as active and the device as trusted
            conn.execute(
                "UPDATE sync_team_keys SET key_state = 'ACTIVE', key_version = ?1, updated_at = ?2 \
                 WHERE team_id = ?3",
                rusqlite::params![body.key_version, now_rfc3339(), team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            conn.execute(
                "UPDATE sync_devices SET trust_state = 'trusted', trusted_key_version = ?1 \
                 WHERE id = ?2 AND team_id = ?3",
                rusqlite::params![body.key_version as f64, body.device_id, team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            Ok(Json(CommitInitializeKeysResponse {
                success: true,
                key_state: "ACTIVE".to_string(),
            }))
        })
        .await
}

async fn rotate_keys(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> SyncResult<Json<RotateKeysResponse>> {
    let team_id = auth.team_id.clone();
    let _initiator = headers
        .get("x-wf-device-id")
        .and_then(|v| v.to_str().ok())
        .or_else(|| body.get("initiator_device_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    state
        .with_db(move |conn| {
            let current_version: i32 = conn
                .query_row(
                    "SELECT key_version FROM sync_team_keys WHERE team_id = ?1",
                    [&team_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?
                .unwrap_or(1);

            let new_version = current_version + 1;
            let challenge = base64::engine::general_purpose::STANDARD
                .encode(uuid::Uuid::new_v4().as_bytes());
            let nonce = base64::engine::general_purpose::STANDARD
                .encode(uuid::Uuid::new_v4().as_bytes());

            conn.execute(
                "INSERT OR REPLACE INTO sync_team_keys \
                 (team_id, key_version, key_state, challenge, nonce, updated_at) \
                 VALUES (?1, ?2, 'PENDING', ?3, ?4, ?5)",
                rusqlite::params![team_id, new_version, challenge, nonce, now_rfc3339()],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            Ok(Json(RotateKeysResponse {
                challenge,
                nonce,
                new_key_version: new_version,
            }))
        })
        .await
}

async fn commit_rotate_keys(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Json(body): Json<CommitRotateKeysRequest>,
) -> SyncResult<Json<CommitRotateKeysResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            // Store envelopes for all devices
            for env in &body.envelopes {
                conn.execute(
                    "INSERT OR REPLACE INTO sync_device_key_envelopes \
                     (id, team_id, device_id, key_version, device_key_envelope, signature) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        new_id(), team_id, env.device_id, body.new_key_version,
                        env.device_key_envelope, body.signature
                    ],
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;

                // Update device's trusted key version
                conn.execute(
                    "UPDATE sync_devices SET trust_state = 'trusted', trusted_key_version = ?1 \
                     WHERE id = ?2 AND team_id = ?3",
                    rusqlite::params![body.new_key_version as f64, env.device_id, team_id],
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;
            }

            conn.execute(
                "UPDATE sync_team_keys SET key_state = 'ACTIVE', key_version = ?1, updated_at = ?2 \
                 WHERE team_id = ?3",
                rusqlite::params![body.new_key_version, now_rfc3339(), team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            Ok(Json(CommitRotateKeysResponse {
                success: true,
                key_version: body.new_key_version,
            }))
        })
        .await
}

async fn reset_team_sync(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Json(_body): Json<serde_json::Value>,
) -> SyncResult<Json<ResetTeamSyncResponse>> {
    let team_id = auth.team_id.clone();
    let reset_at = now_rfc3339();

    state
        .with_db(move |conn| {
            // Reset all devices to untrusted
            conn.execute(
                "UPDATE sync_devices SET trust_state = 'revoked', trusted_key_version = NULL \
                 WHERE team_id = ?1",
                [&team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            // Delete old key envelopes
            conn.execute(
                "DELETE FROM sync_device_key_envelopes WHERE team_id = ?1",
                [&team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            // Delete team keys so the next device can bootstrap fresh
            conn.execute(
                "DELETE FROM sync_team_keys WHERE team_id = ?1",
                [&team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            Ok(Json(ResetTeamSyncResponse {
                success: true,
                key_version: 1,
                reset_at: Some(reset_at),
            }))
        })
        .await
}

pub fn keys_router(state: Arc<SyncServerState>) -> Router {
    Router::new()
        .route("/api/v1/sync/team/keys/initialize", post(initialize_keys))
        .route(
            "/api/v1/sync/team/keys/initialize/commit",
            post(commit_initialize_keys),
        )
        .route("/api/v1/sync/team/keys/rotate", post(rotate_keys))
        .route(
            "/api/v1/sync/team/keys/rotate/commit",
            post(commit_rotate_keys),
        )
        .route("/api/v1/sync/team/keys/reset", post(reset_team_sync))
        .with_state(state)
}
