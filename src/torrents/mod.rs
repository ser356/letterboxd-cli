//! Búsqueda de torrents para películas.
//!
//! Define un trait `TorrentProvider` con implementaciones para varias fuentes
//! (YTS, Apibay, Knaben, Torznab). `search_all` las consulta en paralelo,
//! dedupe por infohash y ordena por seeders × calidad × idioma.

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

pub mod apibay;
pub mod knaben;
pub mod torznab;
pub mod yts;

#[derive(Debug, Clone, Serialize)]
pub struct Torrent {
    pub title: String,
    pub magnet: String,
    pub size_bytes: u64,
    pub seeders: u32,
    pub leechers: u32,
    pub quality: Option<String>,
    pub source: &'static str,
    /// Infohash extraído del magnet (para dedupe). No se serializa al JSON
    /// para no ensuciar la salida.
    #[serde(skip)]
    pub infohash: String,
}

#[derive(Debug, Clone, Default)]
pub struct MovieQuery {
    pub title: String,
    pub year: Option<u16>,
    pub imdb_id: Option<String>,
    /// TMDB ID. Actualmente ningún provider lo usa (todos aceptan IMDb o
    /// keywords), pero se acepta en la CLI para futuros providers.
    #[allow(dead_code)]
    pub tmdb_id: Option<u64>,
    /// Idioma original de la película (ISO 639-1: `"en"`, `"es"`, `"ru"`…).
    /// Se usa para rankear los torrents: los que llevan audio en este
    /// idioma (o "Original"/"Multi") suben en el score frente a doblajes.
    pub original_language: Option<String>,
}

impl MovieQuery {
    /// Cadena de búsqueda por defecto (para providers que no soportan IDs).
    #[allow(dead_code)]
    pub fn keywords(&self) -> String {
        match self.year {
            Some(y) => format!("{} {}", self.title, y),
            None => self.title.clone(),
        }
    }
}

#[async_trait]
pub trait TorrentProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn search(&self, http: &reqwest::Client, q: &MovieQuery) -> Result<Vec<Torrent>>;
}

