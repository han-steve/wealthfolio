use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use chrono::Utc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::main_lib::AppState;
use wealthfolio_connect::DEFAULT_CLOUD_API_URL;
use wealthfolio_core::sync::APP_SYNC_TABLES;
use wealthfolio_device_sync::engine::{
    self, CredentialStore, OutboxStore, ReplayEvent, ReplayStore, SnapshotStore, SyncIdentity,
    SyncTransport, TransportError,
};
use wealthfolio_device_sync::{
    DeviceSyncClient, SnapshotRequestPayload, SyncPullResponse, SyncPushRequest, SyncPushResponse,
    SyncState,
};
use wealthfolio_storage_sqlite::sync::SqliteSyncEngineDbPorts;

const SYNC_IDENTITY_KEY: &str = "sync_identity";

#[derive(Debug, Clone)]
pub struct SyncEngineStatusResult {
    pub cursor: i64,
    pub last_push_at: Option<String>,
    pub last_pull_at: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_failures: i32,
    pub next_retry_at: Option<String>,
    pub last_cycle_status: Option<String>,
    pub last_cycle_duration_ms: Option<i64>,
    pub background_running: bool,
    pub bootstrap_required: bool,
}

#[derive(Debug, Clone)]
pub struct SyncBootstrapResult {
    pub status: String,
    pub message: String,
    pub snapshot_id: Option<String>,
    pub cursor: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SyncSnapshotUploadResult {
    pub status: String,
    pub snapshot_id: Option<String>,
    pub oplog_seq: Option<i64>,
    pub message: String,
}

fn cloud_api_base_url() -> String {
    std::env::var("CONNECT_API_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CLOUD_API_URL.to_string())
}

fn create_client() -> DeviceSyncClient {
    DeviceSyncClient::new(&cloud_api_base_url())
}

fn get_sync_identity_from_store(state: &AppState) -> Option<SyncIdentity> {
    let raw = state
        .secret_store
        .get_secret(SYNC_IDENTITY_KEY)
        .ok()
        .flatten()?;
    let identity: wealthfolio_device_sync::SyncIdentity = serde_json::from_str(&raw).ok()?;
    Some(SyncIdentity {
        device_id: identity.device_id,
        root_key: identity.root_key,
        key_version: identity.key_version,
    })
}

async fn persist_device_config_from_identity(
    state: &AppState,
    identity: &SyncIdentity,
    trust_state: &str,
) {
    if let Some(device_id) = &identity.device_id {
        let _ = state
            .app_sync_repository
            .upsert_device_config(
                device_id.clone(),
                identity.key_version,
                trust_state.to_string(),
            )
            .await;
    }
}

fn encrypt_sync_payload(
    plaintext_payload: &str,
    identity: &SyncIdentity,
    payload_key_version: i32,
) -> Result<String, String> {
    let root_key = identity
        .root_key
        .as_ref()
        .ok_or_else(|| "Sync root key is not configured".to_string())?;
    let key_version = payload_key_version.max(1) as u32;
    let dek = wealthfolio_device_sync::crypto::derive_dek(root_key, key_version)
        .map_err(|e| format!("Failed to derive event DEK: {}", e))?;
    wealthfolio_device_sync::crypto::encrypt(&dek, plaintext_payload)
        .map_err(|e| format!("Failed to encrypt sync payload: {}", e))
}

fn decrypt_sync_payload(
    encrypted_payload: &str,
    identity: &SyncIdentity,
    payload_key_version: i32,
) -> Result<String, String> {
    let root_key = identity
        .root_key
        .as_ref()
        .ok_or_else(|| "Sync root key is not configured".to_string())?;
    let key_version = payload_key_version.max(1) as u32;
    let dek = wealthfolio_device_sync::crypto::derive_dek(root_key, key_version)
        .map_err(|e| format!("Failed to derive event DEK: {}", e))?;
    wealthfolio_device_sync::crypto::decrypt(&dek, encrypted_payload)
        .map_err(|e| format!("Failed to decrypt sync payload: {}", e))
}

fn is_sqlite_image(bytes: &[u8]) -> bool {
    bytes.starts_with(b"SQLite format 3\0")
}

fn sha256_checksum(bytes: &[u8]) -> String {
    wealthfolio_device_sync::crypto::sha256_checksum(bytes)
}

fn decode_snapshot_sqlite_payload(
    blob: Vec<u8>,
    identity: &SyncIdentity,
) -> Result<Vec<u8>, String> {
    let root_key = identity
        .root_key
        .as_deref()
        .ok_or("Missing root_key in sync identity")?;
    let key_version = identity
        .key_version
        .ok_or("Missing key_version in sync identity")?;
    if key_version <= 0 {
        return Err("Invalid key version in sync identity".to_string());
    }

    let blob_text = String::from_utf8(blob)
        .map_err(|_| "Snapshot payload is not valid UTF-8 (expected encrypted ciphertext)")?;
    let dek = wealthfolio_device_sync::crypto::derive_dek(root_key, key_version as u32)
        .map_err(|e| format!("Failed to derive snapshot DEK: {}", e))?;
    let decrypted = wealthfolio_device_sync::crypto::decrypt(&dek, blob_text.trim())
        .map_err(|e| format!("Failed to decrypt snapshot payload: {}", e))?;

    let sqlite_bytes = BASE64_STANDARD
        .decode(decrypted.trim())
        .map_err(|e| format!("Failed to base64-decode decrypted snapshot: {}", e))?;
    if !is_sqlite_image(&sqlite_bytes) {
        return Err("Decrypted snapshot is not a valid SQLite image".to_string());
    }
    Ok(sqlite_bytes)
}

struct ServerEnginePorts {
    state: Arc<AppState>,
    db: SqliteSyncEngineDbPorts,
}

impl ServerEnginePorts {
    fn new(state: Arc<AppState>) -> Self {
        let db = SqliteSyncEngineDbPorts::new(Arc::clone(&state.app_sync_repository));
        Self { state, db }
    }
}

#[async_trait]
impl OutboxStore for ServerEnginePorts {
    async fn list_pending_outbox(
        &self,
        limit: i64,
    ) -> Result<Vec<wealthfolio_core::sync::SyncOutboxEvent>, String> {
        self.db.list_pending_outbox(limit).await
    }

    async fn mark_outbox_dead(
        &self,
        event_ids: Vec<String>,
        error_message: Option<String>,
        error_code: Option<String>,
    ) -> Result<(), String> {
        self.db
            .mark_outbox_dead(event_ids, error_message, error_code)
            .await
    }

    async fn mark_outbox_sent(&self, event_ids: Vec<String>) -> Result<(), String> {
        self.db.mark_outbox_sent(event_ids).await
    }

    async fn schedule_outbox_retry(
        &self,
        event_ids: Vec<String>,
        delay_seconds: i64,
        error_message: Option<String>,
        error_code: Option<String>,
    ) -> Result<(), String> {
        self.db
            .schedule_outbox_retry(event_ids, delay_seconds, error_message, error_code)
            .await
    }

    async fn mark_push_completed(&self) -> Result<(), String> {
        self.db.mark_push_completed().await
    }

    async fn has_pending_outbox(&self) -> Result<bool, String> {
        self.db.has_pending_outbox().await
    }
}

#[async_trait]
impl ReplayStore for ServerEnginePorts {
    async fn acquire_cycle_lock(&self) -> Result<i64, String> {
        self.db.acquire_cycle_lock().await
    }

    async fn verify_cycle_lock(&self, lock_version: i64) -> Result<bool, String> {
        self.db.verify_cycle_lock(lock_version).await
    }

    async fn get_cursor(&self) -> Result<i64, String> {
        self.db.get_cursor().await
    }

    async fn set_cursor(&self, cursor: i64) -> Result<(), String> {
        self.db.set_cursor(cursor).await
    }

    async fn apply_remote_events_lww_batch(
        &self,
        events: Vec<ReplayEvent>,
    ) -> Result<usize, String> {
        self.db.apply_remote_events_lww_batch(events).await
    }

    async fn apply_remote_event_lww(&self, event: ReplayEvent) -> Result<bool, String> {
        self.db.apply_remote_event_lww(event).await
    }

    async fn mark_pull_completed(&self) -> Result<(), String> {
        self.db.mark_pull_completed().await
    }

    async fn mark_cycle_outcome(
        &self,
        status: String,
        duration_ms: i64,
        next_retry_at: Option<String>,
    ) -> Result<(), String> {
        self.db
            .mark_cycle_outcome(status, duration_ms, next_retry_at)
            .await
    }

    async fn mark_engine_error(&self, message: String) -> Result<(), String> {
        self.db.mark_engine_error(message).await
    }

    async fn prune_applied_events_up_to_seq(&self, seq: i64) -> Result<(), String> {
        self.db.prune_applied_events_up_to_seq(seq).await
    }

    async fn get_engine_status(&self) -> Result<wealthfolio_core::sync::SyncEngineStatus, String> {
        self.db.get_engine_status().await
    }
}

#[async_trait]
impl SyncTransport for ServerEnginePorts {
    async fn get_events_cursor(
        &self,
        token: &str,
        device_id: &str,
    ) -> Result<wealthfolio_device_sync::SyncCursorResponse, TransportError> {
        create_client()
            .get_events_cursor(token, device_id)
            .await
            .map_err(|e| TransportError {
                message: e.to_string(),
                retry_class: e.retry_class(),
            })
    }

    async fn push_events(
        &self,
        token: &str,
        device_id: &str,
        request: SyncPushRequest,
    ) -> Result<SyncPushResponse, TransportError> {
        create_client()
            .push_events(token, device_id, request)
            .await
            .map_err(|e| TransportError {
                message: e.to_string(),
                retry_class: e.retry_class(),
            })
    }

    async fn pull_events(
        &self,
        token: &str,
        device_id: &str,
        from_cursor: Option<i64>,
        limit: Option<i64>,
    ) -> Result<SyncPullResponse, TransportError> {
        create_client()
            .pull_events(
                token,
                device_id,
                from_cursor,
                limit.map(|value| value as i32),
            )
            .await
            .map_err(|e| TransportError {
                message: e.to_string(),
                retry_class: e.retry_class(),
            })
    }
}

#[async_trait]
impl CredentialStore for ServerEnginePorts {
    fn get_sync_identity(&self) -> Option<SyncIdentity> {
        get_sync_identity_from_store(&self.state)
    }

    fn get_access_token(&self) -> Result<String, String> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(crate::api::connect::mint_access_token(&self.state))
                .map_err(|e| e.to_string())
        })
    }

    async fn get_sync_state(&self) -> Result<SyncState, String> {
        self.state
            .device_enroll_service
            .get_sync_state()
            .await
            .map(|value| value.state)
            .map_err(|err| err.message)
    }

    async fn persist_device_config(&self, identity: &SyncIdentity, trust_state: &str) {
        persist_device_config_from_identity(&self.state, identity, trust_state).await;
    }

    fn encrypt_sync_payload(
        &self,
        plaintext_payload: &str,
        identity: &SyncIdentity,
        payload_key_version: i32,
    ) -> Result<String, String> {
        encrypt_sync_payload(plaintext_payload, identity, payload_key_version)
    }

    fn decrypt_sync_payload(
        &self,
        encrypted_payload: &str,
        identity: &SyncIdentity,
        payload_key_version: i32,
    ) -> Result<String, String> {
        decrypt_sync_payload(encrypted_payload, identity, payload_key_version)
    }
}

