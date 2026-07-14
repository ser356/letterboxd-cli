//! GUI (Tauri) backend. Solo se compila con `--features gui` (activa por
//! defecto). Reutiliza los módulos existentes (`auth`, `letterboxd`,
//! `tmdb`, `recommend`, `credentials`) y los expone al frontend React
//! como `#[tauri::command]`.
//!
//! El frontend vive en `ui/` (Vite + React + TS + Tailwind v4). En dev,
//! Tauri arranca `npm run dev` en el puerto 1420. En release, sirve el
//! bundle de `ui/dist`.

use anyhow::Context;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

use crate::auth;
use crate::config::Config;
use crate::credentials::{self, Credentials};
use crate::letterboxd::LetterboxdClient;
use crate::progress::Progress;
use crate::recommend::{build_recommendations, Recommendation};
use crate::tmdb::TmdbClient;

/// Estado compartido: config actual (mutable si el user hace login) y un
/// cliente HTTP único.
struct AppState {
    config: Arc<Mutex<Config>>,
    http: reqwest::Client,
}

/// `Progress` no-op para uso desde el frontend (los eventos se emiten por
/// canal Tauri en una fase posterior; de momento la GUI llama y espera).
struct SilentProgress;
impl Progress for SilentProgress {
    fn stage(&self, _msg: &str, _total: u64) {}
    fn inc(&self) {}
    fn finish(&self) {}
}

// ------- Comandos expuestos al frontend -------

#[tauri::command]
async fn has_session(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    Ok(state.config.lock().await.refresh_token.is_some())
}

#[tauri::command]
async fn login(
    username: String,
    password: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let (client_id, client_secret) = {
        let cfg = state.config.lock().await;
        (cfg.client_id.clone(), cfg.client_secret.clone())
    };
    let res = auth::login_with_password(
        &state.http,
        &client_id,
        &client_secret,
        &username,
        &password,
    )
    .await
    .map_err(|e| e.to_string())?;

    let creds = Credentials {
        refresh_token: Some(res.refresh_token.clone()),
        username: Some(username.clone()),
    };
    credentials::save(&creds).map_err(|e| e.to_string())?;

    let mut cfg = state.config.lock().await;
    cfg.refresh_token = Some(res.refresh_token);
    cfg.username = username;
    Ok(())
}

#[tauri::command]
async fn get_recommendations(
    count: usize,
    min_rating: f32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Recommendation>, String> {
    let config = state.config.lock().await.clone();
    let token = auth::get_access_token(&state.http, &config)
        .await
        .map_err(|e| e.to_string())?;
    let lb = LetterboxdClient::new(&state.http, &token);
    let tmdb = TmdbClient::new(&state.http, &config.tmdb_bearer_token);
    build_recommendations(&lb, &tmdb, count, min_rating, &SilentProgress)
        .await
        .map_err(|e| e.to_string())
}

// ------- Entry point -------

/// Arranca la ventana Tauri. Devuelve el `Result` al `main.rs` para que
/// éste decida entre GUI / TUI / CLI antes de llamar aquí.
pub fn run(config: Config, http: reqwest::Client) -> anyhow::Result<()> {
    let state = AppState {
        config: Arc::new(Mutex::new(config)),
        http,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                // Fuerza la app a ejecutar como aplicación GUI regular
                // (con icono en el Dock y menú de app), aunque el binario
                // se haya lanzado desde la terminal. Sin esto, macOS trata
                // el proceso como background y la ventana no roba foco.
                app.set_activation_policy(tauri::ActivationPolicy::Regular);
            }
            Ok(())
        })
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            has_session,
            login,
            get_recommendations,
        ])
        .run(tauri::generate_context!())
        .context("Error al ejecutar la app Tauri")
}
