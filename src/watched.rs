//! Pelis marcadas como "ya vista" desde la GUI.
//!
//! Análogo a `dismissed.rs` pero con semántica distinta: "no sugerir"
//! (dismiss) = "no me interesa, no me la enseñes"; "vista" (watched) =
//! "ya la he visto, no la sugieras más". Se guardan por separado para
//! que Ajustes pueda ofrecer dos catálogos independientes ("Restaurar
//! sugerencias" vs "Catálogo de vistas") y el user pueda vaciar cada
//! uno sin tocar el otro.
//!
//! Ambos sets se unen en `get_recommendations_page` como filtro sobre
//! el pool cacheado.
//!
//! Solo se usa desde el backend GUI (`#[cfg(feature = "gui")]`); el CLI
//! y la TUI no filtran por esto — mantienen el comportamiento clásico.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

const WATCHED_FILE: &str = "watched.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedEntry {
    pub id: u64,
    pub title: String,
    pub poster_path: Option<String>,
    /// Epoch UNIX en segundos.
    pub watched_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Watched {
    #[serde(default)]
    pub entries: Vec<WatchedEntry>,
}

impl Watched {
    pub fn ids(&self) -> HashSet<u64> {
        self.entries.iter().map(|e| e.id).collect()
    }

    pub fn contains(&self, id: u64) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    /// Añade una entrada si no existía; no-op si ya estaba.
    pub fn insert(&mut self, entry: WatchedEntry) {
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

fn watched_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("No se puede obtener el directorio de configuración")?
        .join("videodrome");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(WATCHED_FILE))
}

pub fn load() -> Watched {
    let Ok(path) = watched_path() else {
        return Watched::default();
    };
    let Ok(data) = std::fs::read_to_string(path) else {
        return Watched::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save(w: &Watched) -> Result<()> {
    let path = watched_path()?;
    let json = serde_json::to_string_pretty(w).context("Error al serializar watched.json")?;
    std::fs::write(path, json).context("Error al escribir watched.json")?;
    Ok(())
}

/// Borra por completo el fichero (o lo deja como `{"entries":[]}`).
/// Alimenta el botón "Vaciar catálogo" de Ajustes.
pub fn clear() -> Result<()> {
    save(&Watched::default())
}