#[async_trait]
impl SnapshotStore for ServerEnginePorts {
    async fn maybe_generate_snapshot_for_policy(&self) {
        maybe_generate_snapshot_for_policy(Arc::clone(&self.state)).await;
    }
}

pub async fn get_engine_status(state: &Arc<AppState>) -> Result<SyncEngineStatusResult, String> {
    let status = state
        .app_sync_repository
        .get_engine_status()
        .map_err(|e| e.to_string())?;
    let bootstrap_required = match get_sync_identity_from_store(state).and_then(|i| i.device_id) {
        Some(device_id) => state
            .app_sync_repository
            .needs_bootstrap(&device_id)
            .map_err(|e| e.to_string())?,
        None => true,
    };
    let background_running = state.device_sync_runtime.is_background_running().await;

    Ok(SyncEngineStatusResult {
        cursor: status.cursor,
        last_push_at: status.last_push_at,
        last_pull_at: status.last_pull_at,
        last_error: status.last_error,
        consecutive_failures: status.consecutive_failures,
        next_retry_at: status.next_retry_at,
        last_cycle_status: status.last_cycle_status,
        last_cycle_duration_ms: status.last_cycle_duration_ms,
        background_running,
        bootstrap_required,
    })
}

pub async fn run_sync_cycle(state: Arc<AppState>) -> Result<engine::SyncCycleResult, String> {
    let ports = ServerEnginePorts::new(Arc::clone(&state));
    state.device_sync_runtime.run_cycle(&ports).await
}

