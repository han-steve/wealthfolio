# Wealthfolio Fork Technical Handoff & Progress Report

This document outlines the current state, modifications, and infrastructure deployment details for this self-hosted Wealthfolio fork.

---

## 1. Native Apps Deprecated & Web App Adopted

After attempting native iOS app builds signed with developer certificates, we have deprecated the native iOS and desktop sync client in favor of a **fully self-hosted Web App** deployment.
*   **Web App URL:** `https://wealthfolio.homelab` (accessible via Tailscale DNS resolution)
*   **Benefits:** Bypasses compile-time variables, iOS developer provisioning profiles, keychain/keyring synchronization errors, andSupabase account login pages completely.

---

## 2. Technical Modifications in Code

### A. Authentication Bypass (Local / Self-Hosted Mode)
*   **Frontend Bypass:** Modified the frontend React auth provider to dynamically inspect `window.location.hostname`. If the domain matches a homelab signature (e.g. `*.homelab`, `.local`, or local IP ranges), it skips the Supabase auth flow, automatically initializing a mock session with a dynamic dummy JWT token and an active "plus" subscription status.
*   **Backend Bypass:** Modified the Rust client library wrappers (`crates/connect/src/token_lifecycle.rs`) to detect self-hosted domains and automatically return mock tokens with a 10-year expiration date, bypassing the OS keychain refresh token checks.
*   **No Auth Server Needed:** The homeserver compiles and runs with `WF_AUTH_REQUIRED="false"`. No Supabase dependency is required.

### B. Deprecation of Sync Server Features from Code
*   We modified `apps/server/Cargo.toml` to remove `connect-sync` and `device-sync` from the Cargo default features:
    ```toml
    [features]
    default = []
    connect-sync = []
    device-sync = []
    ```
    This removes the multi-device client sync REST endpoints (`/api/v1/sync/...` and `/api/v1/connect/...`) from the compiled server binary, reducing compile times and attack surface.

---

## 3. Infrastructure & Deployment Changes

### A. Core Server Deployment (`wealthfolio.yaml`)
*   Located in the homelab infra repo at [apps/wealthfolio/wealthfolio.yaml](file:///Users/stevehan/p/homelab/apps/wealthfolio/wealthfolio.yaml).
*   Cleaned up all sync-server env vars (e.g., `WF_SYNC_DB_PATH`, `WF_SYNC_SNAPSHOT_DIR`, `WF_FIXED_OTP`, `WF_KEY_VERSION`, `WF_DEVICE_ID`, and secret refs for `WF_ROOT_KEY` and `WF_PAIR_CODE`).
*   The server now mounts a single PVC `wealthfolio-data` to `/data` containing the sqlite database `/data/wealthfolio.db` and serves the static frontend from `/app/dist`.

### B. Daily SimpleFIN Sync Job (`simplefin-sync`)
*   Runs daily at **3:15 AM** via a Kubernetes CronJob in the `wealthfolio` namespace.
*   Executes `simplefin_sync.py` directly against the database volume.
*   **Bypassing Deprecated Sync pushes:** We updated the script execution command to include the `--no-snapshot` flag. This disables the deprecated event-push and snapshot upload routines (removing calls to Supabase `/auth/v1/otp`), eliminating errors.
*   **Automatic Portfolio Recalculation:** We modified `simplefin_sync.py` to support `WF_SERVER_URL` and query the running cluster service at `http://wealthfolio.wealthfolio.svc.cluster.local:8080/api/v1/portfolio/recalculate` directly. Recalculation now triggers automatically in the cluster immediately after sync completes (returning HTTP 202).

### C. SimpleFIN Grafana Metrics Exporter
*   The `simplefin-exporter` CronJob and ServiceMonitor run in the `monitoring` namespace (configured in `apps/simplefin/simplefin-cronjob.yaml`).
*   Fixed a `403 Forbidden` error caused by an expired setup token by patching the exporter's Secret with the active base64-encoded `SIMPLEFIN_ACCESS_URL` token from the wealthfolio namespace. It successfully pulls metrics for all 29 accounts for Prometheus scrape targets.

---

## 4. Troubleshooting Negative Account Balances

### A. Cash Drift and historical Negative Balances
If you notice that some accounts show negative cash balances prior to daily drift runs:
*   **Cause:** Investment accounts import buy transactions (stocks/ETFs) but do not always import the funding cash transfers (deposits). This drops the database cash balance below zero.
*   **Auto-Correction:** The sync script automatically calculates the difference between the database cash balance and SimpleFIN's reported cash balance (the cash drift). It inserts a `CASH DRIFT` reconciliation transaction (an ADJUSTMENT activity) to correct this, restoring the correct positive balance.

### B. The TreasuryDirect I-Bond Account Case
*   **The Issue:** The `td-ibond` account shows:
    *   Cash Balance: `-$10,000`
    *   Investment Value: `$11,604`
    *   Total Net Value: `$1,604` (which should be `$11,604`)
*   **The Cause:** There is a single activity for `td-ibond` in the database:
    *   `sfin-ibond-buy | BUY | $10,000 | 1 unit | Asset: ASSET-IBOND`
    *   Because the purchase (BUY) was made without an initial funding `DEPOSIT` activity of `$10,000`, the account's cash balance went negative by `-$10,000`.
    *   Furthermore, since TreasuryDirect is an offline or non-standard holding, the sync script does not run a cash-drift adjustment for it.
*   **The Fix:** Manually insert a `DEPOSIT` activity of `$10,000` on `2022-01-01` into the `td-ibond` account. This will neutralize the `-$10,000` cash balance and make the total net value of the account display as the correct `$11,604`.
