pub mod auth;
pub mod brokerage;
pub mod devices;
pub mod events;
pub mod keys;
pub mod pairing;
pub mod server_device;
pub mod snapshots;
pub mod user;

use std::sync::Arc;

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use base64::Engine as _;

pub type SyncResult<T> = Result<T, SyncError>;

const LOCAL_SYNC_USER_ID: &str = "local-sync-user";
const LOCAL_SYNC_TEAM_ID: &str = "local-sync-team";
const LOCAL_SYNC_EMAIL: &str = "local@wealthfolio.local";

fn sync_auth_required() -> bool {
    std::env::var("WF_AUTH_REQUIRED")
        .map(|v| {
            let n = v.trim().to_ascii_lowercase();
            !(n == "false" || n == "0" || n == "off" || n == "no")
        })
        .unwrap_or(true)
}

#[derive(Debug, Serialize)]
pub struct SyncApiError {
    pub error: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub enum SyncError {
    Unauthorized,
    NotFound(String),
    BadRequest(String),
    Internal(String),
    Conflict(String),
}

impl IntoResponse for SyncError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            SyncError::Unauthorized => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", "Unauthorized".to_string()),
            SyncError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg),
            SyncError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg),
            SyncError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", msg),
            SyncError::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT", msg),
        };
        let body = SyncApiError {
            error: code.to_lowercase(),
            code: code.to_string(),
            message,
        };
        (status, Json(body)).into_response()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncClaims {
    pub sub: String,
    pub aud: String,
    pub role: String,
    pub email: String,
    pub app_metadata: AppMetadata,
    pub user_metadata: serde_json::Value,
    pub iat: i64,
    pub exp: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppMetadata {
    pub team_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Clone)]
pub struct SyncServerState {
    pub db_path: String,
    pub encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    validation: Validation,
    pub snapshot_dir: String,
    /// Fixed device ID for the homeserver (from WF_DEVICE_ID or deterministic default)
    pub server_device_id: String,
    /// Root key for E2EE (from WF_ROOT_KEY env var), used by server-as-device pairing
    pub server_root_key: Option<Vec<u8>>,
    /// Key version matching the client's key_version
    pub server_key_version: i32,
    /// Optional fixed pairing code from WF_PAIR_CODE env var.
    /// When set, overrides the HKDF-derived code from the root key.
    pub configured_pair_code: Option<String>,
}

impl SyncServerState {
    pub fn new(db_path: &str, snapshot_dir: &str, jwt_secret: &[u8]) -> anyhow::Result<Self> {
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch(DB_INIT_SQL)?;
        // Migrations for existing databases
        let _ = conn.execute_batch(
            "ALTER TABLE sync_refresh_tokens ADD COLUMN revoked_at TEXT;"
        );
        if !sync_auth_required() {
            seed_local_identity(&conn)?;
        }

        // Load server-as-device configuration from environment
        let server_root_key = std::env::var("WF_ROOT_KEY").ok()
            .filter(|s| !s.trim().is_empty())
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s.trim()).ok());

        // Optional hard-coded pairing code (WF_PAIR_CODE=123456).
        // When set, all new devices use this fixed code — no need to derive from root key.
        let configured_pair_code = std::env::var("WF_PAIR_CODE").ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(ref code) = configured_pair_code {
            tracing::info!("Server pair code: {} (from WF_PAIR_CODE)", code);
        }

        let server_key_version: i32 = std::env::var("WF_KEY_VERSION")
            .unwrap_or_else(|_| "1".to_string())
            .parse()
            .unwrap_or(1);

        // Compute or read server device ID
        let server_device_id = std::env::var("WF_DEVICE_ID")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                // Derive a deterministic UUID from the JWT secret
                use sha2::{Digest, Sha256};
                let mut h = Sha256::new();
                h.update(jwt_secret);
                h.update(b"wealthfolio-server-device-id");
                let hash = h.finalize();
                format!(
                    "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
                    u32::from_be_bytes(hash[0..4].try_into().unwrap()),
                    u16::from_be_bytes(hash[4..6].try_into().unwrap()),
                    u16::from_be_bytes(hash[6..8].try_into().unwrap()) & 0x0fff,
                    (u16::from_be_bytes(hash[8..10].try_into().unwrap()) & 0x3fff) | 0x8000,
                    {
                        let b = &hash[10..16];
                        u64::from_be_bytes([0, 0, b[0], b[1], b[2], b[3], b[4], b[5]])
                    }
                )
            });

        // Register server as a trusted device
        if let Err(e) = server_device::ensure_server_device_registered(
            &conn, &server_device_id, server_key_version,
        ) {
            tracing::warn!("Failed to register server device: {}", e);
        }

        drop(conn);

        std::fs::create_dir_all(snapshot_dir)?;

        let encoding_key = EncodingKey::from_secret(jwt_secret);
        let decoding_key = DecodingKey::from_secret(jwt_secret);
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&["authenticated"]);
        validation.leeway = 60;

        Ok(Self {
            db_path: db_path.to_string(),
            encoding_key,
            decoding_key,
            validation,
            snapshot_dir: snapshot_dir.to_string(),
            server_device_id,
            server_root_key,
            server_key_version,
            configured_pair_code,
        })
    }

    pub fn open_conn(&self) -> SyncResult<rusqlite::Connection> {
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| SyncError::Internal(e.to_string()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| SyncError::Internal(e.to_string()))?;
        Ok(conn)
    }

    pub async fn with_db<T, F>(&self, f: F) -> SyncResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> SyncResult<T> + Send + 'static,
    {
        let db_path = self.db_path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(&db_path)
                .map_err(|e| SyncError::Internal(e.to_string()))?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
                .map_err(|e| SyncError::Internal(e.to_string()))?;
            f(&conn)
        })
        .await
        .map_err(|e| SyncError::Internal(e.to_string()))?
    }

    pub fn validate_token(&self, token: &str) -> SyncResult<SyncClaims> {
        jsonwebtoken::decode::<SyncClaims>(token, &self.decoding_key, &self.validation)
            .map(|data| data.claims)
            .map_err(|_| SyncError::Unauthorized)
    }

    pub fn issue_token(&self, claims: &SyncClaims) -> SyncResult<String> {
        jsonwebtoken::encode(&Header::default(), claims, &self.encoding_key)
            .map_err(|e| SyncError::Internal(e.to_string()))
    }
}

