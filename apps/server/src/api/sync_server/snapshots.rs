use std::sync::Arc;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
    routing::{get, post},
    Router,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{new_id, now_rfc3339, AuthUser, SyncError, SyncResult, SyncServerState};
use rusqlite::OptionalExtension;

fn sha256_with_prefix(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    format!("sha256:{:x}", digest)
}

// ─── Response Types ───────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotLatestResponse {
    pub snapshot_id: String,
    pub schema_version: i32,
    pub covers_tables: Vec<String>,
    pub oplog_seq: i64,
    pub size_bytes: i64,
    pub checksum: String,
    pub created_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotUploadResponse {
    pub snapshot_id: String,
    pub r2_key: String,
    pub oplog_seq: i64,
    pub created_at: String,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn get_latest_snapshot(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    _headers: HeaderMap,
) -> SyncResult<axum::Json<SnapshotLatestResponse>> {
    let team_id = auth.team_id.clone();
    let snapshot_dir = state.snapshot_dir.clone();

    let rows: Vec<(String, i32, String, i64, i64, String, String)> = state
        .with_db(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, schema_version, covers_tables, oplog_seq, size_bytes, checksum, created_at \
                     FROM sync_snapshots WHERE team_id = ?1 ORDER BY oplog_seq DESC, created_at DESC LIMIT 50",
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;
            let rows = stmt
                .query_map([&team_id], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                })
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| SyncError::Internal(e.to_string()))?);
            }
            Ok(out)
        })
        .await?;

    for (id, schema_version, covers_tables_str, oplog_seq, size_bytes, checksum, created_at) in rows {
        if !checksum.starts_with("sha256:") {
            continue;
        }

        let snapshot_path = format!("{}/{}", snapshot_dir, id);
        if tokio::fs::metadata(&snapshot_path).await.is_err() {
            continue;
        }

        let covers_tables: Vec<String> = covers_tables_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let (resolved_checksum, resolved_size_bytes) = match tokio::fs::read(&snapshot_path).await {
            Ok(data) => (sha256_with_prefix(&data), data.len() as i64),
            Err(_) => (checksum, size_bytes),
        };

        return Ok(axum::Json(SnapshotLatestResponse {
            snapshot_id: id,
            schema_version,
            covers_tables,
            oplog_seq,
            size_bytes: resolved_size_bytes,
            checksum: resolved_checksum,
            created_at,
        }));
    }

    Ok(axum::Json(SnapshotLatestResponse {
        snapshot_id: String::new(),
        schema_version: 1,
        covers_tables: Vec::new(),
        oplog_seq: 0,
        size_bytes: 0,
        checksum: String::new(),
        created_at: String::new(),
    }))
}

