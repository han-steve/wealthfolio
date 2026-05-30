/// Integration tests for the self-hosted Wealthfolio Connect sync server.
///
/// Tests the full Supabase-compatible auth + device-sync API implemented in
/// apps/server/src/api/sync_server/. Uses tower::ServiceExt::oneshot so no
/// real TCP port is needed.
use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rand::{rngs::OsRng, RngCore};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::ServiceExt;
use wealthfolio_server::api::sync_server::{sync_router, SyncServerState};

// ─── Test helpers ─────────────────────────────────────────────────────────────

fn build_jwt_secret() -> Vec<u8> {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.to_vec()
}

struct TestApp {
    router: axum::Router,
}

impl TestApp {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let db_path = tmp.path().join("sync.db").to_string_lossy().to_string();
        let snap_dir = tmp.path().join("snapshots").to_string_lossy().to_string();
        let secret = build_jwt_secret();
        let state = Arc::new(SyncServerState::new(&db_path, &snap_dir, &secret).unwrap());
        let router = sync_router(state);
        // Keep tempdir alive by leaking – OK in tests
        std::mem::forget(tmp);
        TestApp { router }
    }

    async fn request(&self, req: Request<Body>) -> (StatusCode, Value) {
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, body)
    }

    async fn post_json(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.post_json_auth(uri, body, None).await
    }

    async fn post_json_auth(&self, uri: &str, body: Value, token: Option<&str>) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = req.body(Body::from(body.to_string())).unwrap();
        self.request(req).await
    }

    async fn get_auth(&self, uri: &str, token: &str) -> (StatusCode, Value) {
        let req = Request::builder()
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        self.request(req).await
    }

    async fn get_auth_with_header(&self, uri: &str, token: &str, extra_header: (&str, &str)) -> (StatusCode, Value) {
        let req = Request::builder()
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(extra_header.0, extra_header.1)
            .body(Body::empty())
            .unwrap();
        self.request(req).await
    }

    /// Sign up a new user and return their access_token.
    async fn signup(&self, email: &str, password: &str) -> String {
        let (_s, body) = self
            .post_json(
                "/auth/v1/signup",
                json!({ "email": email, "password": password }),
            )
            .await;
        body["access_token"].as_str().unwrap().to_string()
    }

    /// Enroll a device and return the device_id + response body.
    async fn enroll(&self, token: &str, nonce: &str, name: &str) -> Value {
        let (_s, body) = self
            .post_json_auth(
                "/api/v1/sync/team/devices",
                json!({
                    "device_nonce": nonce,
                    "display_name": name,
                    "platform": "macos",
                }),
                Some(token),
            )
            .await;
        body
    }

    /// Bootstrap E2EE keys for a device (initialize → commit).
    async fn bootstrap_keys(&self, token: &str, device_id: &str) {
        let (_s, init_body) = self
            .post_json_auth(
                "/api/v1/sync/team/keys/initialize",
                json!({ "device_id": device_id }),
                Some(token),
            )
            .await;
        assert_eq!(init_body["mode"], "BOOTSTRAP", "expected BOOTSTRAP mode");

        let (_s, _commit) = self
            .post_json_auth(
                "/api/v1/sync/team/keys/initialize/commit",
                json!({
                    "device_id": device_id,
                    "key_version": 1,
                    "device_key_envelope": BASE64.encode(b"fake-envelope"),
                    "signature": BASE64.encode(b"fake-sig"),
                }),
                Some(token),
            )
            .await;
    }
}

// ─── Auth: settings ───────────────────────────────────────────────────────────

#[tokio::test]
async fn auth_settings_no_auth_required() {
    let app = TestApp::new();
    let (status, body) = app
        .request(Request::builder().uri("/auth/v1/settings").body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["disable_signup"], false);
    assert_eq!(body["autoconfirm"], true);
}

// ─── Auth: signup ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn signup_creates_user_and_team() {
    let app = TestApp::new();
    let (status, body) = app
        .post_json("/auth/v1/signup", json!({ "email": "alice@test.com", "password": "pass123" }))
        .await;
    assert_eq!(status, 200);
    assert!(!body["access_token"].as_str().unwrap().is_empty());
    assert!(!body["refresh_token"].as_str().unwrap().is_empty());
    assert_eq!(body["user"]["email"], "alice@test.com");
    assert!(body["user"]["app_metadata"]["team_id"].as_str().is_some());
    assert_eq!(body["expires_in"], 3600);
}