pub async fn ensure_background_engine_started(state: Arc<AppState>) -> Result<(), String> {
    if get_sync_identity_from_store(&state).is_none() {
        return Ok(());
    }
    let ports = Arc::new(ServerEnginePorts::new(Arc::clone(&state)));
    state
        .device_sync_runtime
        .ensure_background_started(ports)
        .await;
    Ok(())
}

pub async fn ensure_background_engine_stopped(state: Arc<AppState>) -> Result<(), String> {
    state.device_sync_runtime.ensure_background_stopped().await;
    Ok(())
}

fn snapshot_upload_cancelled_result(message: &str) -> SyncSnapshotUploadResult {
    SyncSnapshotUploadResult {
        status: "cancelled".to_string(),
        snapshot_id: None,
        oplog_seq: None,
        message: message.to_string(),
    }
}

async fn request_snapshot_generation(
    token: &str,
    device_id: &str,
    identity: &SyncIdentity,
    message: &str,
) -> Result<SyncBootstrapResult, String> {
    let payload_key_version = identity.key_version.unwrap_or(1).max(1);
    create_client()
        .request_snapshot(
            token,
            device_id,
            SnapshotRequestPayload {
                min_schema_version: Some(1),
                covers_tables: Some(APP_SYNC_TABLES.iter().map(|v| v.to_string()).collect()),
                payload: encrypt_sync_payload("{}", identity, payload_key_version)?,
                payload_key_version,
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok(SyncBootstrapResult {
        status: "requested".to_string(),
        message: message.to_string(),
        snapshot_id: None,
        cursor: None,
    })
}

pub async fn sync_bootstrap_snapshot_if_needed(
    state: Arc<AppState>,
) -> Result<SyncBootstrapResult, String> {
    let identity = get_sync_identity_from_store(&state)
        .ok_or_else(|| "No sync identity configured. Please enable sync first.".to_string())?;
    let device_id = identity
        .device_id
        .clone()
        .ok_or_else(|| "No device ID configured".to_string())?;
    let token = crate::api::connect::mint_access_token(&state)
        .await
        .map_err(|e| e.to_string())?;

    let sync_state = state
        .device_enroll_service
        .get_sync_state()
        .await
        .map_err(|e| e.message)?;
    if sync_state.state != SyncState::Ready {
        return Ok(SyncBootstrapResult {
            status: "skipped".to_string(),
            message: "Device is not in READY state".to_string(),
            snapshot_id: None,
            cursor: None,
        });
    }
    persist_device_config_from_identity(&state, &identity, "trusted").await;

    let sync_repo = Arc::clone(&state.app_sync_repository);
    if !sync_repo
        .needs_bootstrap(&device_id)
        .map_err(|e| e.to_string())?
    {
        return Ok(SyncBootstrapResult {
            status: "skipped".to_string(),
            message: "Snapshot bootstrap already completed".to_string(),
            snapshot_id: None,
            cursor: Some(sync_repo.get_cursor().map_err(|e| e.to_string())?),
        });
    }

    let latest = match create_client()
        .get_latest_snapshot_with_cursor_fallback(&token, &device_id)
        .await
    {
        Ok(value) => value,
        Err(err) => {
            if err.status_code() == Some(404) {
                sync_repo
                    .mark_bootstrap_complete(device_id, identity.key_version)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(SyncBootstrapResult {
                    status: "skipped".to_string(),
                    message: "First device — no snapshot needed".to_string(),
                    snapshot_id: None,
                    cursor: Some(sync_repo.get_cursor().map_err(|e| e.to_string())?),
                });
            }
            return Err(err.to_string());
        }
    };

    let latest = match latest {
        Some(value) => value,
        None => {
            sync_repo
                .mark_bootstrap_complete(device_id, identity.key_version)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(SyncBootstrapResult {
                status: "skipped".to_string(),
                message: "First device — no snapshot needed".to_string(),
                snapshot_id: None,
                cursor: Some(sync_repo.get_cursor().map_err(|e| e.to_string())?),
            });
        }
    };

    const LOCAL_SCHEMA_VERSION: i32 = 1;
    if latest.schema_version > LOCAL_SCHEMA_VERSION {
        return Err(format!(
            "Snapshot schema version {} is newer than local version {}. Please update the app.",
            latest.schema_version, LOCAL_SCHEMA_VERSION
        ));
    }

    let snapshot_id = latest.snapshot_id.trim().to_string();
    if snapshot_id.is_empty() {
        return request_snapshot_generation(
            &token,
            &device_id,
            &identity,
            "Latest snapshot metadata was invalid. Requested a fresh snapshot.",
        )
        .await;
    }

    let snapshot_oplog_seq = latest.oplog_seq;
    let latest_checksum = if latest.checksum.trim().is_empty() {
        None
    } else {
        Some(latest.checksum)
    };
    let latest_tables = if latest.covers_tables.is_empty() {
        APP_SYNC_TABLES.iter().map(|v| v.to_string()).collect()
    } else {
        latest.covers_tables
    };

    let (headers, blob) = create_client()
        .download_snapshot(&token, &device_id, &snapshot_id)
        .await
        .map_err(|e| e.to_string())?;
    let actual_checksum = sha256_checksum(&blob);
    if headers.checksum != actual_checksum {
        return Err(format!(
            "Snapshot checksum mismatch (download header): expected={}, got={}",
            headers.checksum, actual_checksum
        ));
    }
    if let Some(expected_checksum) = latest_checksum.as_ref() {
        if expected_checksum != &actual_checksum {
            return Err(format!(
                "Snapshot checksum mismatch (latest metadata): expected={}, got={}",
                expected_checksum, actual_checksum
            ));
        }
    }

    let sqlite_image = decode_snapshot_sqlite_payload(blob, &identity)?;
    let temp_snapshot_path =
        std::env::temp_dir().join(format!("wf_snapshot_server_{}.db", Uuid::new_v4()));
    std::fs::write(&temp_snapshot_path, sqlite_image)
        .map_err(|e| format!("Failed to persist snapshot image: {}", e))?;
    let snapshot_path_str = temp_snapshot_path.to_string_lossy().to_string();

    let mut tables_to_restore: Vec<String> = latest_tables
        .iter()
        .filter(|table| APP_SYNC_TABLES.contains(&table.as_str()))
        .map(|table| table.to_string())
        .collect();
    if tables_to_restore.is_empty() {
        tables_to_restore = APP_SYNC_TABLES
            .iter()
            .map(|table| table.to_string())
            .collect();
    }

    let restore_result = sync_repo
        .restore_snapshot_tables_from_file(
            snapshot_path_str,
            tables_to_restore,
            snapshot_oplog_seq,
            device_id,
            identity.key_version,
        )
        .await;
    let _ = std::fs::remove_file(&temp_snapshot_path);
    restore_result.map_err(|e| e.to_string())?;

    Ok(SyncBootstrapResult {
        status: "applied".to_string(),
        message: "Snapshot bootstrap completed".to_string(),
        snapshot_id: Some(snapshot_id),
        cursor: Some(snapshot_oplog_seq),
    })
}

pub async fn generate_snapshot_now(
    state: Arc<AppState>,
) -> Result<SyncSnapshotUploadResult, String> {
    state
        .device_sync_runtime
        .snapshot_upload_cancelled
        .store(false, Ordering::Relaxed);

    let identity = get_sync_identity_from_store(&state)
        .ok_or_else(|| "No sync identity configured. Please enable sync first.".to_string())?;
    let device_id = identity
        .device_id
        .clone()
        .ok_or_else(|| "No device ID configured".to_string())?;
    let key_version = identity.key_version.unwrap_or(1).max(1);
    let token = crate::api::connect::mint_access_token(&state)
        .await
        .map_err(|e| e.to_string())?;

    let sync_state = create_client()
        .get_device(&token, &device_id)
        .await
        .map_err(|e| e.to_string())?;
    if sync_state.trust_state != wealthfolio_device_sync::TrustState::Trusted {
        return Ok(SyncSnapshotUploadResult {
            status: "skipped".to_string(),
            snapshot_id: None,
            oplog_seq: None,
            message: "Current device is not trusted".to_string(),
        });
    }
    if state
        .device_sync_runtime
        .snapshot_upload_cancelled
        .load(Ordering::Relaxed)
    {
        return Ok(snapshot_upload_cancelled_result(
            "Snapshot upload cancelled before export",
        ));
    }

    let sqlite_bytes = state
        .app_sync_repository
        .export_snapshot_sqlite_image(APP_SYNC_TABLES.iter().map(|v| v.to_string()).collect())
        .await
        .map_err(|e| format!("Failed to export snapshot SQLite image: {}", e))?;
    if state
        .device_sync_runtime
        .snapshot_upload_cancelled
        .load(Ordering::Relaxed)
    {
        return Ok(snapshot_upload_cancelled_result(
            "Snapshot upload cancelled after export",
        ));
    }

    let encoded_snapshot = BASE64_STANDARD.encode(sqlite_bytes);
    let encrypted_snapshot_payload =
        encrypt_sync_payload(&encoded_snapshot, &identity, key_version)?;
    let payload = encrypted_snapshot_payload.into_bytes();
    let checksum = sha256_checksum(&payload);
    let metadata_payload = encrypt_sync_payload(
        &serde_json::json!({
            "schemaVersion": 1,
            "coversTables": APP_SYNC_TABLES,
            "generatedAt": Utc::now().to_rfc3339(),
        })
        .to_string(),
        &identity,
        key_version,
    )?;

    let upload_headers = wealthfolio_device_sync::SnapshotUploadHeaders {
        event_id: Some(Uuid::now_v7().to_string()),
        schema_version: 1,
        covers_tables: APP_SYNC_TABLES.iter().map(|v| v.to_string()).collect(),
        size_bytes: payload.len() as i64,
        checksum,
        metadata_payload,
        payload_key_version: key_version,
    };

    let response = create_client()
        .upload_snapshot_with_cancel_flag(
            &token,
            &device_id,
            upload_headers,
            payload,
            Some(&state.device_sync_runtime.snapshot_upload_cancelled),
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok(SyncSnapshotUploadResult {
        status: "uploaded".to_string(),
        snapshot_id: Some(response.snapshot_id),
        oplog_seq: Some(response.oplog_seq),
        message: "Snapshot uploaded".to_string(),
    })
}

pub async fn cancel_snapshot_upload(state: Arc<AppState>) {
    state
        .device_sync_runtime
        .snapshot_upload_cancelled
        .store(true, Ordering::Relaxed);
}

pub async fn maybe_generate_snapshot_for_policy(state: Arc<AppState>) {
    let cursor = match state.app_sync_repository.get_cursor() {
        Ok(value) => value,
        Err(err) => {
            warn!(
                "[DeviceSync] Failed reading cursor for snapshot policy: {}",
                err
            );
            return;
        }
    };

    let now = Utc::now();
    let (due_by_time, due_by_seq, _last_uploaded_cursor) = {
        let policy = state.device_sync_runtime.snapshot_policy.lock().await;
        let due_by_time = policy
            .last_uploaded_at
            .map(|at| (now - at).num_seconds() >= engine::DEVICE_SYNC_SNAPSHOT_INTERVAL_SECS as i64)
            .unwrap_or(true);
        let last_uploaded_cursor = policy.last_uploaded_cursor;
        let due_by_seq = cursor.saturating_sub(last_uploaded_cursor)
            >= engine::DEVICE_SYNC_SNAPSHOT_EVENT_THRESHOLD;
        (due_by_time, due_by_seq, last_uploaded_cursor)
    };

    if !due_by_time && !due_by_seq {
        return;
    }

    match generate_snapshot_now(Arc::clone(&state)).await {
        Ok(result) if result.status == "uploaded" => {
            let mut policy = state.device_sync_runtime.snapshot_policy.lock().await;
            policy.last_uploaded_at = Some(now);
            policy.last_uploaded_cursor = result.oplog_seq.unwrap_or(cursor);
        }
        Ok(_) => {}
        Err(err) => {
            let key_version = get_sync_identity_from_store(&state)
                .and_then(|identity| identity.key_version)
                .unwrap_or(1)
                .max(1);
            info!(
                "[DeviceSync] Snapshot policy upload failed cursor={} key_version={} error={}",
                cursor, key_version, err
            );
        }
    }
}
