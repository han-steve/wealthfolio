use crate::{context::ServiceContext, secret_store::shared_secret_store};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn set_secret(
    secret_key: String,
    secret: String,
    _state: State<'_, Arc<ServiceContext>>, // keep signature consistent
) -> Result<(), String> {
    shared_secret_store()
        .set_secret(&secret_key, &secret)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_secret(
    secret_key: String,
    _state: State<'_, Arc<ServiceContext>>,
) -> Result<Option<String>, String> {
    shared_secret_store()
        .get_secret(&secret_key)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_secret(
    secret_key: String,
    _state: State<'_, Arc<ServiceContext>>,
) -> Result<(), String> {
    shared_secret_store()
        .delete_secret(&secret_key)
        .map_err(|e| e.to_string())
}