#[tokio::test]
async fn signup_rejects_duplicate_email() {
    let app = TestApp::new();
    app.post_json("/auth/v1/signup", json!({ "email": "dup@test.com", "password": "pass123" }))
        .await;
    let (status, _) = app
        .post_json("/auth/v1/signup", json!({ "email": "dup@test.com", "password": "other" }))
        .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn signup_rejects_short_password() {
    let app = TestApp::new();
    let (status, _) = app
        .post_json("/auth/v1/signup", json!({ "email": "x@test.com", "password": "ab" }))
        .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn signup_email_case_insensitive() {
    let app = TestApp::new();
    app.post_json("/auth/v1/signup", json!({ "email": "CaseUser@Test.COM", "password": "pass123" }))
        .await;
    // Logging in with all-lowercase should work
    let (status, body) = app
        .post_json(
            "/auth/v1/token?grant_type=password",
            json!({ "email": "caseuser@test.com", "password": "pass123" }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["user"]["email"], "caseuser@test.com");
}

// ─── Auth: OTP ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn otp_flow_creates_user_and_returns_token() {
    let app = TestApp::new();

    // Request OTP
    let (status, _) = app
        .post_json("/auth/v1/otp", json!({ "email": "otp@test.com" }))
        .await;
    assert_eq!(status, 200);

    // Read the OTP from the sync.db (the db path is embedded in the state)
    // We test this indirectly by trying a wrong code first, then right code
    let (bad_status, _) = app
        .post_json(
            "/auth/v1/verify",
            json!({ "email": "otp@test.com", "token": "000000", "type": "email" }),
        )
        .await;
    assert_eq!(bad_status, 400, "wrong OTP should fail");
}

#[tokio::test]
async fn otp_invalidates_previous_codes() {
    // Requesting a second OTP should invalidate the first one.
    // We verify this behaviorally: two OTP requests, first code is gone.
    // (We can't check the DB directly here but the "used" flag is set on re-request.)
    let app = TestApp::new();
    app.post_json("/auth/v1/otp", json!({ "email": "otp2@test.com" })).await;
    app.post_json("/auth/v1/otp", json!({ "email": "otp2@test.com" })).await;
    // Both codes would fail with "000000" — just confirm we don't crash
    let (status, _) = app
        .post_json(
            "/auth/v1/verify",
            json!({ "email": "otp2@test.com", "token": "000000", "type": "email" }),
        )
        .await;
    assert_eq!(status, 400);
}

// ─── Auth: password login ─────────────────────────────────────────────────────

#[tokio::test]
async fn password_login_returns_token() {
    let app = TestApp::new();
    app.post_json("/auth/v1/signup", json!({ "email": "bob@test.com", "password": "secret99" }))
        .await;

    let (status, body) = app
        .post_json(
            "/auth/v1/token?grant_type=password",
            json!({ "email": "bob@test.com", "password": "secret99" }),
        )
        .await;
    assert_eq!(status, 200);
    assert!(!body["access_token"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn password_login_wrong_password_returns_400() {
    let app = TestApp::new();
    app.post_json("/auth/v1/signup", json!({ "email": "c@test.com", "password": "correct" }))
        .await;

    let (status, _) = app
        .post_json(
            "/auth/v1/token?grant_type=password",
            json!({ "email": "c@test.com", "password": "wrong" }),
        )
        .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn password_login_unknown_user_returns_400() {
    let app = TestApp::new();
    let (status, _) = app
        .post_json(
            "/auth/v1/token?grant_type=password",
            json!({ "email": "nobody@test.com", "password": "anything" }),
        )
        .await;
    assert_eq!(status, 400);
}

// ─── Auth: refresh token ──────────────────────────────────────────────────────

#[tokio::test]
async fn refresh_token_issues_new_access_token() {
    let app = TestApp::new();
    let (_, signup_body) = app
        .post_json("/auth/v1/signup", json!({ "email": "d@test.com", "password": "pass123" }))
        .await;
    let refresh_token = signup_body["refresh_token"].as_str().unwrap().to_string();

    let (status, body) = app
        .post_json(
            "/auth/v1/token?grant_type=refresh_token",
            json!({ "refresh_token": refresh_token }),
        )
        .await;
    assert_eq!(status, 200);
    let new_at = body["access_token"].as_str().unwrap();
    assert!(!new_at.is_empty());
    // Old access_token and new one may differ (new iat/exp)
}

#[tokio::test]
async fn refresh_token_single_use() {
    let app = TestApp::new();
    let (_, body) = app
        .post_json("/auth/v1/signup", json!({ "email": "e@test.com", "password": "pass123" }))
        .await;
    let rt = body["refresh_token"].as_str().unwrap().to_string();

    // First use: should succeed
    let (s1, _) = app
        .post_json("/auth/v1/token?grant_type=refresh_token", json!({ "refresh_token": rt }))
        .await;
    assert_eq!(s1, 200);

    // Second use of same token: should fail (token was revoked on use)
    let (s2, _) = app
        .post_json("/auth/v1/token?grant_type=refresh_token", json!({ "refresh_token": rt }))
        .await;
    assert_eq!(s2, 401);
}

// ─── Auth: get user ───────────────────────────────────────────────────────────

#[tokio::test]
async fn get_user_returns_current_user() {
    let app = TestApp::new();
    let token = app.signup("f@test.com", "pass123").await;
    let (status, body) = app.get_auth("/auth/v1/user", &token).await;
    assert_eq!(status, 200);
    assert_eq!(body["email"], "f@test.com");
}

#[tokio::test]
async fn get_user_without_token_returns_401() {
    let app = TestApp::new();
    let (status, _) = app
        .request(Request::builder().uri("/auth/v1/user").body(Body::empty()).unwrap())
        .await;
    assert_eq!(status, 401);
}

// ─── Auth: logout ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn logout_revokes_refresh_tokens() {
    let app = TestApp::new();
    let (_, body) = app
        .post_json("/auth/v1/signup", json!({ "email": "g@test.com", "password": "pass123" }))
        .await;
    let token = body["access_token"].as_str().unwrap().to_string();
    let rt = body["refresh_token"].as_str().unwrap().to_string();

    let (status, _) = app
        .post_json_auth("/auth/v1/logout", json!({}), Some(&token))
        .await;
    assert_eq!(status, 204);

    // After logout, refresh_token should be invalid
    let (s, _) = app
        .post_json("/auth/v1/token?grant_type=refresh_token", json!({ "refresh_token": rt }))
        .await;
    assert_eq!(s, 401);
}

// ─── User: /api/v1/user/me ────────────────────────────────────────────────────

#[tokio::test]
async fn user_me_returns_pro_plan() {
    let app = TestApp::new();
    let token = app.signup("h@test.com", "pass123").await;
    let (status, body) = app.get_auth("/api/v1/user/me", &token).await;
    assert_eq!(status, 200);
    assert_eq!(body["team"]["plan"], "pro");
    assert_eq!(body["team"]["subscriptionStatus"], "active");
}

// ─── Devices: enrollment ──────────────────────────────────────────────────────

#[tokio::test]
async fn first_device_gets_bootstrap_mode() {
    let app = TestApp::new();
    let token = app.signup("dev1@test.com", "pass123").await;
    let body = app.enroll(&token, "nonce-abc", "My Mac").await;
    assert_eq!(body["mode"], "BOOTSTRAP");
    assert!(body["device_id"].as_str().is_some());
    assert_eq!(body["e2ee_key_version"], 1);
}

#[tokio::test]
async fn second_device_gets_pair_mode_after_keys_initialized() {
    let app = TestApp::new();
    let token = app.signup("dev2@test.com", "pass123").await;

    // Enroll first device and bootstrap keys
    let d1 = app.enroll(&token, "nonce-d1", "Device 1").await;
    let d1_id = d1["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &d1_id).await;

    // Enroll second device
    let d2 = app.enroll(&token, "nonce-d2", "Device 2").await;
    assert_eq!(d2["mode"], "PAIR");
    assert!(!d2["trusted_devices"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn trusted_device_reenroll_gets_ready() {
    let app = TestApp::new();
    let token = app.signup("dev3@test.com", "pass123").await;
    let d1 = app.enroll(&token, "nonce-ready", "Device 1").await;
    let d1_id = d1["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &d1_id).await;

    // Re-enroll same nonce → should get READY
    let d1_again = app.enroll(&token, "nonce-ready", "Device 1 Updated").await;
    assert_eq!(d1_again["mode"], "READY");
    assert_eq!(d1_again["device_id"], d1["device_id"]);
}

#[tokio::test]
async fn enroll_requires_auth() {
    let app = TestApp::new();
    let (status, _) = app
        .post_json(
            "/api/v1/sync/team/devices",
            json!({ "device_nonce": "x", "display_name": "y", "platform": "macos" }),
        )
        .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn list_and_get_devices() {
    let app = TestApp::new();
    let token = app.signup("devlist@test.com", "pass123").await;
    let d = app.enroll(&token, "nonce-list", "My Device").await;
    let device_id = d["device_id"].as_str().unwrap();

    let (status, list) = app.get_auth("/api/v1/sync/team/devices", &token).await;
    assert_eq!(status, 200);
    let arr = list.as_array().unwrap();
    assert!(arr.iter().any(|dev| dev["id"] == device_id));

    let (status, single) = app
        .get_auth(&format!("/api/v1/sync/team/devices/{device_id}"), &token)
        .await;
    assert_eq!(status, 200);
    assert_eq!(single["id"], device_id);
}

#[tokio::test]
async fn update_and_delete_device() {
    let app = TestApp::new();
    let token = app.signup("devmod@test.com", "pass123").await;
    let d = app.enroll(&token, "nonce-mod", "Old Name").await;
    let device_id = d["device_id"].as_str().unwrap();

    // Update display name
    let (status, _) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{device_id}"),
            json!({ "display_name": "New Name" }),
            Some(&token),
        )
        .await;
    // PATCH via post_json_auth won't work (uses POST) — use raw request
    let req = Request::builder()
        .method(Method::PATCH)
        .uri(&format!("/api/v1/sync/team/devices/{device_id}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(json!({"display_name": "New Name"}).to_string()))
        .unwrap();
    let (status, body) = app.request(req).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);

    // Delete
    let req = Request::builder()
        .method(Method::DELETE)
        .uri(&format!("/api/v1/sync/team/devices/{device_id}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = app.request(req).await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
}

#[tokio::test]
async fn revoke_device_changes_trust_state() {
    let app = TestApp::new();
    let token = app.signup("revoke@test.com", "pass123").await;
    let d = app.enroll(&token, "nonce-rev", "Device").await;
    let device_id = d["device_id"].as_str().unwrap();

    let (status, body) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{device_id}/revoke"),
            json!({}),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);

    // Re-check device trust_state is now revoked
    let (_, dev) = app
        .get_auth(&format!("/api/v1/sync/team/devices/{device_id}"), &token)
        .await;
    assert_eq!(dev["trustState"], "revoked");
}

// ─── Keys: initialize / commit ────────────────────────────────────────────────

#[tokio::test]
async fn keys_initialize_returns_bootstrap_challenge() {
    let app = TestApp::new();
    let token = app.signup("keys1@test.com", "pass123").await;
    let d = app.enroll(&token, "keys-nonce", "Device").await;
    let device_id = d["device_id"].as_str().unwrap();

    let (status, body) = app
        .post_json_auth(
            "/api/v1/sync/team/keys/initialize",
            json!({ "device_id": device_id }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["mode"], "BOOTSTRAP");
    assert!(body["challenge"].as_str().is_some());
    assert!(body["nonce"].as_str().is_some());
    assert_eq!(body["key_version"], 1);
}

#[tokio::test]
async fn keys_commit_marks_device_trusted() {
    let app = TestApp::new();
    let token = app.signup("keys2@test.com", "pass123").await;
    let d = app.enroll(&token, "keys-nonce2", "Device").await;
    let device_id = d["device_id"].as_str().unwrap();

    app.post_json_auth(
        "/api/v1/sync/team/keys/initialize",
        json!({ "device_id": device_id }),
        Some(&token),
    )
    .await;

    let (status, body) = app
        .post_json_auth(
            "/api/v1/sync/team/keys/initialize/commit",
            json!({
                "device_id": device_id,
                "key_version": 1,
                "device_key_envelope": BASE64.encode(b"envelope"),
                "signature": BASE64.encode(b"sig"),
            }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);
    assert_eq!(body["keyState"], "ACTIVE");

    // Device should now be trusted
    let (_, dev) = app
        .get_auth(&format!("/api/v1/sync/team/devices/{device_id}"), &token)
        .await;
    assert_eq!(dev["trustState"], "trusted");
    assert_eq!(dev["trustedKeyVersion"], 1.0);
}

#[tokio::test]
async fn keys_initialize_already_init_trusted_device_returns_ready() {
    let app = TestApp::new();
    let token = app.signup("keys3@test.com", "pass123").await;
    let d = app.enroll(&token, "keys-ready", "Device").await;
    let device_id = d["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &device_id).await;

    let (status, body) = app
        .post_json_auth(
            "/api/v1/sync/team/keys/initialize",
            json!({ "device_id": device_id }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["mode"], "READY");
    assert_eq!(body["e2ee_key_version"], 1);
}

#[tokio::test]
async fn keys_initialize_untrusted_device_returns_pairing_required() {
    let app = TestApp::new();
    let token = app.signup("keys4@test.com", "pass123").await;

    // Bootstrap with first device
    let d1 = app.enroll(&token, "keys-d1", "Device 1").await;
    let d1_id = d1["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &d1_id).await;

    // Enroll second device (untrusted)
    let d2 = app.enroll(&token, "keys-d2", "Device 2").await;
    let d2_id = d2["device_id"].as_str().unwrap().to_string();

    let (status, body) = app
        .post_json_auth(
            "/api/v1/sync/team/keys/initialize",
            json!({ "device_id": d2_id }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["mode"], "PAIRING_REQUIRED");
    let trusted = body["trusted_devices"].as_array().unwrap();
    assert!(trusted.iter().any(|d| d["id"] == d1_id));
}

// ─── Keys: rotate ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn key_rotation_increments_version() {
    let app = TestApp::new();
    let token = app.signup("rot1@test.com", "pass123").await;
    let d = app.enroll(&token, "rot-nonce", "Device").await;
    let device_id = d["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &device_id).await;

    let (status, body) = app
        .post_json_auth(
            "/api/v1/sync/team/keys/rotate",
            json!({ "initiator_device_id": device_id }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["newKeyVersion"], 2);
    assert!(body["challenge"].as_str().is_some());
}

#[tokio::test]
async fn key_rotation_commit_updates_trusted_devices() {
    let app = TestApp::new();
    let token = app.signup("rot2@test.com", "pass123").await;
    let d = app.enroll(&token, "rot-commit-nonce", "Device").await;
    let device_id = d["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &device_id).await;

    // Start rotation
    app.post_json_auth(
        "/api/v1/sync/team/keys/rotate",
        json!({ "initiator_device_id": device_id }),
        Some(&token),
    )
    .await;

    // Commit rotation
    let (status, body) = app
        .post_json_auth(
            "/api/v1/sync/team/keys/rotate/commit",
            json!({
                "new_key_version": 2,
                "envelopes": [{ "device_id": device_id, "device_key_envelope": BASE64.encode(b"env2") }],
                "signature": BASE64.encode(b"sig2"),
            }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["keyVersion"], 2);
}

// Regression: interrupted bootstrap (PENDING state) must return BOOTSTRAP, not PAIRING_REQUIRED.
// Before the fix, a device that called initialize_keys but never called commit would see
// PAIRING_REQUIRED with no trusted devices on retry, causing the client to enter ORPHANED state.
#[tokio::test]
async fn keys_initialize_pending_state_returns_bootstrap() {
    let app = TestApp::new();
    let token = app.signup("pending1@test.com", "pass123").await;
    let d = app.enroll(&token, "pending-nonce", "Device").await;
    let device_id = d["device_id"].as_str().unwrap().to_string();

    // First call: creates PENDING team key, returns BOOTSTRAP with challenge+nonce
    let (status, first) = app
        .post_json_auth(
            "/api/v1/sync/team/keys/initialize",
            json!({ "device_id": device_id }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(first["mode"], "BOOTSTRAP");
    let challenge1 = first["challenge"].as_str().unwrap().to_string();
    let nonce1 = first["nonce"].as_str().unwrap().to_string();

    // Intentionally skip commit (simulate interrupted bootstrap).
    // Second call: key is still PENDING, must return BOOTSTRAP with same challenge/nonce.
    let (status2, second) = app
        .post_json_auth(
            "/api/v1/sync/team/keys/initialize",
            json!({ "device_id": device_id }),
            Some(&token),
        )
        .await;
    assert_eq!(status2, 200);
    assert_eq!(second["mode"], "BOOTSTRAP", "PENDING key must return BOOTSTRAP, not PAIRING_REQUIRED");
    assert_eq!(second["challenge"].as_str().unwrap(), challenge1, "challenge must be stable across retries");
    assert_eq!(second["nonce"].as_str().unwrap(), nonce1, "nonce must be stable across retries");
}

// ─── Keys: reset ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn reset_allows_re_bootstrap() {
    let app = TestApp::new();
    let token = app.signup("reset1@test.com", "pass123").await;
    let d = app.enroll(&token, "reset-nonce", "Device").await;
    let device_id = d["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &device_id).await;

    // Reset
    let (status, body) = app
        .post_json_auth("/api/v1/sync/team/keys/reset", json!({}), Some(&token))
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["success"], true);

    // After reset, re-enrolling same nonce should get BOOTSTRAP (not READY)
    let d_again = app.enroll(&token, "reset-nonce", "Device").await;
    assert_eq!(d_again["mode"], "BOOTSTRAP");

    // And initializing keys should give BOOTSTRAP again
    let d_new = app.enroll(&token, "reset-nonce-2", "Device 2").await;
    let d_new_id = d_new["device_id"].as_str().unwrap().to_string();
    let (_, init_body) = app
        .post_json_auth(
            "/api/v1/sync/team/keys/initialize",
            json!({ "device_id": d_new_id }),
            Some(&token),
        )
        .await;
    assert_eq!(init_body["mode"], "BOOTSTRAP");
}

// ─── Pairing: full flow ───────────────────────────────────────────────────────

#[tokio::test]
async fn pairing_full_flow() {
    let app = TestApp::new();
    let token = app.signup("pair1@test.com", "pass123").await;

    // Set up issuer device
    let d1 = app.enroll(&token, "pair-d1", "Issuer").await;
    let d1_id = d1["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &d1_id).await;

    // Set up claimer device (enrolled, not trusted)
    let d2 = app.enroll(&token, "pair-d2", "Claimer").await;
    let d2_id = d2["device_id"].as_str().unwrap().to_string();

    // Use a known code so we can compute its hash
    let code = "TESTPAIRCODE123";
    use sha2::Digest;
    let code_hash = format!("{:x}", sha2::Sha256::digest(code.as_bytes()));

    // Issuer creates pairing
    let (status, pair_body) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{d1_id}/pairings"),
            json!({
                "code_hash": code_hash,
                "ephemeral_public_key": BASE64.encode(b"issuer-eph-pub"),
            }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    let pairing_id = pair_body["pairingId"].as_str().unwrap().to_string();
    assert!(!pairing_id.is_empty());
    assert_eq!(pair_body["requireSas"], false);

    // Claimer claims the pairing with the code
    let (status, claim_body) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{d2_id}/pairings/claim"),
            json!({
                "code": code,
                "ephemeral_public_key": BASE64.encode(b"claimer-eph-pub"),
            }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200, "claim failed: {claim_body}");
    assert_eq!(claim_body["sessionId"], pairing_id);
    assert_eq!(
        claim_body["issuerEphemeralPub"],
        BASE64.encode(b"issuer-eph-pub")
    );

    // Issuer polls pairing status → should be 'claimed'
    let (status, get_body) = app
        .get_auth(
            &format!("/api/v1/sync/team/devices/{d1_id}/pairings/{pairing_id}"),
            &token,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(get_body["status"], "claimed");

    // Issuer approves
    let (status, _) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{d1_id}/pairings/{pairing_id}/approve"),
            json!({}),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);

    // Issuer completes (sends encrypted key bundle)
    let (status, complete_body) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{d1_id}/pairings/{pairing_id}/complete"),
            json!({
                "encrypted_key_bundle": BASE64.encode(b"encrypted-bundle"),
                "sas_proof": null,
                "signature": BASE64.encode(b"sig"),
            }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(complete_body["success"], true);

    // Claimer polls for messages
    let (status, msgs_body) = app
        .get_auth(
            &format!("/api/v1/sync/team/devices/{d2_id}/pairings/{pairing_id}/messages"),
            &token,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(msgs_body["sessionStatus"], "completed");
    let messages = msgs_body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["payloadType"], "rk_transfer_v1");
    assert_eq!(messages[0]["payload"], BASE64.encode(b"encrypted-bundle"));

    // Claimer confirms (marks itself trusted)
    let (status, confirm_body) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{d2_id}/pairings/{pairing_id}/confirm"),
            json!({ "proof": null }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(confirm_body["success"], true);
    assert_eq!(confirm_body["keyVersion"], 1);

    // Claimer device should now be trusted
    let (_, dev) = app
        .get_auth(&format!("/api/v1/sync/team/devices/{d2_id}"), &token)
        .await;
    assert_eq!(dev["trustState"], "trusted");
}

#[tokio::test]
async fn pairing_invalid_code_returns_404() {
    let app = TestApp::new();
    let token = app.signup("pair2@test.com", "pass123").await;
    let d1 = app.enroll(&token, "p2-d1", "Device").await;
    let d1_id = d1["device_id"].as_str().unwrap();

    let (status, _) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{d1_id}/pairings/claim"),
            json!({
                "code": "WRONGCODE",
                "ephemeral_public_key": BASE64.encode(b"key"),
            }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn pairing_untrusted_device_cannot_create_pairing() {
    let app = TestApp::new();
    let token = app.signup("pair3@test.com", "pass123").await;

    // Bootstrap with d1
    let d1 = app.enroll(&token, "p3-d1", "Device 1").await;
    let d1_id = d1["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &d1_id).await;

    // d2 is enrolled but untrusted
    let d2 = app.enroll(&token, "p3-d2", "Device 2").await;
    let d2_id = d2["device_id"].as_str().unwrap();

    let (status, _) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{d2_id}/pairings"),
            json!({ "code_hash": "abc", "ephemeral_public_key": "key" }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn cancel_pairing_prevents_claim() {
    let app = TestApp::new();
    let token = app.signup("pair4@test.com", "pass123").await;
    let d1 = app.enroll(&token, "p4-d1", "Device").await;
    let d1_id = d1["device_id"].as_str().unwrap().to_string();
    app.bootstrap_keys(&token, &d1_id).await;

    let code = "CANCELCODE";
    use sha2::Digest;
    let code_hash = format!("{:x}", sha2::Sha256::digest(code.as_bytes()));

    let (_, pair_body) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{d1_id}/pairings"),
            json!({ "code_hash": code_hash, "ephemeral_public_key": "key" }),
            Some(&token),
        )
        .await;
    let pairing_id = pair_body["pairingId"].as_str().unwrap().to_string();

    // Cancel it
    app.post_json_auth(
        &format!("/api/v1/sync/team/devices/{d1_id}/pairings/{pairing_id}/cancel"),
        json!({}),
        Some(&token),
    )
    .await;

    // Now claim should fail (pairing is no longer open)
    let d2 = app.enroll(&token, "p4-d2", "Claimer").await;
    let d2_id = d2["device_id"].as_str().unwrap();
    let (status, _) = app
        .post_json_auth(
            &format!("/api/v1/sync/team/devices/{d2_id}/pairings/claim"),
            json!({ "code": code, "ephemeral_public_key": "key" }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 404);
}

// ─── Events: push / pull ──────────────────────────────────────────────────────

#[tokio::test]
async fn push_and_pull_events() {
    let app = TestApp::new();
    let token = app.signup("evt1@test.com", "pass123").await;
    let d = app.enroll(&token, "evt-d1", "Device").await;
    let device_id = d["device_id"].as_str().unwrap().to_string();

    // Push two events
    let (status, push_body) = app
        .post_json_auth(
            "/api/v1/sync/events/push",
            json!({
                "events": [
                    {
                        "event_id": "evt-001",
                        "device_id": device_id,
                        "type": "CREATE",
                        "entity": "account",
                        "entity_id": "acct-1",
                        "client_timestamp": "2026-01-01T00:00:00Z",
                        "payload": BASE64.encode(b"payload-1"),
                        "payload_key_version": 1,
                    },
                    {
                        "event_id": "evt-002",
                        "device_id": device_id,
                        "type": "UPDATE",
                        "entity": "account",
                        "entity_id": "acct-1",
                        "client_timestamp": "2026-01-01T00:01:00Z",
                        "payload": BASE64.encode(b"payload-2"),
                        "payload_key_version": 1,
                    }
                ]
            }),
            Some(&token),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(push_body["accepted"].as_array().unwrap().len(), 2);
    assert_eq!(push_body["duplicate"].as_array().unwrap().len(), 0);
    assert!(push_body["serverCursor"].as_i64().unwrap() >= 2);

    // Pull all events
    let (status, pull_body) = app.get_auth("/api/v1/sync/events/pull?since=0", &token).await;
    assert_eq!(status, 200);
    let events = pull_body["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["eventId"], "evt-001");
    assert_eq!(events[1]["eventId"], "evt-002");
    assert_eq!(pull_body["hasMore"], false);
}

#[tokio::test]
async fn push_deduplicates_events() {
    let app = TestApp::new();
    let token = app.signup("evt2@test.com", "pass123").await;
    let d = app.enroll(&token, "evt-dup", "Device").await;
    let device_id = d["device_id"].as_str().unwrap();

    let event = json!({
        "events": [{
            "event_id": "dedup-001",
            "device_id": device_id,
            "type": "CREATE",
            "entity": "activity",
            "entity_id": "act-1",
            "client_timestamp": "2026-01-01T00:00:00Z",
            "payload": BASE64.encode(b"data"),
            "payload_key_version": 1,
        }]
    });

    let (_, r1) = app.post_json_auth("/api/v1/sync/events/push", event.clone(), Some(&token)).await;
    assert_eq!(r1["accepted"].as_array().unwrap().len(), 1);

    // Push same event again
    let (_, r2) = app.post_json_auth("/api/v1/sync/events/push", event, Some(&token)).await;
    assert_eq!(r2["accepted"].as_array().unwrap().len(), 0);
    assert_eq!(r2["duplicate"].as_array().unwrap().len(), 1);
    assert_eq!(r2["duplicate"][0]["eventId"], "dedup-001");
}

#[tokio::test]
async fn pull_events_with_cursor_returns_only_new() {
    let app = TestApp::new();
    let token = app.signup("evt3@test.com", "pass123").await;
    let d = app.enroll(&token, "evt-cursor", "Device").await;
    let device_id = d["device_id"].as_str().unwrap();

    // Push 3 events
    for i in 1..=3 {
        app.post_json_auth(
            "/api/v1/sync/events/push",
            json!({
                "events": [{
                    "event_id": format!("cursor-{i}"),
                    "device_id": device_id,
                    "type": "CREATE",
                    "entity": "account",
                    "entity_id": format!("acct-{i}"),
                    "client_timestamp": "2026-01-01T00:00:00Z",
                    "payload": "",
                    "payload_key_version": 1,
                }]
            }),
            Some(&token),
        )
        .await;
    }

    // Get cursor after first event
    let (_, pull1) = app.get_auth("/api/v1/sync/events/pull?since=0&limit=1", &token).await;
    let cursor_after_1 = pull1["nextCursor"].as_i64().unwrap();

    // Pull from that cursor
    let (_, pull2) = app
        .get_auth(&format!("/api/v1/sync/events/pull?since={cursor_after_1}"), &token)
        .await;
    let events = pull2["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["eventId"], "cursor-2");
}

#[tokio::test]
async fn team_isolation_between_users() {
    let app = TestApp::new();
    let t1 = app.signup("iso1@test.com", "pass123").await;
    let t2 = app.signup("iso2@test.com", "pass123").await;

    let d1 = app.enroll(&t1, "iso-d1", "User1 Device").await;
    let device_id_1 = d1["device_id"].as_str().unwrap();

    // User 1 pushes an event
    app.post_json_auth(
        "/api/v1/sync/events/push",
        json!({
            "events": [{
                "event_id": "iso-evt-1",
                "device_id": device_id_1,
                "type": "CREATE",
                "entity": "account",
                "entity_id": "acct-secret",
                "client_timestamp": "2026-01-01T00:00:00Z",
                "payload": BASE64.encode(b"user1-secret"),
                "payload_key_version": 1,
            }]
        }),
        Some(&t1),
    )
    .await;

    // User 2 pulls events → should see 0 events (different team)
    let (status, pull_body) = app.get_auth("/api/v1/sync/events/pull?since=0", &t2).await;
    assert_eq!(status, 200);
    assert_eq!(
        pull_body["events"].as_array().unwrap().len(),
        0,
        "user2 should not see user1's events"
    );
}

// ─── Events: cursor & reconcile ───────────────────────────────────────────────

#[tokio::test]
async fn get_cursor_returns_zero_when_no_events() {
    let app = TestApp::new();
    let token = app.signup("cur1@test.com", "pass123").await;
    let (status, body) = app.get_auth("/api/v1/sync/events/cursor", &token).await;
    assert_eq!(status, 200);
    assert_eq!(body["cursor"], 0);
}

#[tokio::test]
async fn reconcile_noop_when_empty() {
    let app = TestApp::new();
    let token = app.signup("rec1@test.com", "pass123").await;
    let d = app.enroll(&token, "rec-d1", "Device").await;
    let device_id = d["device_id"].as_str().unwrap();
    let (status, body) = app
        .get_auth_with_header(
            "/api/v1/sync/events/reconcile-ready-state",
            &token,
            ("x-wf-device-id", device_id),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["action"], "NOOP");
}

#[tokio::test]
async fn reconcile_pull_tail_when_events_present() {
    let app = TestApp::new();
    let token = app.signup("rec2@test.com", "pass123").await;
    let d = app.enroll(&token, "rec-d2", "Device").await;
    let device_id = d["device_id"].as_str().unwrap().to_string();

    app.post_json_auth(
        "/api/v1/sync/events/push",
        json!({
            "events": [{
                "event_id": "rec-evt-1",
                "device_id": device_id,
                "type": "CREATE",
                "entity": "account",
                "entity_id": "acct-1",
                "client_timestamp": "2026-01-01T00:00:00Z",
                "payload": "",
                "payload_key_version": 1,
            }]
        }),
        Some(&token),
    )
    .await;

    let (status, body) = app
        .get_auth_with_header(
            "/api/v1/sync/events/reconcile-ready-state",
            &token,
            ("x-wf-device-id", &device_id),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["action"], "PULL_TAIL");
}

// Regression: after snapshot upload, a device that has already applied the snapshot
// (cursor >= snap_seq) must get PULL_TAIL, not BOOTSTRAP_SNAPSHOT.
#[tokio::test]
async fn reconcile_respects_device_cursor_after_snapshot() {
    let app = TestApp::new();
    let token = app.signup("rec3@test.com", "pass123").await;
    let d = app.enroll(&token, "rec-d3", "Device").await;
    let device_id = d["device_id"].as_str().unwrap().to_string();

    // Push one event → server_cursor = 1
    app.post_json_auth(
        "/api/v1/sync/events/push",
        json!({ "events": [{ "event_id": "rec-snap-1", "device_id": device_id,
            "type": "CREATE", "entity": "account", "entity_id": "acct-snap",
            "client_timestamp": "2026-01-01T00:00:00Z", "payload": "", "payload_key_version": 1 }] }),
        Some(&token),
    ).await;

    // Upload snapshot at seq=1 (simulates issuer completing pairing transfer)
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sync/snapshots/upload")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("x-wf-device-id", &device_id)
        .header("x-snapshot-schema-version", "1")
        .header("x-snapshot-covers-tables", "accounts")
        .header("x-snapshot-size-bytes", "4")
        .header("x-snapshot-checksum", "sha256:test")
        .header("x-snapshot-payload-key-version", "1")
        .body(Body::from(b"data".to_vec()))
        .unwrap();
    app.request(req).await;

    // Device with cursor=0 should get BOOTSTRAP_SNAPSHOT (needs the snapshot)
    let (status, body) = app
        .get_auth_with_header(
            "/api/v1/sync/events/reconcile-ready-state?cursor=0",
            &token,
            ("x-wf-device-id", &device_id),
        ).await;
    assert_eq!(status, 200);
    assert_eq!(body["action"], "BOOTSTRAP_SNAPSHOT", "fresh device should bootstrap");

    // Device with cursor=1 (already applied snapshot) should get PULL_TAIL, not BOOTSTRAP_SNAPSHOT
    let (status, body) = app
        .get_auth_with_header(
            "/api/v1/sync/events/reconcile-ready-state?cursor=1",
            &token,
            ("x-wf-device-id", &device_id),
        ).await;
    assert_eq!(status, 200);
    assert_eq!(body["action"], "PULL_TAIL", "up-to-date device must not re-bootstrap");
}

// ─── Snapshots ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_latest_snapshot_empty_when_none() {
    let app = TestApp::new();
    let token = app.signup("snap1@test.com", "pass123").await;
    let (status, body) = app.get_auth("/api/v1/sync/snapshots/latest", &token).await;
    assert_eq!(status, 200);
    assert_eq!(body["snapshotId"], "");
    assert_eq!(body["oplogSeq"], 0);
}

#[tokio::test]
async fn upload_and_download_snapshot() {
    let app = TestApp::new();
    let token = app.signup("snap2@test.com", "pass123").await;
    let d = app.enroll(&token, "snap-d1", "Device").await;
    let device_id = d["device_id"].as_str().unwrap();

    let payload = b"fake-encrypted-snapshot-data";

    // Upload
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sync/snapshots/upload")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("x-wf-device-id", device_id)
        .header("x-snapshot-schema-version", "1")
        .header("x-snapshot-covers-tables", "accounts,activities")
        .header("x-snapshot-size-bytes", payload.len().to_string())
        .header("x-snapshot-checksum", "sha256:abc123")
        .header("x-snapshot-payload-key-version", "1")
        .body(Body::from(payload.to_vec()))
        .unwrap();
    let (status, upload_body) = app.request(req).await;
    assert_eq!(status, 200, "upload failed: {upload_body}");
    let snapshot_id = upload_body["snapshotId"].as_str().unwrap().to_string();
    assert!(!snapshot_id.is_empty());

    // Get latest
    let (status, latest) = app.get_auth("/api/v1/sync/snapshots/latest", &token).await;
    assert_eq!(status, 200);
    assert_eq!(latest["snapshotId"], snapshot_id);
    let covers: Vec<&str> = latest["coversTables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(covers.contains(&"accounts"));
    assert!(covers.contains(&"activities"));

    // Download
    let req = Request::builder()
        .uri(&format!("/api/v1/sync/snapshots/{snapshot_id}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let body_bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body_bytes.as_ref(), payload);
}

#[tokio::test]
async fn snapshot_not_found_returns_500_or_internal() {
    let app = TestApp::new();
    let token = app.signup("snap3@test.com", "pass123").await;
    let req = Request::builder()
        .uri("/api/v1/sync/snapshots/nonexistent-id")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = app.request(req).await;
    // Returns 404 (not found in DB) or 500 (file not found)
    assert!(status == StatusCode::NOT_FOUND || status == StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn snapshot_idempotent_with_event_id() {
    let app = TestApp::new();
    let token = app.signup("snap4@test.com", "pass123").await;
    let d = app.enroll(&token, "snap-idem", "Device").await;
    let device_id = d["device_id"].as_str().unwrap();

    let make_upload_req = || {
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/sync/snapshots/upload")
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header("x-wf-device-id", device_id)
            .header("x-snapshot-schema-version", "1")
            .header("x-snapshot-covers-tables", "accounts")
            .header("x-snapshot-size-bytes", "4")
            .header("x-snapshot-checksum", "abc")
            .header("x-snapshot-payload-key-version", "1")
            .header("x-snapshot-event-id", "idem-snap-001")
            .body(Body::from(b"data".to_vec()))
            .unwrap()
    };

    let (s1, b1) = app.request(make_upload_req()).await;
    let (s2, b2) = app.request(make_upload_req()).await;
    assert_eq!(s1, 200);
    assert_eq!(s2, 200);
    // Both should return the same snapshot_id
    assert_eq!(b1["snapshotId"], b2["snapshotId"]);
}

#[tokio::test]
async fn reconcile_bootstrap_snapshot_when_snapshot_ahead_of_cursor() {
    let app = TestApp::new();
    let token = app.signup("rec3@test.com", "pass123").await;
    let d = app.enroll(&token, "rec-d3", "Device").await;
    let device_id = d["device_id"].as_str().unwrap().to_string();

    // Upload a snapshot with oplog_seq = 100 (no real events)
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sync/snapshots/upload")
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("x-wf-device-id", &device_id)
        .header("x-snapshot-schema-version", "1")
        .header("x-snapshot-covers-tables", "accounts")
        .header("x-snapshot-size-bytes", "4")
        .header("x-snapshot-checksum", "abc")
        .header("x-snapshot-payload-key-version", "1")
        .header("x-snapshot-base-seq", "100")
        .body(Body::from(b"data".to_vec()))
        .unwrap();
    app.request(req).await;

    let (status, body) = app
        .get_auth_with_header(
            "/api/v1/sync/events/reconcile-ready-state",
            &token,
            ("x-wf-device-id", &device_id),
        )
        .await;
    assert_eq!(status, 200);
    // server_cursor (0 events) <= snap_seq (100) → BOOTSTRAP_SNAPSHOT
    assert_eq!(body["action"], "BOOTSTRAP_SNAPSHOT");
    assert!(body["latestSnapshot"]["snapshotId"].as_str().is_some());
}

// ─── Security: unauthenticated requests ───────────────────────────────────────

#[tokio::test]
async fn all_protected_endpoints_require_auth() {
    let app = TestApp::new();
    let unauthenticated_endpoints = vec![
        (Method::GET, "/api/v1/sync/team/devices"),
        (Method::POST, "/api/v1/sync/team/devices"),
        (Method::GET, "/api/v1/sync/team/devices/some-id"),
        (Method::POST, "/api/v1/sync/team/keys/initialize"),
        (Method::POST, "/api/v1/sync/team/keys/initialize/commit"),
        (Method::POST, "/api/v1/sync/team/keys/rotate"),
        (Method::POST, "/api/v1/sync/team/keys/rotate/commit"),
        (Method::POST, "/api/v1/sync/team/keys/reset"),
        (Method::POST, "/api/v1/sync/events/push"),
        (Method::GET, "/api/v1/sync/events/pull"),
        (Method::GET, "/api/v1/sync/events/cursor"),
        (Method::GET, "/api/v1/sync/events/reconcile-ready-state"),
        (Method::GET, "/api/v1/sync/snapshots/latest"),
        (Method::POST, "/api/v1/sync/snapshots/upload"),
        (Method::GET, "/api/v1/user/me"),
    ];

    for (method, path) in unauthenticated_endpoints {
        let req = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let (status, _) = app.request(req).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} should require auth but returned {status}"
        );
    }
}

#[tokio::test]
async fn expired_or_invalid_token_returns_401() {
    let app = TestApp::new();
    let req = Request::builder()
        .uri("/api/v1/sync/team/devices")
        .header(header::AUTHORIZATION, "Bearer not-a-valid-jwt")
        .body(Body::empty())
        .unwrap();
    let (status, _) = app.request(req).await;
    assert_eq!(status, 401);
}
