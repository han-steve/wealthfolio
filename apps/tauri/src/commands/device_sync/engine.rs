//! Tauri adapter for device sync engine orchestration.

use std::sync::Arc;

use async_trait::async_trait;

use crate::context::ServiceContext;
use wealthfolio_device_sync::engine::{
    CredentialStore, OutboxStore, ReplayEvent, ReplayStore, SyncIdentity, SyncTransport,
    TransportError,
};
use wealthfolio_device_sync::{SyncPullResponse, SyncPushRequest, SyncPushResponse, SyncState};
use wealthfolio_storage_sqlite::sync::SqliteSyncEngineDbPorts;

use super::{
    create_client, decrypt_sync_payload, encrypt_sync_payload, get_access_token,
    get_sync_identity_from_store, persist_device_config_from_identity, SyncCycleResult,
};

struct TauriEnginePorts {
    context: Arc<ServiceContext>,
    db: SqliteSyncEngineDbPorts,
}

impl TauriEnginePorts {
    fn new(context: Arc<ServiceContext>) -> Self {
        let db = SqliteSyncEngineDbPorts::new(context.app_sync_repository());
        Self { context, db }
    }

    fn to_parent_identity(identity: &SyncIdentity) -> super::SyncIdentity {
        super::SyncIdentity {
            device_id: identity.device_id.clone(),
            root_key: identity.root_key.clone(),
            key_version: identity.key_version,
        }
    }
}

#[async_trait]
impl OutboxStore for TauriEnginePorts {
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
impl ReplayStore for TauriEnginePorts {
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
impl SyncTransport for TauriEnginePorts {
    async fn get_events_cursor(
        &self,
        token: &str,
        device_id: &str,
    ) -> Result<wealthfolio_device_sync::SyncCursorResponse, TransportError> {
        create_client()
            .map_err(|e| TransportError {
                message: e,
                retry_class: wealthfolio_device_sync::ApiRetryClass::Permanent,
            })?
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
            .map_err(|e| TransportError {
                message: e,
                retry_class: wealthfolio_device_sync::ApiRetryClass::Permanent,
            })?
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
            .map_err(|e| TransportError {
                message: e,
                retry_class: wealthfolio_device_sync::ApiRetryClass::Permanent,
            })?
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
impl CredentialStore for TauriEnginePorts {
    fn get_sync_identity(&self) -> Option<SyncIdentity> {
        get_sync_identity_from_store().map(|identity| SyncIdentity {
            device_id: identity.device_id,
            root_key: identity.root_key,
            key_version: identity.key_version,
        })
    }

    fn get_access_token(&self) -> Result<String, String> {
        get_access_token()
    }

    async fn get_sync_state(&self) -> Result<SyncState, String> {
        self.context
            .device_enroll_service()
            .get_sync_state()
            .await
            .map(|value| value.state)
            .map_err(|err| err.message)
    }

    async fn persist_device_config(&self, identity: &SyncIdentity, trust_state: &str) {
        let identity = Self::to_parent_identity(identity);
        persist_device_config_from_identity(self.context.as_ref(), &identity, trust_state).await;
    }

    fn encrypt_sync_payload(
        &self,
        plaintext_payload: &str,
        identity: &SyncIdentity,
        payload_key_version: i32,
    ) -> Result<String, String> {
        encrypt_sync_payload(
            plaintext_payload,
            &Self::to_parent_identity(identity),
            payload_key_version,
        )
    }

    fn decrypt_sync_payload(
        &self,
        encrypted_payload: &str,
        identity: &SyncIdentity,
        payload_key_version: i32,
    ) -> Result<String, String> {
        decrypt_sync_payload(
            encrypted_payload,
            &Self::to_parent_identity(identity),
            payload_key_version,
        )
    }
}

pub(super) async fn run_sync_cycle(
    context: Arc<ServiceContext>,
) -> Result<SyncCycleResult, String> {
    let runtime = context.device_sync_runtime();
    let ports = TauriEnginePorts::new(context);
    let result = runtime.run_cycle(&ports).await?;
    Ok(SyncCycleResult {
        status: result.status,
        lock_version: result.lock_version,
        pushed_count: result.pushed_count,
        pulled_count: result.pulled_count,
        cursor: result.cursor,
        needs_bootstrap: result.needs_bootstrap,
    })
}

pub async fn ensure_background_engine_started(context: Arc<ServiceContext>) -> Result<(), String> {
    if get_sync_identity_from_store().is_none() {
        return Ok(());
    }

    let runtime = context.device_sync_runtime();
    let ports = Arc::new(TauriEnginePorts::new(context));
    runtime.ensure_background_started(ports).await;
    Ok(())
}

pub async fn ensure_background_engine_stopped(context: Arc<ServiceContext>) -> Result<(), String> {
    let runtime = context.device_sync_runtime();
    runtime.ensure_background_stopped().await;
    Ok(())
}
