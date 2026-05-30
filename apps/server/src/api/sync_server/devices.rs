use std::sync::Arc;

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::{new_id, now_rfc3339, AuthUser, SyncError, SyncResult, SyncServerState};
use rusqlite::OptionalExtension;

// ─── Request/Response Types ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RegisterDeviceRequest {
    pub device_nonce: String,
    pub display_name: String,
    pub platform: String,
    pub os_version: Option<String>,
    pub app_version: Option<String>,
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
pub struct BootstrapEnrollData {
    pub device_id: String,
    pub e2ee_key_version: i32,
}

#[derive(Serialize)]
pub struct PairEnrollData {
    pub device_id: String,
    pub e2ee_key_version: i32,
    pub require_sas: bool,
    pub pairing_ttl_seconds: i32,
    pub trusted_devices: Vec<TrustedDeviceSummary>,
}

#[derive(Serialize)]
pub struct ReadyEnrollData {
    pub device_id: String,
    pub e2ee_key_version: i32,
    pub trust_state: String,
}

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EnrollResponse {
    Bootstrap(BootstrapEnrollData),
    Pair(PairEnrollData),
    Ready(ReadyEnrollData),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceResponse {
    pub id: String,
    pub user_id: String,
    pub display_name: String,
    pub platform: String,
    pub device_public_key: Option<String>,
    pub trust_state: String,
    pub trusted_key_version: Option<f64>,
    pub os_version: Option<String>,
    pub app_version: Option<String>,
    pub last_seen_at: Option<String>,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct UpdateDeviceRequest {
    pub display_name: Option<String>,
}

#[derive(Serialize)]
pub struct SuccessResponse {
    pub success: bool,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn enroll_device(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Json(body): Json<RegisterDeviceRequest>,
) -> SyncResult<Json<EnrollResponse>> {
    let user_id = auth.user_id.clone();
    let team_id = auth.team_id.clone();
    let _state_clone = state.clone();

    state
        .with_db(move |conn| {
            // Reuse existing device_nonce globally (device_nonce is UNIQUE table-wide).
            // This avoids 500s when a device re-enrolls after team/auth changes.
            let existing: Option<(String, String, Option<f64>, String)> = conn
                .query_row(
                    "SELECT id, trust_state, trusted_key_version, team_id FROM sync_devices WHERE device_nonce = ?1",
                    rusqlite::params![body.device_nonce],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let device_id = if let Some((did, _ts, _tkv, existing_team_id)) = &existing {
                // Keep device row aligned to current auth context in case team ownership changed.
                conn.execute(
                    "UPDATE sync_devices SET team_id = ?1, user_id = ?2, last_seen_at = ?3, display_name = ?4, \
                     platform = ?5, os_version = ?6, app_version = ?7 WHERE id = ?8",
                    rusqlite::params![
                        if existing_team_id != &team_id { &team_id } else { existing_team_id },
                        user_id,
                        now_rfc3339(),
                        body.display_name,
                        body.platform,
                        body.os_version,
                        body.app_version,
                        did
                    ],
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;
                did.clone()
            } else {
                let did = new_id();
                let insert_result = conn.execute(
                    "INSERT INTO sync_devices (id, team_id, user_id, device_nonce, display_name, \
                     platform, os_version, app_version, last_seen_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        did, team_id, user_id, body.device_nonce, body.display_name,
                        body.platform, body.os_version, body.app_version, now_rfc3339()
                    ],
                );

                match insert_result {
                    Ok(_) => did,
                    Err(e) => {
                        // Concurrent enroll requests can race between SELECT and INSERT.
                        // Recover by reusing the row that won the nonce uniqueness race.
                        let err_msg = e.to_string();
                        if err_msg.contains("sync_devices.device_nonce") {
                            let existing_did: String = conn
                                .query_row(
                                    "SELECT id FROM sync_devices WHERE device_nonce = ?1",
                                    rusqlite::params![body.device_nonce],
                                    |row| row.get(0),
                                )
                                .map_err(|e| SyncError::Internal(e.to_string()))?;

                            conn.execute(
                                "UPDATE sync_devices SET team_id = ?1, user_id = ?2, last_seen_at = ?3, \
                                 display_name = ?4, platform = ?5, os_version = ?6, app_version = ?7 \
                                 WHERE id = ?8",
                                rusqlite::params![
                                    team_id,
                                    user_id,
                                    now_rfc3339(),
                                    body.display_name,
                                    body.platform,
                                    body.os_version,
                                    body.app_version,
                                    existing_did
                                ],
                            )
                            .map_err(|e| SyncError::Internal(e.to_string()))?;

                            existing_did
                        } else {
                            return Err(SyncError::Internal(err_msg));
                        }
                    }
                }
            };

            // Get team key info
            let team_key: Option<(i32, String)> = conn
                .query_row(
                    "SELECT key_version, key_state FROM sync_team_keys WHERE team_id = ?1",
                    [&team_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            if let Some((kv, _ks)) = team_key {
                // E2EE is initialized - check if this device is trusted
                let trust_state: String = conn
                    .query_row(
                        "SELECT trust_state FROM sync_devices WHERE id = ?1",
                        [&device_id],
                        |row| row.get(0),
                    )
                    .map_err(|e| SyncError::Internal(e.to_string()))?;

                if trust_state == "trusted" {
                    return Ok(Json(EnrollResponse::Ready(ReadyEnrollData {
                        device_id,
                        e2ee_key_version: kv,
                        trust_state,
                    })));
                }

                // Not trusted - needs to pair with an existing trusted device
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

                Ok(Json(EnrollResponse::Pair(PairEnrollData {
                    device_id,
                    e2ee_key_version: kv,
                    require_sas: false,
                    pairing_ttl_seconds: 300,
                    trusted_devices,
                })))
            } else {
                // No E2EE initialized yet - first device bootstraps
                Ok(Json(EnrollResponse::Bootstrap(BootstrapEnrollData {
                    device_id,
                    e2ee_key_version: 1,
                })))
            }
        })
        .await
}

async fn list_devices(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
) -> SyncResult<Json<Vec<DeviceResponse>>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, user_id, display_name, platform, device_public_key, \
                     trust_state, trusted_key_version, os_version, app_version, \
                     last_seen_at, created_at \
                     FROM sync_devices WHERE team_id = ?1",
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let devices: Vec<DeviceResponse> = stmt
                .query_map([&team_id], |row| {
                    Ok(DeviceResponse {
                        id: row.get(0)?,
                        user_id: row.get(1)?,
                        display_name: row.get(2)?,
                        platform: row.get(3)?,
                        device_public_key: row.get(4)?,
                        trust_state: row.get(5)?,
                        trusted_key_version: row.get(6)?,
                        os_version: row.get(7)?,
                        app_version: row.get(8)?,
                        last_seen_at: row.get(9)?,
                        created_at: row.get(10)?,
                    })
                })
                .map_err(|e| SyncError::Internal(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(Json(devices))
        })
        .await
}

async fn get_device(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path(device_id): Path<String>,
) -> SyncResult<Json<DeviceResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            let device: Option<DeviceResponse> = conn
                .query_row(
                    "SELECT id, user_id, display_name, platform, device_public_key, \
                     trust_state, trusted_key_version, os_version, app_version, \
                     last_seen_at, created_at \
                     FROM sync_devices WHERE id = ?1 AND team_id = ?2",
                    rusqlite::params![device_id, team_id],
                    |row| {
                        Ok(DeviceResponse {
                            id: row.get(0)?,
                            user_id: row.get(1)?,
                            display_name: row.get(2)?,
                            platform: row.get(3)?,
                            device_public_key: row.get(4)?,
                            trust_state: row.get(5)?,
                            trusted_key_version: row.get(6)?,
                            os_version: row.get(7)?,
                            app_version: row.get(8)?,
                            last_seen_at: row.get(9)?,
                            created_at: row.get(10)?,
                        })
                    },
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            device.map(Json).ok_or_else(|| SyncError::NotFound("Device not found".to_string()))
        })
        .await
}

async fn update_device(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path(device_id): Path<String>,
    Json(body): Json<UpdateDeviceRequest>,
) -> SyncResult<Json<SuccessResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            if let Some(name) = body.display_name {
                conn.execute(
                    "UPDATE sync_devices SET display_name = ?1 WHERE id = ?2 AND team_id = ?3",
                    rusqlite::params![name, device_id, team_id],
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;
            }
            Ok(Json(SuccessResponse { success: true }))
        })
        .await
}

async fn delete_device(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path(device_id): Path<String>,
) -> SyncResult<Json<SuccessResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            conn.execute(
                "DELETE FROM sync_devices WHERE id = ?1 AND team_id = ?2",
                rusqlite::params![device_id, team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;
            Ok(Json(SuccessResponse { success: true }))
        })
        .await
}

async fn revoke_device(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    Path(device_id): Path<String>,
) -> SyncResult<Json<SuccessResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            conn.execute(
                "UPDATE sync_devices SET trust_state = 'revoked' WHERE id = ?1 AND team_id = ?2",
                rusqlite::params![device_id, team_id],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;
            Ok(Json(SuccessResponse { success: true }))
        })
        .await
}

pub fn devices_router(state: Arc<SyncServerState>) -> Router {
    Router::new()
        .route(
            "/api/v1/sync/team/devices",
            post(enroll_device).get(list_devices),
        )
        .route(
            "/api/v1/sync/team/devices/{device_id}",
            get(get_device).patch(update_device).delete(delete_device),
        )
        .route(
            "/api/v1/sync/team/devices/{device_id}/revoke",
            post(revoke_device),
        )
        .with_state(state)
}