async fn download_snapshot(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    _headers: HeaderMap,
    Path(snapshot_id): Path<String>,
) -> SyncResult<Response> {
    let team_id = auth.team_id.clone();
    let snapshot_dir = state.snapshot_dir.clone();
    let snapshot_id_for_path = snapshot_id.clone();

    let (schema_version, covers_tables_str, _stored_checksum) = state
        .with_db(move |conn| {
            let row: Option<(i32, String, String)> = conn
                .query_row(
                    "SELECT schema_version, covers_tables, checksum \
                     FROM sync_snapshots WHERE id = ?1 AND team_id = ?2",
                    rusqlite::params![snapshot_id, team_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            row.ok_or_else(|| SyncError::NotFound("Snapshot not found".to_string()))
        })
        .await?;

    let snapshot_path = format!("{}/{}", snapshot_dir, snapshot_id_for_path);
    let data = tokio::fs::read(&snapshot_path)
        .await
        .map_err(|e| SyncError::Internal(format!("Failed to read snapshot file: {e}")))?;
    let checksum = sha256_with_prefix(&data);

    let mut response = Response::new(Body::from(data));
    *response.status_mut() = StatusCode::OK;
    let h = response.headers_mut();
    h.insert("content-type", HeaderValue::from_static("application/octet-stream"));
    h.insert(
        "x-snapshot-schema-version",
        HeaderValue::from_str(&schema_version.to_string()).unwrap(),
    );
    h.insert(
        "x-snapshot-covers-tables",
        HeaderValue::from_str(&covers_tables_str).unwrap(),
    );
    h.insert(
        "x-snapshot-checksum",
        HeaderValue::from_str(&checksum).unwrap(),
    );
    Ok(response)
}

async fn upload_snapshot(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> SyncResult<axum::Json<SnapshotUploadResponse>> {
    let team_id = auth.team_id.clone();
    let snapshot_dir = state.snapshot_dir.clone();

    let device_id = headers
        .get("x-wf-device-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let schema_version: i32 = headers
        .get("x-snapshot-schema-version")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    let covers_tables = headers
        .get("x-snapshot-covers-tables")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let size_bytes: i64 = headers
        .get("x-snapshot-size-bytes")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(body.len() as i64);

    let provided_checksum = headers
        .get("x-snapshot-checksum")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let metadata_payload = headers
        .get("x-snapshot-metadata-payload")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let payload_key_version: i32 = headers
        .get("x-snapshot-payload-key-version")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    let base_seq: Option<i64> = headers
        .get("x-snapshot-base-seq")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    let event_id = headers
        .get("x-snapshot-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let snapshot_id = event_id.unwrap_or_else(new_id);
    let payload = body.to_vec();
    let computed_checksum = sha256_with_prefix(&payload);
    let checksum = if provided_checksum.is_empty() {
        computed_checksum.clone()
    } else if provided_checksum == computed_checksum {
        provided_checksum
    } else {
        tracing::warn!(
            "[Sync] Snapshot upload checksum mismatch; using computed checksum instead of provided header"
        );
        computed_checksum.clone()
    };

    // Write blob to disk
    let snapshot_path = format!("{}/{}", snapshot_dir, snapshot_id);
    tokio::fs::write(&snapshot_path, &payload).await.map_err(|e| {
        SyncError::Internal(format!("Failed to write snapshot: {e}"))
    })?;

    let _oplog_seq_value = base_seq.unwrap_or_else(|| {
        // Will be updated below from DB
        0
    });

    let snap_id_clone = snapshot_id.clone();
    let created_at_val = now_rfc3339();
    let created_at_clone = created_at_val.clone();

    let oplog_seq = state
        .with_db(move |conn| {
            // Get current max seq for this team
            let max_seq: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM sync_events WHERE team_id = ?1",
                    [&team_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let actual_oplog_seq = if base_seq.is_some() { base_seq.unwrap() } else { max_seq };

            conn.execute(
                "INSERT OR REPLACE INTO sync_snapshots \
                 (id, team_id, device_id, schema_version, covers_tables, oplog_seq, \
                  size_bytes, checksum, metadata_payload, payload_key_version, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    snap_id_clone, team_id, device_id, schema_version, covers_tables,
                    actual_oplog_seq, size_bytes, checksum, metadata_payload,
                    payload_key_version, created_at_clone
                ],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            Ok(actual_oplog_seq)
        })
        .await?;

    Ok(axum::Json(SnapshotUploadResponse {
        snapshot_id: snapshot_id.clone(),
        r2_key: format!("snapshots/local/{}", snapshot_id),
        oplog_seq,
        created_at: created_at_val,
    }))
}

pub fn snapshots_router(state: Arc<SyncServerState>) -> Router {
    Router::new()
        .route("/api/v1/sync/snapshots/latest", get(get_latest_snapshot))
        .route(
            "/api/v1/sync/snapshots/{snapshot_id}",
            get(download_snapshot),
        )
        .route("/api/v1/sync/snapshots/upload", post(upload_snapshot))
        .layer(DefaultBodyLimit::max(128 * 1024 * 1024))
        .with_state(state)
}