/// Consulta a todos los providers en paralelo, dedupe por infohash, filtra por
/// seeders mínimos y ordena por score descendente. Los errores individuales
/// no abortan: se registran como warnings pero no rompen la búsqueda global.
pub async fn search_all(
    http: &reqwest::Client,
    providers: &[Arc<dyn TorrentProvider>],
    query: &MovieQuery,
    min_seeders: u32,
    limit: usize,
) -> Vec<Torrent> {
    let mut futs = FuturesUnordered::new();
    for p in providers {
        let p = Arc::clone(p);
        let http = http.clone();
        let query = query.clone();
        futs.push(async move {
            let name = p.name();
            let res = p.search(&http, &query).await;
            (name, res)
        });
    }

    // Dedupe por infohash, quedándonos con la entrada de más seeders.
    // Se hace en el mismo loop que consume los futures — evita un `Vec`
    // intermedio que en búsquedas amplias (miles de resultados de Knaben)
    // dispara reallocaciones inútiles.
    let mut best: HashMap<String, Torrent> = HashMap::new();
    while let Some((_name, res)) = futs.next().await {
        // Silenciamos errores individuales: si un provider está caído
        // (YTS a menudo, un Torznab local mal configurado, etc.) el
        // resto sigue funcionando. En la TUI no podemos hacer eprintln
        // porque corromperíamos la pantalla alternativa.
        let Ok(items) = res else { continue };
        for t in items {
            if t.infohash.is_empty() || t.seeders < min_seeders {
                continue;
            }
            match best.get_mut(&t.infohash) {
                Some(prev) if prev.seeders < t.seeders => *prev = t,
                Some(_) => {}
                None => {
                    best.insert(t.infohash.clone(), t);
                }
            }
        }
    }

    let mut out: Vec<Torrent> = best.into_values().collect();
    let orig_lang = query.original_language.as_deref();
    out.sort_by(|a, b| {
        score(a, orig_lang)
            .partial_cmp(&score(b, orig_lang))
            .map(|o| o.reverse())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    out
}

/// score = seeders * peso_calidad * peso_idioma.
/// Prioriza calidad razonable sin descartar releases con muchos seeders
/// aunque sean 720p/SD, y ANTEPONE audio original / multi a los doblajes
/// (los rusos de RuTracker son numerosos y saturan la lista si no se
/// castigan).
fn score(t: &Torrent, original_language: Option<&str>) -> f64 {
    let q_weight = match t.quality.as_deref() {
        Some(q) if q.contains("2160") || q.eq_ignore_ascii_case("4k") => 1.00,
        Some(q) if q.contains("1080") => 0.90,
        Some(q) if q.contains("720") => 0.60,
        Some(_) => 0.35,
        None => 0.50,
    };
    let hint = classify_audio(&t.title, original_language);
    let lang_weight = language_multiplier(hint);
    (t.seeders as f64) * q_weight * lang_weight
}

/// Peso de idioma en el score. `Original` y `Multi` son deseables (audio
/// original disponible); los doblajes se castigan para que no dominen el
/// ranking. `Unknown` queda en medio (no penaliza fuerte porque muchos
/// releases scene no marcan idioma en el título).
fn language_multiplier(hint: AudioHint) -> f64 {
    match hint {
        AudioHint::Original => 1.00,
        AudioHint::Multi => 0.90,
        AudioHint::Unknown => 0.55,
        AudioHint::Dubbed(_) => 0.25,
    }
}

/// Devuelve los providers habilitados por defecto. Torznab se activa si están
/// definidas `TORZNAB_URL` y `TORZNAB_APIKEY` en el entorno.
pub fn default_providers() -> Vec<Arc<dyn TorrentProvider>> {
    let mut providers: Vec<Arc<dyn TorrentProvider>> = vec![
        Arc::new(yts::Yts),
        Arc::new(knaben::Knaben),
        Arc::new(apibay::Apibay),
    ];

    if let (Ok(url), Ok(key)) = (
        std::env::var("TORZNAB_URL"),
        std::env::var("TORZNAB_APIKEY"),
    ) {
        providers.push(Arc::new(torznab::Torznab::new(url, key)));
    }

    providers
}

// ── Helpers públicos para los providers ─────────────────────────────────────

/// Extrae el infohash de un magnet link. Soporta btih hex y base32.
pub fn infohash_from_magnet(magnet: &str) -> String {
    // Formato típico: magnet:?xt=urn:btih:<HASH>&...
    magnet
        .split(&['?', '&'][..])
        .find_map(|kv| kv.strip_prefix("xt=urn:btih:"))
        .unwrap_or("")
        .split('&')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// Detecta calidad a partir del título del release.
pub fn quality_from_title(title: &str) -> Option<String> {
    let t = title.to_ascii_lowercase();
    for q in ["2160p", "1080p", "720p", "480p"] {
        if t.contains(q) {
            return Some(q.to_string());
        }
    }
    if t.contains("4k") {
        return Some("2160p".to_string());
    }
    None
}

/// Construye un magnet estándar a partir de un infohash y un display name.
pub fn build_magnet(infohash: &str, name: &str) -> String {
    const TRACKERS: &[&str] = &[
        "udp://tracker.opentrackr.org:1337/announce",
        "udp://tracker.openbittorrent.com:6969/announce",
        "udp://open.stealth.si:80/announce",
        "udp://exodus.desync.com:6969/announce",
    ];
    let mut m = format!(
        "magnet:?xt=urn:btih:{}&dn={}",
        infohash,
        urlencoding::encode(name)
    );
    for tr in TRACKERS {
        m.push_str("&tr=");
        m.push_str(&urlencoding::encode(tr));
    }
    m
}

/// Formato humano para bytes: "12.4 GB", "540 MB", "1.2 TB".
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}

// ── Detección de idioma de audio (heurística sobre el título) ───────────────

/// Pista sobre el audio de un release. Heurística basada en tokens habituales
/// del scene/P2P — no es 100% fiable pero acierta en la mayoría de casos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AudioHint {
    /// Muy probable audio original (idioma coincide con el de rodaje).
    Original,
    /// Doblado a un idioma concreto (ISO 639-1 aproximado).
    Dubbed(&'static str),
    /// Release con múltiples pistas de audio (incluye probablemente original).
    Multi,
    /// No hay pistas suficientes en el título.
    Unknown,
}

impl AudioHint {
    /// Etiqueta corta para UI (max 8 chars).
    pub fn badge(&self) -> &'static str {
        match self {
            AudioHint::Original => "orig",
            AudioHint::Dubbed("ru") => "dub-ru",
            AudioHint::Dubbed("es") => "dub-es",
            AudioHint::Dubbed("fr") => "dub-fr",
            AudioHint::Dubbed("it") => "dub-it",
            AudioHint::Dubbed("de") => "dub-de",
            AudioHint::Dubbed(_) => "dub",
            AudioHint::Multi => "multi",
            AudioHint::Unknown => "?",
        }
    }
}

/// Clasifica el audio de un release a partir de su título y del idioma
/// original de la película (del `original_language` de TMDB).
///
/// Reglas clave:
/// * Si el título tiene marcadores multi-audio explícitos (MULTI, dual,
///   `[EN+RUS]`…) → `Multi`. `WEB-DL` y variantes NO cuentan como
///   multi-audio: es un marcador de fuente, no de idioma.
/// * Si el título lleva un idioma detectable, se compara con
///   `original_language`: si coincide es `Original`, si difiere es
///   `Dubbed(iso)`. Esto evita castigar releases castellanos de
///   películas españolas, italianos de películas italianas, etc.
/// * Cirílico en el título se trata como pista de idioma ruso.
/// * Si no aparece ningún marcador y el release no lleva cirílico, se
///   asume audio original (default del scene internacional).
pub fn classify_audio(title: &str, original_language: Option<&str>) -> AudioHint {
    let t = title.to_lowercase();
    let has_cyrillic = title
        .chars()
        .any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));
    let ol = original_language
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    // Multi-audio explícito. NOTA: NO usamos `.dl.` / ` dl ` como se
    // hacía antes: matcheaba WEB.DL (variante extendida de WEB-DL) y
    // marcaba todos esos releases como multi. Si hace falta detectar
    // German Dual, usa marcadores explícitos (`dual`, `.multi.`, etc.).
    if t.contains("multi")
        || t.contains("dual audio")
        || t.contains("dual-audio")
        || t.contains("dualaudio")
        || t.contains(" da2 ")
        || t.contains(" 2audio")
        || multi_language_bracket(&t)
    {
        return AudioHint::Multi;
    }

    // Detectamos el idioma de audio con la primera regla que matchee.
    // Si coincide con el idioma original de la peli → `Original`; si no
    // → `Dubbed(iso)`. Esto arregla el bug histórico de castigar
    // releases castellanos de pelis españolas.
    let detected: Option<&'static str> = if has_cyrillic {
        Some("ru")
    } else if t.contains("castellano")
        || t.contains("espanol")
        || t.contains("español")
        || t.contains("spanish")
        || t.contains(" esp ")
        || t.contains("[esp]")
        || t.contains("latino")
    {
        Some("es")
    } else if t.contains(" ita ") || t.contains("italian") {
        Some("it")
    } else if t.contains(" fra ") || t.contains("french") {
        Some("fr")
    } else if t.contains(" ger ") || t.contains("german") || t.contains("deutsch") {
        Some("de")
    } else {
        None
    };

    if let Some(iso) = detected {
        return if ol == iso {
            AudioHint::Original
        } else {
            AudioHint::Dubbed(iso)
        };
    }

    // Marcador genérico "dub" sin idioma identificado.
    if t.contains(" dub") || t.contains(".dub.") || t.ends_with(" dub") {
        return AudioHint::Dubbed("??");
    }

    // Sin marcadores: asumimos audio original. En releases scene
    // internacionales el default es "idioma original de la peli".
    AudioHint::Original
}