fn seed_local_identity(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO sync_users (id, email, password_hash) VALUES (?1, ?2, NULL)",
        rusqlite::params![LOCAL_SYNC_USER_ID, LOCAL_SYNC_EMAIL],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO sync_teams (id, name, owner_id) VALUES (?1, ?2, ?3)",
        rusqlite::params![LOCAL_SYNC_TEAM_ID, "Local Team", LOCAL_SYNC_USER_ID],
    )?;
    Ok(())
}

/// Extractor that validates the Bearer token and returns authenticated user info.
pub struct AuthUser {
    pub user_id: String,
    pub team_id: String,
    pub email: String,
}

impl<S> FromRequestParts<S> for AuthUser
where
    Arc<SyncServerState>: axum::extract::FromRef<S>,
    S: Send + Sync,
{
    type Rejection = SyncError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if !sync_auth_required() {
            return Ok(AuthUser {
                user_id: LOCAL_SYNC_USER_ID.to_string(),
                team_id: LOCAL_SYNC_TEAM_ID.to_string(),
                email: LOCAL_SYNC_EMAIL.to_string(),
            });
        }

        let sync_state = Arc::<SyncServerState>::from_ref(state);
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or(SyncError::Unauthorized)?;

        let claims = sync_state.validate_token(token)?;
        Ok(AuthUser {
            user_id: claims.sub,
            team_id: claims.app_metadata.team_id,
            email: claims.email,
        })
    }
}

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

const DB_INIT_SQL: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS sync_users (
    id TEXT PRIMARY KEY,
    email TEXT UNIQUE NOT NULL COLLATE NOCASE,
    password_hash TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE IF NOT EXISTS sync_teams (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    owner_id TEXT NOT NULL REFERENCES sync_users(id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE IF NOT EXISTS sync_refresh_tokens (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES sync_users(id),
    token TEXT UNIQUE NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0,
    revoked_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    expires_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_otps (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL COLLATE NOCASE,
    code TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE IF NOT EXISTS sync_devices (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES sync_teams(id),
    user_id TEXT NOT NULL REFERENCES sync_users(id),
    device_nonce TEXT UNIQUE NOT NULL,
    display_name TEXT NOT NULL,
    platform TEXT NOT NULL,
    os_version TEXT,
    app_version TEXT,
    device_public_key TEXT,
    trust_state TEXT NOT NULL DEFAULT 'untrusted',
    trusted_key_version REAL,
    last_seen_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE IF NOT EXISTS sync_team_keys (
    team_id TEXT PRIMARY KEY REFERENCES sync_teams(id),
    key_version INTEGER NOT NULL DEFAULT 1,
    key_state TEXT NOT NULL DEFAULT 'ACTIVE',
    challenge TEXT,
    nonce TEXT,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE IF NOT EXISTS sync_device_key_envelopes (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    key_version INTEGER NOT NULL,
    device_key_envelope TEXT NOT NULL,
    signature TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    UNIQUE(device_id, key_version)
);

CREATE TABLE IF NOT EXISTS sync_pairings (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL,
    issuer_device_id TEXT NOT NULL,
    claimer_device_id TEXT,
    code_hash TEXT NOT NULL,
    ephemeral_public_key TEXT NOT NULL,
    claimer_ephemeral_pub TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    key_version INTEGER NOT NULL,
    require_sas INTEGER NOT NULL DEFAULT 0,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE IF NOT EXISTS sync_pairing_messages (
    id TEXT PRIMARY KEY,
    pairing_id TEXT NOT NULL REFERENCES sync_pairings(id),
    payload_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE TABLE IF NOT EXISTS sync_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    entity TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    client_timestamp TEXT NOT NULL,
    server_timestamp TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    payload TEXT NOT NULL,
    payload_key_version INTEGER NOT NULL DEFAULT 1,
    UNIQUE(event_id, team_id)
);

CREATE INDEX IF NOT EXISTS idx_sync_events_team_seq ON sync_events(team_id, seq);

CREATE TABLE IF NOT EXISTS sync_snapshots (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    covers_tables TEXT NOT NULL,
    oplog_seq INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL,
    checksum TEXT NOT NULL,
    metadata_payload TEXT,
    payload_key_version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);

CREATE INDEX IF NOT EXISTS idx_sync_snapshots_team ON sync_snapshots(team_id, oplog_seq DESC);
"#;

pub fn sync_router(state: Arc<SyncServerState>) -> Router {
    Router::new()
        .merge(auth::auth_router(state.clone()))
        .merge(user::user_router(state.clone()))
        .merge(devices::devices_router(state.clone()))
        .merge(keys::keys_router(state.clone()))
        .merge(pairing::pairing_router(state.clone()))
        .merge(events::events_router(state.clone()))
        .merge(snapshots::snapshots_router(state.clone()))
        .merge(brokerage::brokerage_router(state.clone()))
        .merge(server_device::server_device_router(state.clone()))
}
