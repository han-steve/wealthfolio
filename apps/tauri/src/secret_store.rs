use std::sync::Arc;

use keyring::Entry;

use wealthfolio_core::{
    errors::Error,
    secrets::{format_service_id, SecretStore},
    Result,
};

const USERNAME: &str = "default";

#[derive(Debug, Default)]
pub struct KeyringSecretStore;

impl SecretStore for KeyringSecretStore {
    fn set_secret(&self, service: &str, secret: &str) -> Result<()> {
        let entry = entry_for(service)?;
        entry
            .set_password(secret)
            .map_err(|err| Error::Secret(err.to_string()))
    }

    fn get_secret(&self, service: &str) -> Result<Option<String>> {
        let entry = entry_for(service)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(Error::Secret(err.to_string())),
        }
    }

    fn delete_secret(&self, service: &str) -> Result<()> {
        let entry = entry_for(service)?;
        match entry.delete_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(Error::Secret(err.to_string())),
        }
    }
}

fn entry_for(service: &str) -> Result<Entry> {
    let service_id = format_service_id(service);
    Entry::new(&service_id, USERNAME).map_err(|err| Error::Secret(err.to_string()))
}

/// File-based secret store for macOS desktop.
/// Avoids Keychain popups caused by ad-hoc code signing during development.
/// Secrets are stored in a JSON file inside the app data directory.
#[cfg(target_os = "macos")]
mod file_store {
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use wealthfolio_core::{
        errors::Error,
        secrets::{format_service_id, SecretStore},
        Result,
    };

    #[derive(Debug)]
    pub struct FileSecretStore {
        path: PathBuf,
        lock: Mutex<()>,
    }

    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct Store {
        secrets: HashMap<String, String>,
    }

    impl FileSecretStore {
        pub fn new(app_data_dir: &str) -> Self {
            let path = PathBuf::from(app_data_dir).join("secrets.json");
            Self {
                path,
                lock: Mutex::new(()),
            }
        }

        fn load(&self) -> Store {
            if !self.path.exists() {
                return Store::default();
            }
            match fs::read_to_string(&self.path) {
                Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
                Err(_) => Store::default(),
            }
        }

        fn save(&self, store: &Store) -> Result<()> {
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| Error::Secret(format!("mkdir: {}", e)))?;
            }
            let json = serde_json::to_string_pretty(store)
                .map_err(|e| Error::Secret(format!("serialize: {}", e)))?;
            fs::write(&self.path, json)
                .map_err(|e| Error::Secret(format!("write: {}", e)))?;
            Ok(())
        }
    }

    impl SecretStore for FileSecretStore {
        fn set_secret(&self, service: &str, secret: &str) -> Result<()> {
            let _guard = self.lock.lock().map_err(|_| Error::Secret("lock".into()))?;
            let key = format_service_id(service);
            let mut store = self.load();
            store.secrets.insert(key, secret.to_string());
            self.save(&store)
        }

        fn get_secret(&self, service: &str) -> Result<Option<String>> {
            let _guard = self.lock.lock().map_err(|_| Error::Secret("lock".into()))?;
            let key = format_service_id(service);
            let store = self.load();
            Ok(store.secrets.get(&key).cloned())
        }

        fn delete_secret(&self, service: &str) -> Result<()> {
            let _guard = self.lock.lock().map_err(|_| Error::Secret("lock".into()))?;
            let key = format_service_id(service);
            let mut store = self.load();
            store.secrets.remove(&key);
            self.save(&store)
        }
    }
}

use std::sync::OnceLock;

static SECRET_STORE: OnceLock<Arc<dyn SecretStore>> = OnceLock::new();

/// Initialize the shared secret store with the app data directory.
/// Must be called once during app startup before any secret access.
#[cfg(target_os = "macos")]
pub fn init_secret_store(app_data_dir: &str) {
    let store = Arc::new(file_store::FileSecretStore::new(app_data_dir));
    // Migrate existing keychain secrets to file store (one-time)
    migrate_keychain_to_file(&*store);
    // Migrate identity from the legacy app bundle (com.teymz.wealthfolio → com.wealthfolio.app)
    migrate_legacy_bundle_secrets(app_data_dir, &*store);
    let _ = SECRET_STORE.set(store);
}

/// One-time migration: copy secrets from macOS Keychain to file store,
/// then delete from Keychain to stop the popup prompts.
#[cfg(target_os = "macos")]
fn migrate_keychain_to_file(file_store: &dyn SecretStore) {
    use log::info;
    // Only migrate long-lived secrets. access_token is short-lived and is
    // always re-fetched from the refresh_token — including it here would
    // cause a keychain prompt every launch if the token isn't persisted.
    let keys = ["sync_identity", "sync_refresh_token"];
    let keyring_store = KeyringSecretStore;
    for key in &keys {
        // Check file store first — if already present, skip Keychain entirely
        // to avoid triggering macOS permission prompts on every launch.
        if file_store.get_secret(key).ok().flatten().is_some() {
            continue;
        }
        match keyring_store.get_secret(key) {
            Ok(Some(value)) => {
                if file_store.set_secret(key, &value).is_ok() {
                    let _ = keyring_store.delete_secret(key);
                    info!("Migrated secret '{}' from Keychain to file store", key);
                }
            }
            _ => {}
        }
    }
}

/// One-time migration: copy long-lived secrets from the legacy app bundle
/// (com.teymz.wealthfolio) to the new bundle (com.wealthfolio.app).
/// This handles the case where the user updated from the old bundle ID.
#[cfg(target_os = "macos")]
fn migrate_legacy_bundle_secrets(current_app_data_dir: &str, new_store: &dyn SecretStore) {
    use log::info;
    // Derive the old bundle path by replacing the new bundle name with the legacy one.
    // Both live under ~/Library/Application Support/<bundle-id>.
    let current_path = std::path::Path::new(current_app_data_dir);
    let parent = match current_path.parent() {
        Some(p) => p,
        None => return,
    };
    let legacy_dir = parent.join("com.teymz.wealthfolio");
    if !legacy_dir.exists() {
        return;
    }
    let old_store = file_store::FileSecretStore::new(legacy_dir.to_str().unwrap_or(""));
    let keys = ["sync_identity", "sync_refresh_token"];
    for key in &keys {
        // Only migrate if the new store doesn't already have this key.
        if new_store.get_secret(key).ok().flatten().is_some() {
            continue;
        }
        if let Ok(Some(value)) = old_store.get_secret(key) {
            if new_store.set_secret(key, &value).is_ok() {
                info!("Migrated secret '{}' from legacy bundle to new bundle", key);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn init_secret_store(_app_data_dir: &str) {
    let _ = SECRET_STORE.set(Arc::new(KeyringSecretStore));
}

pub fn shared_secret_store() -> Arc<dyn SecretStore> {
    SECRET_STORE
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(KeyringSecretStore))
}
