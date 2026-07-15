//! Descartes ("no sugerir") persistidos por el usuario desde la GUI.
//!
//! Vive en `~/.config/videodrome/dismissed.json`. Guardamos title +
//! poster_path junto al TMDB id para poder pintar el panel de "Restaurar"
//! en Ajustes sin tener que refetchar TMDB por cada entrada descartada.
//!
//! Solo se usa desde el backend GUI (`#[cfg(feature = "gui")]`); el CLI
//! y la TUI no filtran por esto — mantienen el comportamiento clásico.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

const DISMISSED_FILE: &str = "dismissed.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissedEntry {
    pub id: u64,
    pub title: String,
    pub poster_path: Option<String>,
    /// Epoch UNIX en segundos.
    pub dismissed_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dismissed {
    #[serde(default)]
    pub entries: Vec<DismissedEntry>,
}

impl Dismissed {
    pub fn ids(&self) -> HashSet<u64> {
        self.entries.iter().map(|e| e.id).collect()
    }

    pub fn contains(&self, id: u64) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    /// Añade una entrada si no existía; no-op si ya estaba.
    pub fn insert(&mut self, entry: DismissedEntry) {
        if !self.contains(entry.id) {
            self.entries.push(entry);
        }
    }

    /// Elimina por id. Devuelve `true` si estaba presente.
    pub fn remove(&mut self, id: u64) -> bool {
        let len = self.entries.len();
        self.entries.retain(|e| e.id != id);
        self.entries.len() != len
    }
}

fn dismissed_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("No se puede obtener el directorio de configuración")?
        .join("videodrome");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(DISMISSED_FILE))
}

pub fn load() -> Dismissed {
    let Ok(path) = dismissed_path() else {
        return Dismissed::default();
    };
    let Ok(data) = std::fs::read_to_string(path) else {
        return Dismissed::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save(d: &Dismissed) -> Result<()> {
    let path = dismissed_path()?;
    let json = serde_json::to_string_pretty(d).context("Error al serializar dismissed.json")?;
    std::fs::write(path, json).context("Error al escribir dismissed.json")?;
    Ok(())
}
