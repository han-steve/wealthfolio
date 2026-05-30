use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::header::AUTHORIZATION,
    routing::{get, post},
    Json, Router,
};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use super::{new_id, now_rfc3339, AppMetadata, SyncClaims, SyncError, SyncResult, SyncServerState};
use rusqlite::OptionalExtension;

const ACCESS_TOKEN_TTL_SECS: i64 = 3600;
const REFRESH_TOKEN_TTL_DAYS: i64 = 90;
/// Grace period (seconds) during which a recently-revoked refresh token is still
/// accepted. This prevents the iOS "re-login every open" bug caused by token
/// rotation race conditions (app suspended between server revoking old token and
/// client storing the new one).
const REFRESH_REVOKE_GRACE_SECS: i64 = 120;

// ─── Request/Response Types ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TokenQuery {
    pub grant_type: Option<String>,
}

#[derive(Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: Option<String>,
}

#[derive(Deserialize)]
pub struct PasswordLoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct RefreshTokenRequest {
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct OtpRequest {
    pub email: String,
}

#[derive(Deserialize)]
pub struct VerifyRequest {
    pub email: Option<String>,
    pub token: String,
    #[serde(rename = "type")]
    pub verify_type: Option<String>,
}

#[derive(Serialize)]
pub struct SupabaseUser {
    pub id: String,
    pub aud: String,
    pub role: String,
    pub email: String,
    pub email_confirmed_at: String,
    pub app_metadata: serde_json::Value,
    pub user_metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
    pub is_anonymous: bool,
}

#[derive(Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub expires_at: i64,
    pub refresh_token: String,
    pub user: SupabaseUser,
}

#[derive(Serialize)]
pub struct AuthSettings {
    pub external: serde_json::Value,
    pub disable_signup: bool,
    pub autoconfirm: bool,
    pub mailer_autoconfirm: bool,
    pub sms_autoconfirm: bool,
    pub external_email_enabled: bool,
    pub external_phone_enabled: bool,
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn hash_password(password: &str) -> SyncResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| SyncError::Internal(format!("Password hash error: {e}")))
}

fn verify_password(password: &str, hash: &str) -> SyncResult<()> {
    let parsed = PasswordHash::new(hash)
        .map_err(|e| SyncError::Internal(format!("Invalid password hash: {e}")))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| SyncError::BadRequest("Invalid email or password".to_string()))
}

pub fn make_supabase_user(id: &str, email: &str, team_id: &str, created_at: &str) -> SupabaseUser {
    let ts = if created_at.is_empty() { &now_rfc3339() as &str } else { created_at };
    SupabaseUser {
        id: id.to_string(),
        aud: "authenticated".to_string(),
        role: "authenticated".to_string(),
        email: email.to_string(),
        email_confirmed_at: ts.to_string(),
        app_metadata: serde_json::json!({
            "provider": "email",
            "providers": ["email"],
            "team_id": team_id
        }),
        user_metadata: serde_json::Value::Object(Default::default()),
        created_at: ts.to_string(),
        updated_at: ts.to_string(),
        is_anonymous: false,
    }
}

pub fn make_claims(user_id: &str, email: &str, team_id: &str) -> SyncClaims {
    let now = chrono::Utc::now().timestamp();
    SyncClaims {
        sub: user_id.to_string(),
        aud: "authenticated".to_string(),
        role: "authenticated".to_string(),
        email: email.to_string(),
        app_metadata: AppMetadata {
            team_id: team_id.to_string(),
            provider: Some("email".to_string()),
        },
        user_metadata: serde_json::Value::Object(Default::default()),
        iat: now,
        exp: now + ACCESS_TOKEN_TTL_SECS,
    }
}

pub struct UserRow {
    pub id: String,
    pub email: String,
    pub password_hash: Option<String>,
    pub created_at: String,
}