/// Detecta patrones tipo `[ENG+RUS]`, `[EN.RU.ES]`, `[EN/FR]` en el título:
/// dos o más códigos de idioma ISO 639-1/-2 dentro del mismo bracket o
/// grupo entre puntos.
///
/// Exige que cada código tenga *frontera de palabra* delante y detrás
/// (separador o borde de grupo) para no contar coincidencias falsas como
/// `en` dentro de `Golden`. Trabaja sobre bytes ASCII sin re-allocar.
fn multi_language_bracket(t: &str) -> bool {
    // Nota: `t` viene ya en minúsculas del caller. No re-lowercase.
    const LANG_CODES: &[&str] = &[
        "eng", "en", "rus", "ru", "esp", "spa", "es", "fre", "fra", "fr", "ita", "it", "ger",
        "deu", "de", "por", "pt", "jpn", "ja", "chi", "zh", "kor", "ko",
    ];
    let bytes = t.as_bytes();
    let mut in_group = false;
    let mut count = 0u8;
    let mut prev_is_sep = true;

    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'[' || ch == b'(' {
            in_group = true;
            count = 0;
            prev_is_sep = true;
            i += 1;
            continue;
        }
        if ch == b']' || ch == b')' {
            if count >= 2 {
                return true;
            }
            in_group = false;
            prev_is_sep = true;
            i += 1;
            continue;
        }
        if !in_group {
            prev_is_sep = !ch.is_ascii_alphabetic();
            i += 1;
            continue;
        }
        // Dentro de un grupo: intentamos matchear un código de idioma
        // solo si venimos de un separador y este byte es alfabético
        // (para no partir palabras como "Golden").
        let is_alpha = ch.is_ascii_alphabetic();
        let mut advance = 1usize;
        if prev_is_sep && is_alpha {
            for code in LANG_CODES {
                let end = i + code.len();
                if end <= bytes.len() && &bytes[i..end] == code.as_bytes() {
                    let after = bytes.get(end).copied().unwrap_or(b' ');
                    if !after.is_ascii_alphabetic() {
                        count = count.saturating_add(1);
                        advance = code.len();
                        break;
                    }
                }
            }
        }
        // Si consumimos un código completo, el byte que sigue es
        // separador por construcción (lo verificamos arriba con `after`).
        prev_is_sep = if advance == 1 { !is_alpha } else { true };
        i += advance;
    }
    false
}

