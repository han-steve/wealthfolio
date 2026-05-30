use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::{AuthUser, SyncError, SyncResult, SyncServerState};
use rusqlite::OptionalExtension;

// ─── Request/Response Types ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SyncPushEventRequest {
    pub event_id: String,
    pub device_id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub entity: String,
    pub entity_id: String,
    pub client_timestamp: String,
    pub payload: String,
    pub payload_key_version: i32,
}

#[derive(Deserialize)]
pub struct SyncPushRequest {
    pub events: Vec<SyncPushEventRequest>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPushResultItem {
    pub event_id: String,
    pub seq: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPushResponse {
    pub accepted: Vec<SyncPushResultItem>,
    pub duplicate: Vec<SyncPushResultItem>,
    pub server_cursor: i64,
}

#[derive(Deserialize)]
pub struct PullQuery {
    pub since: Option<i64>,
    pub limit: Option<i32>,
}

#[derive(Deserialize)]
pub struct ReconcileQuery {
    pub cursor: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncEvent {
    pub event_id: String,
    pub device_id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub entity: String,
    pub entity_id: String,
    pub client_timestamp: String,
    pub payload: String,
    pub payload_key_version: i32,
    pub seq: i64,
    pub user_id: String,
    pub team_id: String,
    pub server_timestamp: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPullResponse {
    pub from: i64,
    pub to: i64,
    pub next_cursor: i64,
    pub has_more: bool,
    pub events: Vec<SyncEvent>,
    #[serde(rename = "gc_watermark")]
    pub gc_watermark: Option<i64>,
    pub latest_snapshot_seq: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncLatestSnapshotRef {
    pub snapshot_id: String,
    pub schema_version: i32,
    pub oplog_seq: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncCursorResponse {
    pub cursor: i64,
    pub gc_watermark: Option<i64>,
    pub latest_snapshot: Option<SyncLatestSnapshotRef>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileReadyStateResponse {
    pub action: String,
    pub cursor: Option<i64>,
    pub latest_snapshot: Option<SyncLatestSnapshotRef>,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn push_events(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(body): Json<SyncPushRequest>,
) -> SyncResult<Json<SyncPushResponse>> {
    let team_id = auth.team_id.clone();
    let user_id = auth.user_id.clone();
    let device_id_header = headers
        .get("x-wf-device-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    state
        .with_db(move |conn| {
            let mut accepted = Vec::new();
            let mut duplicate = Vec::new();

            for event in &body.events {
                let effective_device_id = if event.device_id.is_empty() {
                    device_id_header.as_str()
                } else {
                    event.device_id.as_str()
                };

                // Check for duplicate
                let existing_seq: Option<i64> = conn
                    .query_row(
                        "SELECT seq FROM sync_events WHERE event_id = ?1 AND team_id = ?2",
                        rusqlite::params![event.event_id, team_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|e| SyncError::Internal(e.to_string()))?;

                if let Some(seq) = existing_seq {
                    duplicate.push(SyncPushResultItem { event_id: event.event_id.clone(), seq });
                    continue;
                }

                conn.execute(
                    "INSERT INTO sync_events \
                     (event_id, team_id, user_id, device_id, event_type, entity, entity_id, \
                      client_timestamp, payload, payload_key_version) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        event.event_id, team_id, user_id, effective_device_id,
                        event.event_type, event.entity, event.entity_id,
                        event.client_timestamp, event.payload, event.payload_key_version
                    ],
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;

                let seq = conn.last_insert_rowid();
                accepted.push(SyncPushResultItem { event_id: event.event_id.clone(), seq });
            }

            let server_cursor: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM sync_events WHERE team_id = ?1",
                    [&team_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            Ok(Json(SyncPushResponse { accepted, duplicate, server_cursor }))
        })
        .await
}

async fn pull_events(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    _headers: HeaderMap,
    Query(query): Query<PullQuery>,
) -> SyncResult<Json<SyncPullResponse>> {
    let team_id = auth.team_id.clone();
    let since = query.since.unwrap_or(0);
    let limit = query.limit.unwrap_or(500).min(1000) as i64;

    state
        .with_db(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT event_id, device_id, event_type, entity, entity_id, \
                     client_timestamp, payload, payload_key_version, seq, user_id, team_id, server_timestamp \
                     FROM sync_events WHERE team_id = ?1 AND seq > ?2 \
                     ORDER BY seq ASC LIMIT ?3",
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let events: Vec<SyncEvent> = stmt
                .query_map(rusqlite::params![team_id, since, limit], |row| {
                    Ok(SyncEvent {
                        event_id: row.get(0)?,
                        device_id: row.get(1)?,
                        event_type: row.get(2)?,
                        entity: row.get(3)?,
                        entity_id: row.get(4)?,
                        client_timestamp: row.get(5)?,
                        payload: row.get(6)?,
                        payload_key_version: row.get(7)?,
                        seq: row.get(8)?,
                        user_id: row.get(9)?,
                        team_id: row.get(10)?,
                        server_timestamp: row.get(11)?,
                    })
                })
                .map_err(|e| SyncError::Internal(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            let total_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sync_events WHERE team_id = ?1 AND seq > ?2",
                    rusqlite::params![team_id, since],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let has_more = total_count > limit;
            let next_cursor = events.last().map(|e| e.seq).unwrap_or(since);
            let to = next_cursor;

            // Latest snapshot seq for GC hints
            let latest_snapshot_seq: Option<i64> = conn
                .query_row(
                    "SELECT oplog_seq FROM sync_snapshots WHERE team_id = ?1 \
                     ORDER BY oplog_seq DESC LIMIT 1",
                    [&team_id],
                    |row| row.get(0),
                )
                .optional()
                .unwrap_or(None);

            Ok(Json(SyncPullResponse {
                from: since,
                to,
                next_cursor,
                has_more,
                events,
                // Self-hosted server retains all events (no GC), so there is no
                // real watermark below which events are missing. Returning None
                // prevents the sync engine from triggering a false stale_cursor
                // for devices that are simply behind the latest snapshot.
                gc_watermark: None,
                latest_snapshot_seq,
            }))
        })
        .await
}

async fn get_cursor(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    _headers: HeaderMap,
) -> SyncResult<Json<SyncCursorResponse>> {
    let team_id = auth.team_id.clone();

    state
        .with_db(move |conn| {
            let cursor: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM sync_events WHERE team_id = ?1",
                    [&team_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let latest_snapshot: Option<SyncLatestSnapshotRef> = conn
                .query_row(
                    "SELECT id, schema_version, oplog_seq FROM sync_snapshots \
                     WHERE team_id = ?1 ORDER BY oplog_seq DESC LIMIT 1",
                    [&team_id],
                    |row| {
                        Ok(SyncLatestSnapshotRef {
                            snapshot_id: row.get(0)?,
                            schema_version: row.get(1)?,
                            oplog_seq: row.get(2)?,
                        })
                    },
                )
                .optional()
                .unwrap_or(None);

            Ok(Json(SyncCursorResponse {
                cursor,
                // Self-hosted server retains all events (no GC), so gc_watermark
                // is always None to avoid misleading clients about event retention.
                gc_watermark: None,
                latest_snapshot,
            }))
        })
        .await
}

async fn reconcile_ready_state(
    State(state): State<Arc<SyncServerState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(params): Query<ReconcileQuery>,
) -> SyncResult<Json<ReconcileReadyStateResponse>> {
    let team_id = auth.team_id.clone();
    let device_cursor: Option<i64> = params.cursor.or_else(|| {
        headers
            .get("x-wf-device-cursor")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
    });

    state
        .with_db(move |conn| {
            let server_cursor: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM sync_events WHERE team_id = ?1",
                    [&team_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let latest_snapshot: Option<SyncLatestSnapshotRef> = conn
                .query_row(
                    "SELECT id, schema_version, oplog_seq FROM sync_snapshots \
                     WHERE team_id = ?1 ORDER BY oplog_seq DESC LIMIT 1",
                    [&team_id],
                    |row| {
                        Ok(SyncLatestSnapshotRef {
                            snapshot_id: row.get(0)?,
                            schema_version: row.get(1)?,
                            oplog_seq: row.get(2)?,
                        })
                    },
                )
                .optional()
                .unwrap_or(None);

            // Decide action based on device's cursor vs server state.
            // New clients send ?cursor=N explicitly; old clients omit it (fall back to
            // server_cursor for backward compatibility — same behaviour as before this change).
            let action = if server_cursor == 0 && latest_snapshot.is_none() {
                "NOOP"
            } else if let Some(ref snap) = latest_snapshot {
                let needs_bootstrap = match device_cursor {
                    Some(c) => c < snap.oplog_seq,
                    None => server_cursor <= snap.oplog_seq,
                };
                if needs_bootstrap { "BOOTSTRAP_SNAPSHOT" } else { "PULL_TAIL" }
            } else {
                "PULL_TAIL"
            };

            Ok(Json(ReconcileReadyStateResponse {
                action: action.to_string(),
                cursor: Some(server_cursor),
                latest_snapshot,
            }))
        })
        .await
}

pub fn events_router(state: Arc<SyncServerState>) -> Router {
    Router::new()
        .route("/api/v1/sync/events/push", post(push_events))
        .route("/api/v1/sync/events/pull", get(pull_events))
        .route("/api/v1/sync/events/cursor", get(get_cursor))
        .route(
            "/api/v1/sync/events/reconcile-ready-state",
            get(reconcile_ready_state),
        )
        .with_state(state)
}