/// Get existing user or create a new one (with auto team creation).
pub fn get_or_create_user_and_team(
    conn: &rusqlite::Connection,
    email: &str,
    password_hash: Option<&str>,
) -> SyncResult<(UserRow, String)> {
    let existing: Option<(String, Option<String>, String)> = conn
        .query_row(
            "SELECT id, password_hash, created_at FROM sync_users WHERE email = ?1 COLLATE NOCASE",
            [email],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| SyncError::Internal(e.to_string()))?;

    let user = if let Some((uid, ph, ca)) = existing {
        UserRow { id: uid, email: email.to_string(), password_hash: ph, created_at: ca }
    } else {
        let uid = new_id();
        let ca = now_rfc3339();
        conn.execute(
            "INSERT INTO sync_users (id, email, password_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![uid, email, password_hash, ca],
        )
        .map_err(|e| SyncError::Internal(e.to_string()))?;
        UserRow { id: uid, email: email.to_string(), password_hash: password_hash.map(String::from), created_at: ca }
    };

    let team_id: Option<String> = conn
        .query_row(
            "SELECT id FROM sync_teams WHERE owner_id = ?1",
            [&user.id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| SyncError::Internal(e.to_string()))?;

    let team_id = if let Some(tid) = team_id {
        tid
    } else {
        let tid = new_id();
        conn.execute(
            "INSERT INTO sync_teams (id, name, owner_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![tid, format!("{}'s Team", email), user.id],
        )
        .map_err(|e| SyncError::Internal(e.to_string()))?;
        tid
    };

    Ok((user, team_id))
}

fn create_refresh_token(conn: &rusqlite::Connection, user_id: &str) -> SyncResult<String> {
    let token = format!("{}{}", new_id().replace('-', ""), new_id().replace('-', ""));
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(REFRESH_TOKEN_TTL_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    conn.execute(
        "INSERT INTO sync_refresh_tokens (id, user_id, token, expires_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![new_id(), user_id, token, expires_at],
    )
    .map_err(|e| SyncError::Internal(e.to_string()))?;
    Ok(token)
}

pub fn build_token_response(
    state: &SyncServerState,
    conn: &rusqlite::Connection,
    user_id: &str,
    email: &str,
    team_id: &str,
    created_at: &str,
) -> SyncResult<TokenResponse> {
    let claims = make_claims(user_id, email, team_id);
    let access_token = state.issue_token(&claims)?;
    let refresh_token = create_refresh_token(conn, user_id)?;
    let supabase_user = make_supabase_user(user_id, email, team_id, created_at);
    Ok(TokenResponse {
        access_token,
        token_type: "bearer".to_string(),
        expires_in: ACCESS_TOKEN_TTL_SECS,
        expires_at: claims.exp,
        refresh_token,
        user: supabase_user,
    })
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn get_settings() -> Json<AuthSettings> {
    Json(AuthSettings {
        external: serde_json::json!({
            "email": { "enabled": true },
            "google": { "enabled": false }
        }),
        disable_signup: false,
        autoconfirm: true,
        mailer_autoconfirm: true,
        sms_autoconfirm: false,
        external_email_enabled: true,
        external_phone_enabled: false,
    })
}

async fn signup(
    State(state): State<Arc<SyncServerState>>,
    Json(body): Json<SignupRequest>,
) -> SyncResult<Json<TokenResponse>> {
    let email = body.email.trim().to_lowercase();
    if email.is_empty() {
        return Err(SyncError::BadRequest("Email is required".to_string()));
    }

    let password_hash = if let Some(pw) = &body.password {
        if pw.len() < 6 {
            return Err(SyncError::BadRequest("Password must be at least 6 characters".to_string()));
        }
        Some(hash_password(pw)?)
    } else {
        None
    };

    let state_clone = state.clone();
    let email_clone = email.clone();
    let ph_clone = password_hash.clone();

    state
        .with_db(move |conn| {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sync_users WHERE email = ?1 COLLATE NOCASE",
                    [&email_clone],
                    |row| row.get::<_, i64>(0),
                )
                .map(|c| c > 0)
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            if exists {
                return Err(SyncError::BadRequest("Email already registered. Please sign in.".to_string()));
            }

            let (user, team_id) = get_or_create_user_and_team(conn, &email_clone, ph_clone.as_deref())?;
            build_token_response(&state_clone, conn, &user.id, &user.email, &team_id, &user.created_at)
        })
        .await
        .map(Json)
}

async fn token_handler(
    State(state): State<Arc<SyncServerState>>,
    Query(query): Query<TokenQuery>,
    body: axum::body::Bytes,
) -> SyncResult<Json<TokenResponse>> {
    let grant_type = query.grant_type.as_deref().unwrap_or("");
    match grant_type {
        "password" => {
            let req: PasswordLoginRequest = serde_json::from_slice(&body)
                .map_err(|_| SyncError::BadRequest("Invalid request body".to_string()))?;
            password_login(state, req).await
        }
        "refresh_token" => {
            let req: RefreshTokenRequest = serde_json::from_slice(&body)
                .map_err(|_| SyncError::BadRequest("Invalid request body".to_string()))?;
            do_refresh_token(state, req).await
        }
        _ => Err(SyncError::BadRequest(format!("Unsupported grant_type: {grant_type}"))),
    }
}

async fn password_login(
    state: Arc<SyncServerState>,
    req: PasswordLoginRequest,
) -> SyncResult<Json<TokenResponse>> {
    let email = req.email.trim().to_lowercase();
    let password = req.password.clone();
    let state_clone = state.clone();

    state
        .with_db(move |conn| {
            let row: Option<(String, Option<String>, String)> = conn
                .query_row(
                    "SELECT id, password_hash, created_at FROM sync_users WHERE email = ?1 COLLATE NOCASE",
                    [&email],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let (uid, ph, ca) = row
                .ok_or_else(|| SyncError::BadRequest("Invalid email or password".to_string()))?;
            let hash = ph.ok_or_else(|| {
                SyncError::BadRequest("No password set. Use magic link to sign in.".to_string())
            })?;
            verify_password(&password, &hash)?;

            let team_id: String = conn
                .query_row(
                    "SELECT id FROM sync_teams WHERE owner_id = ?1",
                    [&uid],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?
                .ok_or_else(|| SyncError::Internal("No team found for user".to_string()))?;

            build_token_response(&state_clone, conn, &uid, &email, &team_id, &ca)
        })
        .await
        .map(Json)
}

async fn do_refresh_token(
    state: Arc<SyncServerState>,
    req: RefreshTokenRequest,
) -> SyncResult<Json<TokenResponse>> {
    let token = req.refresh_token.clone();
    let state_clone = state.clone();

    state
        .with_db(move |conn| {
            let row: Option<(String, String, i64, String)> = conn
                .query_row(
                    "SELECT rt.user_id, u.created_at, rt.revoked, \
                            COALESCE(rt.revoked_at, '') \
                     FROM sync_refresh_tokens rt \
                     JOIN sync_users u ON u.id = rt.user_id \
                     WHERE rt.token = ?1 AND rt.expires_at > strftime('%Y-%m-%dT%H:%M:%SZ','now')",
                    [&token],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let (uid, ca, revoked, revoked_at) = row.ok_or(SyncError::Unauthorized)?;

            // Allow recently-revoked tokens (grace period for iOS token rotation race)
            if revoked != 0 {
                let within_grace = if !revoked_at.is_empty() {
                    if let Ok(revoked_time) = chrono::DateTime::parse_from_rfc3339(&revoked_at) {
                        let elapsed = chrono::Utc::now().signed_duration_since(revoked_time);
                        elapsed.num_seconds() < REFRESH_REVOKE_GRACE_SECS
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !within_grace {
                    return Err(SyncError::Unauthorized);
                }
            }

            // Revoke old token and record when
            conn.execute(
                "UPDATE sync_refresh_tokens SET revoked = 1, revoked_at = ?2 WHERE token = ?1 AND revoked = 0",
                rusqlite::params![token, now_rfc3339()],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            let (email, team_id) = conn
                .query_row(
                    "SELECT u.email, t.id FROM sync_users u \
                     JOIN sync_teams t ON t.owner_id = u.id WHERE u.id = ?1",
                    [&uid],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            build_token_response(&state_clone, conn, &uid, &email, &team_id, &ca)
        })
        .await
        .map(Json)
}

async fn send_otp(
    State(state): State<Arc<SyncServerState>>,
    Json(body): Json<OtpRequest>,
) -> SyncResult<Json<serde_json::Value>> {
    let email = body.email.trim().to_lowercase();
    if email.is_empty() {
        return Err(SyncError::BadRequest("Email is required".to_string()));
    }

    let code: String = if let Ok(fixed) = std::env::var("WF_FIXED_OTP") {
        fixed
    } else {
        use rand::Rng;
        format!("{:06}", rand::thread_rng().gen_range(0u32..1_000_000))
    };
    let code_clone = code.clone();
    let email_clone = email.clone();

    state
        .with_db(move |conn| {
            get_or_create_user_and_team(conn, &email_clone, None)?;

            let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(10))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

            conn.execute(
                "UPDATE sync_otps SET used = 1 WHERE email = ?1 COLLATE NOCASE AND used = 0",
                [&email_clone],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;

            conn.execute(
                "INSERT INTO sync_otps (id, email, code, expires_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![new_id(), email_clone, code_clone, expires_at],
            )
            .map_err(|e| SyncError::Internal(e.to_string()))?;
            Ok(())
        })
        .await?;

    // Print OTP to stdout so the self-hosted admin can see it
    println!("\n╔══════════════════════════════════════════╗");
    println!("║  WEALTHFOLIO CONNECT: SIGN-IN CODE       ║");
    println!("║  Email: {:<34}║", email);
    println!("║  Code:  {:<34}║", code);
    println!("║  Expires in 10 minutes                   ║");
    println!("╚══════════════════════════════════════════╝\n");

    Ok(Json(serde_json::json!({})))
}

async fn verify_otp(
    State(state): State<Arc<SyncServerState>>,
    Json(body): Json<VerifyRequest>,
) -> SyncResult<Json<TokenResponse>> {
    let email = body.email.as_deref().unwrap_or("").trim().to_lowercase();
    if email.is_empty() {
        return Err(SyncError::BadRequest("Email is required".to_string()));
    }
    let code = body.token.trim().to_string();
    let state_clone = state.clone();

    state
        .with_db(move |conn| {
            let otp_id: Option<String> = conn
                .query_row(
                    "SELECT id FROM sync_otps \
                     WHERE email = ?1 COLLATE NOCASE AND code = ?2 AND used = 0 \
                     AND expires_at > strftime('%Y-%m-%dT%H:%M:%SZ','now') \
                     ORDER BY created_at DESC LIMIT 1",
                    rusqlite::params![email, code],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let otp_id = otp_id.ok_or_else(|| {
                SyncError::BadRequest("Invalid or expired code. Please request a new one.".to_string())
            })?;

            conn.execute("UPDATE sync_otps SET used = 1 WHERE id = ?1", [&otp_id])
                .map_err(|e| SyncError::Internal(e.to_string()))?;

            let (user, team_id) = get_or_create_user_and_team(conn, &email, None)?;
            build_token_response(&state_clone, conn, &user.id, &user.email, &team_id, &user.created_at)
        })
        .await
        .map(Json)
}

async fn get_user(
    State(state): State<Arc<SyncServerState>>,
    headers: axum::http::HeaderMap,
) -> SyncResult<Json<SupabaseUser>> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or(SyncError::Unauthorized)?;

    let claims = state.validate_token(token)?;
    let user = make_supabase_user(&claims.sub, &claims.email, &claims.app_metadata.team_id, "");
    Ok(Json(user))
}

async fn logout(
    State(state): State<Arc<SyncServerState>>,
    headers: axum::http::HeaderMap,
) -> axum::http::StatusCode {
    if let Some(token_str) = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        if let Ok(claims) = state.validate_token(token_str.trim()) {
            let uid = claims.sub.clone();
            let _ = state
                .with_db(move |conn| {
                    conn.execute(
                        "UPDATE sync_refresh_tokens SET revoked = 1 WHERE user_id = ?1",
                        [&uid],
                    )
                    .map_err(|e| SyncError::Internal(e.to_string()))?;
                    Ok(())
                })
                .await;
        }
    }
    axum::http::StatusCode::NO_CONTENT
}

pub fn auth_router(state: Arc<SyncServerState>) -> Router {
    Router::new()
        .route("/auth/v1/settings", get(get_settings))
        .route("/auth/v1/signup", post(signup))
        .route("/auth/v1/token", post(token_handler))
        .route("/auth/v1/otp", post(send_otp))
        .route("/auth/v1/verify", post(verify_otp))
        .route("/auth/v1/user", get(get_user))
        .route("/auth/v1/logout", post(logout))
        .with_state(state)
}