// ---- Helpers compartidos entre providers / vistas ----

/// Si `s` acaba en un año de 4 dígitos (1888-2100) separado por espacio,
/// devuelve `(título_sin_año, Some(año))`. Si no, `(s, None)`.
///
/// Safe para entradas no-ASCII: usa `rfind(' ')` en lugar de `split_at`
/// por bytes (la variante anterior paniqueaba con títulos cirílicos como
/// "Амели" cuando el offset caía en mitad de un char multibyte).
pub fn split_trailing_year(s: &str) -> (String, Option<u16>) {
    let s = s.trim();
    if let Some(idx) = s.rfind(' ') {
        let tail = &s[idx + 1..];
        if tail.len() == 4 {
            if let Ok(y) = tail.parse::<u16>() {
                if (1888..=2100).contains(&y) {
                    return (s[..idx].trim().to_string(), Some(y));
                }
            }
        }
    }
    (s.to_string(), None)
}

/// Comprueba si el título de un release EMPIEZA con `needle` (case-
/// insensitive). Ignora caracteres no alfanuméricos al principio de
/// ambos lados (comillas, corchetes, guiones…). Usado por el fallback
/// ruso para descartar releases que solo *mencionan* el título en su
/// descripción en lugar de empezar por él.
pub fn release_starts_with(release: &str, needle: &str) -> bool {
    let release = release.to_lowercase();
    let release = release.trim_start_matches(|c: char| !c.is_alphanumeric());
    let needle = needle.to_lowercase();
    let needle = needle.trim_start_matches(|c: char| !c.is_alphanumeric());
    release.starts_with(needle.trim())
}

/// Extrae años (1900-2099) del título del release y comprueba si alguno
/// está dentro de ±`tolerance` del año buscado. Si el release no incluye
/// ningún año, se acepta (no podemos discriminar y es preferible un
/// falso positivo a perder el hit).
pub fn release_matches_year(title: &str, target: u16, tolerance: u16) -> bool {
    let mut has_year = false;
    for token in title.split(|c: char| !c.is_alphanumeric()) {
        if token.len() != 4 {
            continue;
        }
        if let Ok(y) = token.parse::<u16>() {
            if (1900..=2099).contains(&y) {
                has_year = true;
                if (target as i32 - y as i32).unsigned_abs() as u16 <= tolerance {
                    return true;
                }
            }
        }
    }
    !has_year
}
