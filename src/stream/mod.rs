//! Streaming BitTorrent al estilo Stremio (rudimentario): mientras se
//! descarga el fichero, se sirve por HTTP con soporte de Range para que
//! VLC (u otro reproductor) lo pueda reproducir progresivamente.
//!
//! Bajo el capó usa `librqbit` como motor BitTorrent embebido:
//! `handle.stream(file_id)` devuelve un `FileStream` que implementa
//! `AsyncRead + AsyncSeek`. Cada `read()` bloquea hasta que la pieza
//! necesaria está descargada, y registra el rango deseado con el piece
//! picker, que prioriza esas piezas — de facto es "descarga secuencial +
//! primera/última pieza primero" cuando VLC pide byte 0 (cabecera) y luego
//! byte final (para índice `mp4`/`mkv` en algunos casos).
//!
//! ## Caché persistente
//!
//! El fichero se escribe bajo `<cache>/videodrome/streams/<infohash>/` en
//! lugar de un tempdir efímero. Al re-abrir la misma peli, librqbit
//! verifica las piezas ya presentes en disco y arranca casi al instante
//! (sin re-bajar). Si el magnet no expone infohash (raro), se cae a un
//! tempdir tradicional que sí se borra al salir.
//!
//! Cada entrada guarda un fichero `.last_used` que se toca al start y al
//! drop del `StreamHandle`; el módulo `prune` borra las entradas cuyo
//! mtime supere el TTL (configurable en Preferences, default 7 días).
//!
//! ## Resume position
//!
//! El handler HTTP registra el mayor `start` de cada Range con start
//! explícito (los suffix ranges de índice se ignoran) en un `AtomicU64`.
//! Al hacer Drop del `StreamHandle`, se persiste `resume.json` con la
//! fracción `max_seek / file_len`. El caller (GUI) puede leerla con
//! `load_resume(infohash)` y pasar `start_seconds` a `open_in_vlc` para
//! que VLC arranque con `--start-time=<seg>`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session, SessionOptions,
    SessionPersistenceConfig,
};
use tempfile::TempDir;
use tokio::io::{AsyncSeekExt, SeekFrom};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

// El trait solo se usa en spawns Windows y en helpers gui-only
// (spawn_hls, serve_embedded_subtitle). En la build CLI/TUI puro
// para macOS/Linux queda sin call sites y warnaría `unused_imports`.
#[allow(unused_imports)]
use crate::winutil::HideConsoleExt;

// ── Submódulos del refactor ──────────────────────────────────
//
// Extraídos de un fichero monolítico en el paso 2 del split. La
// API pública se re-exporta desde `mod.rs` para que gui.rs / tui.rs
// no cambien.
mod cache;
#[cfg(feature = "gui")]
mod hls;
mod resume;
mod state;
mod vlc;

#[allow(unused_imports)]
pub use cache::{cache_dir, clear_all, parse_infohash, prune, prune_orphan_tempdirs, total_size};
#[allow(unused_imports)]
pub use resume::{load_resume, load_resume_any, save_position, Resume, ResumeEpisode};
#[cfg(feature = "gui")]
pub use state::set_client_capabilities;
#[allow(unused_imports)]
pub use vlc::{open_in_vlc, PlayerHandle};

use self::cache::{now_unix, touch_last_used};
#[cfg(feature = "gui")]
use self::hls::{ensure_hls_dir, serve_hls_playlist, serve_hls_segment};
use self::resume::{read_store, write_store_atomic, ResumeParse, ResumeStore, RESUME_FILE};
#[cfg(feature = "gui")]
use self::state::current_client_capabilities;
use self::state::AppState;

/// Subdirectorio (dentro de `<cache>/streams/<infohash>/`) donde
/// librqbit persiste su estado por-torrent para fastresume: el
/// `session.json` (índice) + `<hash>.bitv` (bitfield de piezas
/// completadas) + `<hash>.torrent` (metainfo). Sin esto, cada
/// apertura del mismo torrent re-hashea el fichero entero antes de
/// hacer NADA (audit §1: ~20 s para 10.5 GiB, proporcional al
/// tamaño → ~2 min en un remux UHD de 60 GB).
///
/// Colocarlo DENTRO del dir del infohash es intencional:
/// `clear_all()` y `prune()` borran ese dir por completo, así que
/// el estado se limpia solo cuando limpiamos la caché. Sin trabajo
/// extra ni riesgo de fastresume apuntando a ficheros ya borrados.
const LIBRQBIT_SESSION_SUBDIR: &str = ".session";

/// Handle de una sesión de streaming activa. `Drop` cancela el servidor
/// HTTP, detiene la sesión BitTorrent y — si tenemos infohash — persiste
/// el `resume.json` con la fracción de reproducción alcanzada. Los
/// ficheros de vídeo se conservan en la caché (`streams/<infohash>/`)
/// para acelerar la siguiente reproducción. Solo se borran cuando el
/// magnet no tenía infohash (fallback a tempdir) o cuando el prune por
/// TTL los recoge.
pub struct StreamHandle {
    pub url: String,
    pub file_name: String,
    pub file_len: u64,
    /// Índice del fichero de vídeo dentro del torrent multi-file. Se
    /// usa para llamar `handle.stream(file_id)` desde fuera del
    /// módulo (p.ej. `compute_moviehash`). Solo consumido con feature
    /// `gui`; en CLI/TUI el streaming va por VLC directo y no hace
    /// falta.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub file_id: usize,
    /// Infohash (hex-lowercase o base32) extraído del magnet, si se
    /// pudo parsear. Los callers lo usan para llamar a `load_resume`.
    pub infohash: Option<String>,
    handle: Arc<ManagedTorrent>,
    cancel: CancellationToken,
    /// Mayor `start` (bytes) visto en un Range HTTP con start explícito.
    /// Los suffix ranges (índice al final del MP4) no lo tocan.
    max_seek: Arc<AtomicU64>,
    /// Directorio de datos del torrent. Persistente cuando hay infohash;
    /// tempdir cuando no.
    data_dir: PathBuf,
    _session: Arc<Session>,
    /// `Some` cuando el magnet no tenía infohash y caemos a tempdir
    /// efímero. `None` cuando usamos caché persistente.
    _tempdir: Option<TempDir>,
    _server_task: JoinHandle<()>,
}

/// Snapshot del progreso de un stream en curso.
pub struct StreamStats {
    pub progress_bytes: u64,
    pub total_bytes: u64,
    pub live_peers: u32,
    pub down_mbps: f64,
}

impl StreamHandle {
    pub fn stats(&self) -> StreamStats {
        let s = self.handle.stats();
        let down_mbps = self
            .handle
            .live()
            .map(|l| l.down_speed_estimator().mbps())
            .unwrap_or(0.0);
        let live_peers = s
            .live
            .as_ref()
            .map(|l| l.snapshot.peer_stats.live as u32)
            .unwrap_or(0);
        StreamStats {
            progress_bytes: s.progress_bytes,
            total_bytes: s.total_bytes,
            live_peers,
            down_mbps,
        }
    }

    /// Clona el `Arc<ManagedTorrent>` interno. Los callers que quieran
    /// hacer `compute_moviehash` (free function del módulo) sin
    /// retener el `MutexGuard` del map de streams (para no bloquear
    /// stats/stop) extraen las 3 piezas dentro del lock y ejecutan el
    /// cómputo fuera. Solo se usa desde la GUI.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn torrent_arc(&self) -> Arc<ManagedTorrent> {
        self.handle.clone()
    }
}

/// Free-function variante de `StreamHandle::compute_moviehash`: útil
/// cuando el caller ya ha soltado el lock del map de streams pero
/// conserva las 3 piezas necesarias (Arc del ManagedTorrent + file id
/// + file len). Evita retener el `MutexGuard` durante el await, que
///   bloquearía otras operaciones sobre el map de streams (stats, stop).
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub async fn compute_moviehash(
    handle: Arc<ManagedTorrent>,
    file_id: usize,
    file_len: u64,
) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
    const CHUNK: u64 = 65536;
    if file_len < CHUNK * 2 {
        return None;
    }
    let fut = async move {
        let mut stream = handle.stream(file_id).ok()?;
        let mut first = vec![0u8; CHUNK as usize];
        stream.read_exact(&mut first).await.ok()?;
        stream.seek(SeekFrom::Start(file_len - CHUNK)).await.ok()?;
        let mut last = vec![0u8; CHUNK as usize];
        stream.read_exact(&mut last).await.ok()?;
        crate::subtitles::compute_moviehash(file_len, &first, &last)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(10), fut).await {
        Ok(res) => res,
        Err(_) => {
            tracing::warn!(target: "subs", "compute_moviehash timeout at 10s (peers lentos)");
            None
        }
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        // Persistir resume ANTES de cancelar la sesión — la escritura es
        // síncrona y solo toca `<data_dir>/resume.json`, que no depende
        // del motor de librqbit.
        //
        // Merge-style con resiliencia a corrupción: si el player HTML
        // llamó a `save_position`, tendrá `seconds`+`duration_seconds`
        // que NO queremos pisar. Si el fichero existe pero no parsea
        // (write parcial anterior), NO lo sobreescribimos — mejor
        // dejar el corrupto que reemplazarlo por un default limpio
        // que pierde toda la info previa. Solo escribimos si podemos
        // hacer un merge honesto.
        //
        // Multi-file (§6 audit): escribimos SOLO la entrada
        // `files["<file_id>"]` del store — otras entradas del mismo
        // torrent (otros episodios) sobreviven intactas.
        if let Some(hash) = self.infohash.as_deref() {
            let max = self.max_seek.load(Ordering::Relaxed);
            if self.file_len > 0 {
                let fraction = (max as f32 / self.file_len as f32).clamp(0.0, 1.0);
                let path = self.data_dir.join(RESUME_FILE);
                let existing = match read_store(&path) {
                    ResumeParse::Store(s) => Some(s),
                    ResumeParse::Absent => Some(ResumeStore::default()),
                    ResumeParse::Corrupt => None,
                };
                if let Some(mut store) = existing {
                    let key = self.file_id.to_string();
                    let mut entry_r = store.files.remove(&key).unwrap_or_default();
                    entry_r.fraction = fraction;
                    entry_r.updated_at = now_unix();
                    store.files.insert(key, entry_r);
                    if let Err(e) = write_store_atomic(&path, &store) {
                        tracing::warn!(target: "resume", error = %e, "Drop: atomic write failed");
                    }
                }
            }
            // Tocar el sentinel para que el prune vea "usado ahora".
            let _ = touch_last_used(&self.data_dir);
            let _ = hash; // solo lo usamos para saber que la caché es persistente
        }
        self.cancel.cancel();
    }
}

/// Lista de trackers públicos que se inyectan en cada torrent. Muchos
/// magnets vienen con lista de `tr=` casi vacía (o solo con trackers
/// caídos), y sin trackers ni DHT rápido el motor se queda esperando peers
/// para siempre. Estos son de la lista comunitaria "trackerslist" (los más
/// vivos y con más torrents anunciados).
const EXTRA_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker.moeking.me:6969/announce",
    "udp://explodie.org:6969/announce",
    "udp://ipv4.tracker.harry.lu:80/announce",
    "udp://tracker.dler.org:6969/announce",
    "udp://p4p.arenabg.com:1337/announce",
    "udp://tracker.tiny-vps.com:6969/announce",
    "udp://retracker.lanta-net.ru:2710/announce",
    "http://tracker.opentrackr.org:1337/announce",
];

/// Cuánto esperamos a que el magnet resuelva metadata antes de rendirnos.
const METADATA_TIMEOUT_SECS: u64 = 45;

/// Extensiones consideradas "vídeo" a la hora de elegir fichero
/// dentro de un torrent multi-file. El resto se ignora (samples,
/// extras, nfo).
const VIDEO_EXTS: &[&str] = &["mkv", "mp4", "avi", "m4v", "ts", "webm", "mov", "wmv"];

/// Tamaño mínimo para considerar un fichero "de contenido" y no
/// sample. 50 MB es el umbral que la scene usa históricamente.
const MIN_VIDEO_SIZE_BYTES: u64 = 50 * 1024 * 1024;

/// Info por-fichero devuelta al frontend por `list_files` para que
/// pueda ofrecer selección manual (packs con numeración absoluta de
/// anime, encoding raro, etc.). Serialized snake_case para el
/// consumidor JS.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TorrentFileInfo {
    pub file_id: usize,
    pub name: String,
    pub size: u64,
    pub season: Option<u16>,
    pub episode: Option<u16>,
    /// Es candidato realista a "el vídeo del episodio" (extensión
    /// vídeo + tamaño > sample). El frontend puede filtrar por esto.
    pub is_video: bool,
}

/// Elige el fichero a servir dentro de la lista de un torrent.
///
/// * `target = None` → el vídeo más grande (comportamiento pre-audit,
///   correcto para películas y torrents mono-fichero).
/// * `target = Some(Episode(S, E))` → parsea cada nombre con
///   `release_name::parse` y elige el que matchee S+E. Si varios
///   matchean (mismo episodio en calidades duplicadas), el más
///   grande de ellos. Si ninguno matchea, cae al mayor — así una
///   heurística de S/E fallida no bloquea el arranque.
/// * `target = Some(Index(i))` → devuelve directo `files[i]` (con
///   bounds check). Se usa cuando el provider ya resolvió el índice
///   (Torrentio.fileIdx) y saltarnos el parser evita el edge case de
///   packs con numeración absoluta de anime.
///
/// Filtra ficheros de tamaño < 50 MB para no picar samples/extras.
pub fn select_file(
    files: &[(usize, String, u64)],
    target: Option<crate::torrents::FileSelector>,
) -> Option<(usize, String, u64)> {
    use crate::torrents::FileSelector;

    // Índice directo: el provider ya nos dijo cuál. Bypass del
    // filtro de samples porque el proveedor sabe mejor que la
    // heurística "> 50 MB" cuando el fichero elegido es válido.
    if let Some(FileSelector::Index(i)) = target {
        if let Some(f) = files.iter().find(|(id, _, _)| *id == i) {
            return Some(f.clone());
        }
        // Fuera de rango: cae al mayor. Mejor un fichero incorrecto
        // que un error duro.
    }

    // Vídeos "reales" (ext conocida + tamaño > sample). Si el filtro
    // deja lista vacía (torrent con nombres no estándar), volvemos al
    // set completo antes de descartar.
    let is_video = |name: &str, size: u64| {
        size >= MIN_VIDEO_SIZE_BYTES
            && std::path::Path::new(name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| VIDEO_EXTS.contains(&e))
                .unwrap_or(false)
    };
    let candidates: Vec<&(usize, String, u64)> =
        files.iter().filter(|(_, n, s)| is_video(n, *s)).collect();
    let pool: Vec<&(usize, String, u64)> = if candidates.is_empty() {
        files.iter().collect()
    } else {
        candidates
    };

    if let Some(FileSelector::Episode(qs, qe)) = target {
        let matches: Vec<&&(usize, String, u64)> = pool
            .iter()
            .filter(|(_, n, _)| {
                let p = crate::torrents::release_name::parse(n);
                matches!((p.season, p.episode), (Some(ps), Some(pe)) if ps == qs && pe == qe)
            })
            .collect();
        if let Some(best) = matches.iter().max_by_key(|(_, _, s)| *s) {
            return Some((***best).clone());
        }
        // Sin match exacto: fallback al mayor del pool (mismo que sin
        // target). Mejor un fichero incorrecto que un error duro —
        // el user puede seleccionar manual con `list_files`.
    }

    pool.iter()
        .max_by_key(|(_, _, s)| *s)
        .map(|f| (**f).clone())
}

/// Lista los ficheros del torrent (resolviendo metadata) sin
/// arrancar servidor HTTP ni empezar a bajar contenido. Útil para
/// que la UI ofrezca selección manual en packs con nombres raros.
///
/// La sesión se dropea al retornar — no deja recursos vivos. Usa la
/// misma caché persistente que `start` (mismo `<cache>/streams/<hash>/`),
/// así que si el user llama a esto y después a `start` sobre el
/// mismo magnet, librqbit reutiliza lo bajado.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub async fn list_files(magnet: String) -> Result<Vec<TorrentFileInfo>> {
    let infohash = parse_infohash(&magnet);
    let (data_dir, _tempdir_guard) = match infohash.as_deref() {
        Some(hash) => {
            let dir = cache_dir()?.join(hash);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("No se pudo crear {}", dir.display()))?;
            (dir, None)
        }
        None => {
            let td = tempfile::Builder::new()
                .prefix("videodrome-listfiles-")
                .tempdir()
                .context("No se pudo crear directorio temporal")?;
            (td.path().to_path_buf(), Some(td))
        }
    };

    let cancel = CancellationToken::new();
    let session = Session::new_with_opts(
        data_dir.clone(),
        SessionOptions {
            disable_dht_persistence: true,
            persistence: None,
            cancellation_token: Some(cancel.clone()),
            ..Default::default()
        },
    )
    .await
    .context("Error inicializando la sesión de librqbit")?;

    let response = session
        .add_torrent(
            AddTorrent::from_url(&magnet),
            Some(AddTorrentOptions {
                overwrite: true,
                // Modo list-only: no arranca la descarga, solo pide
                // metadata. Al drop de `session`, no queda nada
                // corriendo. `paused: true` sería otra opción pero
                // reutilizamos la ruta normal para simplicidad.
                paused: true,
                trackers: Some(EXTRA_TRACKERS.iter().map(|s| s.to_string()).collect()),
                ..Default::default()
            }),
        )
        .await
        .context("Error al añadir el torrent")?;

    let handle: Arc<ManagedTorrent> = match response {
        AddTorrentResponse::Added(_, h) => h,
        AddTorrentResponse::AlreadyManaged(_, h) => h,
        AddTorrentResponse::ListOnly(_) => anyhow::bail!("Torrent en modo list-only"),
    };

    tokio::time::timeout(
        std::time::Duration::from_secs(METADATA_TIMEOUT_SECS),
        handle.wait_until_initialized(),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Sin peers en {METADATA_TIMEOUT_SECS}s (magnet muerto o sin seeders reales)."
        )
    })?
    .context("Error resolviendo metadata del torrent")?;

    let out = handle
        .with_metadata(|md| {
            md.file_infos
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let name = f.relative_filename.to_string_lossy().into_owned();
                    let parsed = crate::torrents::release_name::parse(&name);
                    let ext = std::path::Path::new(&name)
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_ascii_lowercase());
                    let is_video = f.len >= MIN_VIDEO_SIZE_BYTES
                        && ext
                            .as_deref()
                            .map(|e| VIDEO_EXTS.contains(&e))
                            .unwrap_or(false);
                    TorrentFileInfo {
                        file_id: i,
                        name,
                        size: f.len,
                        season: parsed.season,
                        episode: parsed.episode,
                        is_video,
                    }
                })
                .collect::<Vec<_>>()
        })
        .context("No se pudo leer metadata del torrent")?;

    // Dropear la sesión explícitamente antes de retornar — el
    // `_tempdir_guard` se dropea al retornar y no queremos que la
    // sesión aún esté abriendo ficheros dentro cuando se borre.
    cancel.cancel();
    drop(session);

    Ok(out)
}

/// Arranca una sesión BitTorrent para el magnet dado, sirve el fichero
/// principal (el más grande) por HTTP en `127.0.0.1:PORT` y devuelve la
/// URL para el reproductor.
///
/// Si el magnet expone infohash, los datos se guardan en la caché
/// persistente (`<cache>/videodrome/streams/<infohash>/`) — la próxima
/// vez que se abra esta misma peli, librqbit reutiliza los ficheros y
/// arranca casi al instante. Sin infohash, se cae a un tempdir efímero.
///
/// `target`: ver `select_file`. `None` = fichero de vídeo más grande.
pub async fn start(magnet: String) -> Result<StreamHandle> {
    start_with_target(magnet, None).await
}

/// Variante con selección explícita de fichero. Ver `start` y
/// `select_file`.
pub async fn start_with_target(
    magnet: String,
    target: Option<crate::torrents::FileSelector>,
) -> Result<StreamHandle> {
    let infohash = parse_infohash(&magnet);

    // Directorio de datos: caché persistente si hay infohash, tempdir si
    // no. `tempdir_guard` mantiene vivo el `TempDir` en el segundo caso;
    // cuando es `None`, el directorio persiste y solo lo limpia el
    // `prune` por TTL.
    let (data_dir, tempdir_guard) = match infohash.as_deref() {
        Some(hash) => {
            let dir = cache_dir()?.join(hash);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("No se pudo crear {}", dir.display()))?;
            // Tocamos el sentinel ya para que un prune concurrente no lo
            // borre justo antes de servir.
            let _ = touch_last_used(&dir);
            (dir, None)
        }
        None => {
            let td = tempfile::Builder::new()
                .prefix("videodrome-stream-")
                .tempdir()
                .context("No se pudo crear directorio temporal")?;
            (td.path().to_path_buf(), Some(td))
        }
    };

    // Un solo cancellation token para toda la sesión: se propaga al motor
    // librqbit (DHT, listeners TCP/UDP, tareas de fondo) y al servidor axum.
    // Sin esto, al hacer Drop del StreamHandle el DHT persistía en un
    // puerto UDP fijo y el siguiente `Session::new` fallaba con "address
    // already in use" hasta que el proceso se reiniciaba.
    let cancel = CancellationToken::new();

    // Persistencia por-torrent (audit §1): solo cuando tenemos
    // infohash y por tanto caché en disco. Sin esto, cada apertura
    // re-hashea el fichero entero antes de servir nada (~20 s por
    // 10.5 GiB, proporcional al tamaño). Con esto + `fastresume:
    // true`, librqbit reutiliza el `.bitv` de la sesión anterior y
    // salta el re-check. En magnets efímeros (sin infohash → tempdir)
    // no tiene sentido: al drop se borra todo igual.
    //
    // El folder vive DENTRO del dir del infohash → `clear_all` y
    // `prune` lo limpian con el resto de la entrada sin trabajo
    // extra ni riesgo de fastresume huérfano.
    let persistence = if infohash.is_some() {
        let folder = data_dir.join(LIBRQBIT_SESSION_SUBDIR);
        if let Err(e) = std::fs::create_dir_all(&folder) {
            tracing::warn!(
                target: "torrent",
                error = %e,
                dir = %folder.display(),
                "no se pudo crear el dir de persistencia; fallback a re-check completo"
            );
            None
        } else {
            Some(SessionPersistenceConfig::Json {
                folder: Some(folder),
            })
        }
    } else {
        None
    };
    let fastresume = persistence.is_some();

    let session = Session::new_with_opts(
        data_dir.clone(),
        SessionOptions {
            // No queremos que la sesión reutilice puertos DHT/estado entre
            // arranques — cada stream es efímero.
            disable_dht_persistence: true,
            persistence,
            fastresume,
            cancellation_token: Some(cancel.clone()),
            ..Default::default()
        },
    )
    .await
    .context("Error inicializando la sesión de librqbit")?;

    let response = session
        .add_torrent(
            AddTorrent::from_url(&magnet),
            Some(AddTorrentOptions {
                // Con caché persistente los ficheros ya existen; librqbit
                // los re-verifica pieza a pieza y solo baja lo que falta.
                overwrite: true,
                trackers: Some(EXTRA_TRACKERS.iter().map(|s| s.to_string()).collect()),
                ..Default::default()
            }),
        )
        .await
        .context("Error al añadir el torrent")?;

    let handle: Arc<ManagedTorrent> = match response {
        AddTorrentResponse::Added(_, h) => h,
        AddTorrentResponse::AlreadyManaged(_, h) => h,
        AddTorrentResponse::ListOnly(_) => anyhow::bail!("Torrent en modo list-only"),
    };

    // Timeout explícito: si el magnet no resuelve metadata en 45s
    // probablemente no hay peers vivos con el infohash. Mejor error claro
    // que "buscando…" para siempre.
    tokio::time::timeout(
        std::time::Duration::from_secs(METADATA_TIMEOUT_SECS),
        handle.wait_until_initialized(),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Sin peers en {METADATA_TIMEOUT_SECS}s (magnet muerto o sin seeders reales). \
             Prueba otro torrent con más seeders."
        )
    })?
    .context("Error resolviendo metadata del torrent")?;

    // Selección del fichero de vídeo a servir. Por defecto el más
    // grande (heurística estándar para películas mono-fichero). Si el
    // caller pidió un episodio concreto (season pack de serie), se
    // busca el fichero que matchee esa S+E parseando el nombre.
    let files: Vec<(usize, String, u64)> = handle
        .with_metadata(|md| {
            md.file_infos
                .iter()
                .enumerate()
                .map(|(i, f)| (i, f.relative_filename.to_string_lossy().into_owned(), f.len))
                .collect::<Vec<_>>()
        })
        .context("No se pudo leer metadata del torrent")?;

    let (file_id, file_name, file_len) =
        select_file(&files, target).context("Torrent sin ficheros")?;

    // Servidor HTTP local
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .context("No se pudo abrir puerto local")?;
    let addr = listener.local_addr()?;

    let state = AppState {
        handle: handle.clone(),
        file_id,
        file_len,
        active_request: Arc::new(tokio::sync::Mutex::new(None)),
        request_counter: Arc::new(AtomicU64::new(0)),
        max_seek: Arc::new(AtomicU64::new(0)),
        local_addr: addr,
        #[cfg(feature = "gui")]
        cached_probe: Arc::new(tokio::sync::Mutex::new(None)),
        #[cfg(feature = "gui")]
        hls: Arc::new(tokio::sync::Mutex::new(None)),
    };
    let max_seek = state.max_seek.clone();
    #[cfg(feature = "gui")]
    let app = Router::new()
        .route("/video", get(serve_video))
        .route("/probe.json", get(serve_probe))
        .route("/hls/playlist.m3u8", get(serve_hls_playlist))
        .route("/hls/{file}", get(serve_hls_segment))
        .route("/hls/audio", axum::routing::post(set_hls_audio))
        .route("/subs/embedded/{idx}", get(serve_embedded_subtitle))
        .layer(axum::middleware::from_fn(log_hls_requests))
        .layer(axum::middleware::from_fn(add_cors_headers))
        .with_state(state);
    #[cfg(not(feature = "gui"))]
    let app = Router::new()
        .route("/video", get(serve_video))
        .layer(axum::middleware::from_fn(add_cors_headers))
        .with_state(state);

    let cancel_task = cancel.clone();
    let server_task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { cancel_task.cancelled().await })
            .await;
    });

    // Telemetría periódica al log (audit): cada 5 s, mientras el stream
    // esté vivo, emitimos progreso + velocidad de librqbit + peers +
    // playhead. Firma esperada del bug (probe atascado con descarga
    // activa): `down_mbps > 0` sostenido mientras `req#N` no llega a
    // su `done`.
    //
    // NIVEL `debug`: 12 líneas/min ≈ 720 líneas/hora reventarían el
    // presupuesto de <200 líneas info de una reproducción típica. El
    // audit da explícitamente esta escape hatch ("si se supera,
    // degradar telemetría a `debug`"). Para reproducir el bug del
    // probe, el usuario ejecuta con `VIDEODROME_LOG_LEVEL=debug`.
    // La tarea se apaga cuando `cancel` se dispara al drop del
    // `StreamHandle`.
    let telemetry_handle = handle.clone();
    let telemetry_max_seek = max_seek.clone();
    let telemetry_cancel = cancel.clone();
    let telemetry_file_len = file_len;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Primer tick es inmediato; lo consumimos para que el primer
        // log llegue a los 5 s reales, no al startup (evita ruido en
        // el arranque donde librqbit aún no tiene stats).
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = telemetry_cancel.cancelled() => return,
                _ = ticker.tick() => {}
            }
            let stats = telemetry_handle.stats();
            let down_mbps = telemetry_handle
                .live()
                .map(|l| l.down_speed_estimator().mbps())
                .unwrap_or(0.0);
            let live_peers = stats
                .live
                .as_ref()
                .map(|l| l.snapshot.peer_stats.live as u32)
                .unwrap_or(0);
            let progress_pct = if stats.total_bytes > 0 {
                (stats.progress_bytes as f64 / stats.total_bytes as f64) * 100.0
            } else {
                0.0
            };
            let playhead = telemetry_max_seek.load(Ordering::Relaxed);
            let playhead_pct = if telemetry_file_len > 0 {
                (playhead as f64 / telemetry_file_len as f64) * 100.0
            } else {
                0.0
            };
            tracing::debug!(
                target: "torrent",
                down_mbps = format!("{down_mbps:.2}"),
                peers = live_peers,
                progress_mb = stats.progress_bytes / 1_048_576,
                total_mb = stats.total_bytes / 1_048_576,
                progress_pct = format!("{progress_pct:.1}"),
                playhead_mb = playhead / 1_048_576,
                playhead_pct = format!("{playhead_pct:.1}"),
                "telemetry"
            );
        }
    });

    let url = format!("http://{addr}/video");

    Ok(StreamHandle {
        url,
        file_name,
        file_len,
        file_id,
        infohash,
        handle,
        cancel,
        max_seek,
        data_dir,
        _session: session,
        _tempdir: tempdir_guard,
        _server_task: server_task,
    })
}

/// Handler HTTP. Soporta `Range: bytes=X-Y` (200/206). Sin Range devuelve
/// el fichero entero como 200 OK.
async fn serve_video(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_range);

    // Rango vacío: fichero de tamaño cero — nada que servir.
    if state.file_len == 0 {
        return Err((
            StatusCode::RANGE_NOT_SATISFIABLE,
            "Fichero vac\u{ed}o".to_string(),
        ));
    }

    let (start, end) = match range {
        Some((Some(s), Some(e))) => {
            // Rango con start y end explícitos. Rechaza `bytes=5-3`.
            if e < s {
                return Err((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    format!("Rango malformado: {s}-{e}"),
                ));
            }
            (s, e.min(state.file_len - 1))
        }
        Some((Some(s), None)) => (s, state.file_len - 1),
        Some((None, Some(suffix))) => {
            // Suffix range (`bytes=-500`): los últimos N bytes del fichero.
            // Algunos players lo usan para leer el índice al final del MP4.
            let n = suffix.min(state.file_len);
            (state.file_len - n, state.file_len - 1)
        }
        // `parse_range` rechaza el caso ambos-None (`bytes=-`) hoy, pero
        // no queremos panicar en producción si alguien relaja esa
        // validación sin actualizar este site. `debug_assert!` casca en
        // tests y builds de dev; en release caemos a servir el fichero
        // completo, que es la interpretación más conservadora del rango
        // "todo".
        Some((None, None)) => {
            debug_assert!(false, "parse_range should reject both-None ranges");
            (0, state.file_len - 1)
        }
        None => (0, state.file_len - 1),
    };

    if start >= state.file_len {
        return Err((
            StatusCode::RANGE_NOT_SATISFIABLE,
            format!("Range {start} >= {}", state.file_len),
        ));
    }

    // Trackear la posición de reproducción SOLO para Ranges con start
    // explícito. Los suffix ranges (`bytes=-N`) los usa VLC para leer el
    // índice al final del MP4 y no reflejan la playhead — si los
    // usáramos, `max_seek` saltaría al 99% al abrir cualquier peli.
    let is_explicit_start = matches!(range, Some((Some(_), _)));
    if is_explicit_start {
        state.max_seek.fetch_max(start, Ordering::Relaxed);
    }

    let content_length = end - start + 1;
    // Asigna un id monótono a esta request. Se usa como campo `req`
    // en TODOS los logs de `/video` para poder correlacionar (a) qué
    // request cancela a qué otra, y (b) cuántos bytes llegó a
    // entregar cada una antes de morir vs. cerrarse por EOF.
    let req_id = state.request_counter.fetch_add(1, Ordering::Relaxed);
    let range_desc = match range {
        Some((Some(s), Some(e))) => format!("{s}-{e}"),
        Some((Some(s), None)) => format!("{s}-"),
        Some((None, Some(n))) => format!("-{n}"),
        _ => "full".to_string(),
    };
    tracing::info!(
        target: "video",
        req = req_id,
        range = %range_desc,
        start,
        end,
        bytes = content_length,
        pct = format!("{:.1}", (start as f64 / state.file_len as f64) * 100.0),
        "range in"
    );

    // Cancela la petición HTTP anterior antes de arrancar la nueva. Así
    // el FileStream viejo se dropea y librqbit deja de repartir ancho de
    // banda con él — véase el comentario de `active_request` en `AppState`.
    //
    // Dos excepciones al cancel:
    //
    //   * `is_suffix_range` (`bytes=-N`): WKWebView los usa para leer
    //     el moov al final del MP4. No son la playhead y no se
    //     comparan con VLC/ffmpeg-HLS — no cancelamos por ellos ni les
    //     cancelamos a nadie.
    //
    //   * `burst_window`: en modo DIRECT, WKWebView emite un
    //     start-range para el moov y otro para los datos casi al
    //     mismo tiempo (dentro de ~30-80ms). Cancelar la request
    //     previa provocaría re-intentos y stalls. Si la request activa
    //     arrancó hace <BURST_WINDOW_MS, asumimos que es del mismo
    //     burst y coexistimos. Los seeks reales de VLC/ffmpeg vienen
    //     con segundos entre medias, muy por encima del umbral.
    const BURST_WINDOW_MS: u128 = 150;
    let is_suffix_range = matches!(range, Some((None, Some(_))));
    let request_token = CancellationToken::new();
    if !is_suffix_range {
        let mut guard = state.active_request.lock().await;
        let now = tokio::time::Instant::now();
        let decision: &'static str;
        let mut cancelled_prev: Option<u64> = None;
        let should_cancel_prev = guard
            .as_ref()
            .map(|(_, _, started)| started.elapsed().as_millis() >= BURST_WINDOW_MS)
            .unwrap_or(false);
        if should_cancel_prev {
            if let Some((prev_id, prev, _)) = guard.replace((req_id, request_token.clone(), now)) {
                prev.cancel();
                cancelled_prev = Some(prev_id);
                decision = "cancelled_prev";
            } else {
                decision = "slot_empty";
            }
        } else if guard.is_some() {
            // Coexistimos con el burst. Sobrescribimos el slot con el
            // nuestro para que la SIGUIENTE cancele a esta si llega
            // después del burst window.
            *guard = Some((req_id, request_token.clone(), now));
            decision = "coexist_burst";
        } else {
            *guard = Some((req_id, request_token.clone(), now));
            decision = "slot_empty";
        }
        tracing::info!(
            target: "video",
            req = req_id,
            decision,
            cancelled_prev,
            "active_request"
        );
    } else {
        tracing::info!(
            target: "video",
            req = req_id,
            decision = "suffix_skip",
            "active_request"
        );
    }

    // Crea un stream nuevo por request (librqbit gestiona la prioridad de
    // piezas por stream registrado).
    let mut file_stream = state
        .handle
        .clone()
        .stream(state.file_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if start > 0 {
        file_stream
            .seek(SeekFrom::Start(start))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    // Convierte AsyncRead en un Stream<Item=Bytes> con límite y con
    // corte al cancelar el token de esta request. `take_until` deja de
    // yield-ear en cuanto la petición siguiente sobrescriba el token.
    let limited = LimitedRead {
        inner: file_stream,
        remaining: content_length,
    };
    let raw = tokio_util::io::ReaderStream::with_capacity(limited, 64 * 1024);
    let cancel_fut = async move { request_token.cancelled().await };
    let cut = futures::stream::StreamExt::take_until(raw, Box::pin(cancel_fut));
    // Instrumentación: envolvemos el stream para contar bytes
    // entregados y loguear una línea al final que distingue
    // "fin natural (EOF)" de "cancelado por otra request". El log
    // es el emparejamiento del `range in` de arriba: sin él no se
    // puede reconstruir del debug.log si una request colgada llegó
    // a entregar algo o murió en seco.
    let stream = TracedResponseStream::new(cut, req_id, content_length);
    let body = Body::from_stream(stream);

    let status = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    let mut resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "video/mp4") // best-effort; VLC autodetecta
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, content_length.to_string());

    if range.is_some() {
        resp = resp.header(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{}", state.file_len))
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        );
    }

    resp.body(body)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Wrapper AsyncRead que limita el número de bytes a leer (para respetar
/// el `end` del Range).
struct LimitedRead<R> {
    inner: R,
    remaining: u64,
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for LimitedRead<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.remaining == 0 {
            return std::task::Poll::Ready(Ok(()));
        }
        let max = (self.remaining as usize).min(buf.remaining());
        let mut limited = buf.take(max);
        let before = limited.filled().len();
        let poll = std::pin::Pin::new(&mut self.inner).poll_read(cx, &mut limited);
        let read = limited.filled().len() - before;
        // SAFETY: bytes escritos en `limited` también están en `buf` porque
        // `buf.take()` comparte el buffer.
        unsafe {
            buf.assume_init(read);
        }
        buf.advance(read);
        self.remaining -= read as u64;
        poll
    }
}

/// Parsea `Range: bytes=START-END`, `bytes=START-` o `bytes=-SUFFIX`.
/// Devuelve `(Option<start>, Option<end>)`: si `start` es `None` se
/// trata como suffix range (los últimos N bytes). Solo se soporta UN
/// rango — los multipart se rechazan por caller.
fn parse_range(v: &str) -> Option<(Option<u64>, Option<u64>)> {
    let rest = v.strip_prefix("bytes=")?;
    let (start, end) = rest.split_once('-')?;
    let start = start.trim();
    let end = end.trim();
    let start_val: Option<u64> = if start.is_empty() {
        None
    } else {
        Some(start.parse().ok()?)
    };
    let end_val: Option<u64> = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    // Al menos uno de los dos debe estar presente.
    if start_val.is_none() && end_val.is_none() {
        return None;
    }
    Some((start_val, end_val))
}

/// Wrapper de stream de respuesta que cuenta bytes entregados y loguea
/// UNA línea al final: `done` (EOF natural, alcanzó `content_length`)
/// o `cancelled` (`take_until` cortó por token o el cliente cerró la
/// conexión).
///
/// Instrumentación del audit: sin esto no se puede saber, del
/// `debug.log`, si una request `/video` que quedó colgada llegó a
/// entregar algo antes de morir. Empareja con el `range in` que emite
/// `serve_video` al entrar.
struct TracedResponseStream<S> {
    inner: S,
    req_id: u64,
    delivered: u64,
    expected: u64,
    finished: bool,
}

impl<S> TracedResponseStream<S> {
    fn new(inner: S, req_id: u64, expected: u64) -> Self {
        Self {
            inner,
            req_id,
            delivered: 0,
            expected,
            finished: false,
        }
    }
}

impl<S, E> futures::stream::Stream for TracedResponseStream<S>
where
    S: futures::stream::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
{
    type Item = Result<bytes::Bytes, E>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let poll = std::pin::Pin::new(&mut self.inner).poll_next(cx);
        if let std::task::Poll::Ready(ref item) = poll {
            match item {
                Some(Ok(b)) => {
                    self.delivered += b.len() as u64;
                }
                Some(Err(_)) => {
                    // Error del stream (IO, etc.). Se loguea en Drop
                    // como cancelled — no distinguimos IO error de
                    // cancelación aquí, la firma en el log es la misma
                    // "no llegó a servir todo".
                }
                None => {
                    self.finished = true;
                    let complete = self.delivered >= self.expected;
                    tracing::info!(
                        target: "video",
                        req = self.req_id,
                        bytes = self.delivered,
                        expected = self.expected,
                        outcome = if complete { "eof" } else { "eof_short" },
                        "request done"
                    );
                }
            }
        }
        poll
    }
}

impl<S> Drop for TracedResponseStream<S> {
    fn drop(&mut self) {
        if !self.finished {
            // Se dropea sin haber emitido `Ready(None)`: el stream fue
            // cortado por `take_until` (cancelación de request) o el
            // cliente cerró la conexión antes del EOF. Esta es la firma
            // del bug del audit: request que se queda colgada sin haber
            // llegado al final.
            tracing::info!(
                target: "video",
                req = self.req_id,
                bytes = self.delivered,
                expected = self.expected,
                outcome = "cancelled",
                "request done"
            );
        }
    }
}

/// Middleware que añade cabeceras CORS permisivas a toda respuesta del
/// servidor local de streaming. Necesario porque el WebView de Tauri
/// vive en `http://127.0.0.1:1420` (dev) o `tauri://localhost` (prod),
/// mientras que este servidor bind-ea a un puerto aleatorio de
/// `127.0.0.1` → distinto origen a ojos del navegador. Sin CORS:
///
///   * `fetch()` a `/probe.json` desde React falla con "not allowed by
///     Access-Control-Allow-Origin" y devuelve `NotSupportedError`.
///   * `<video src="…/play.mp4">` cross-origin dispara un preflight
///     opaco y en algunas versiones de WKWebView aborta la carga
///     silenciosamente (MediaError code 4 sin mensaje).
///
/// El servidor solo escucha en localhost y su vida está atada al
/// StreamHandle, así que abrirlo con `*` no expone nada externo.
async fn add_cors_headers(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    // OPTIONS preflight: devolvemos 204 con los headers antes de que
    // el router intente rutar (algunas versiones de WKWebView los
    // mandan aunque nuestros GET son "simple requests").
    if req.method() == axum::http::Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("Access-Control-Allow-Origin", "*")
            .header("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS")
            .header("Access-Control-Allow-Headers", "Range, Content-Type")
            .header(
                "Access-Control-Expose-Headers",
                "Content-Length, Content-Range, Accept-Ranges",
            )
            .header("Access-Control-Max-Age", "86400")
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    headers.insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
    headers.insert(
        "Access-Control-Expose-Headers",
        HeaderValue::from_static("Content-Length, Content-Range, Accept-Ranges"),
    );
    resp
}

/// Middleware que emite un `info!` por cada petición a `/hls/*` con
/// método, ruta y status de la respuesta. Complementa a
/// `add_cors_headers`: se aplica ANTES (queda arriba en la pila de
/// layers) para que el `status` reflejado sea el emitido por el
/// handler (los handlers HLS pueden devolver 200 / 503 / 504 / 500
/// según deadline / stalled / fatal, y sin este log era imposible
/// correlacionar la request del WebView con el `warn!` interno).
#[cfg(feature = "gui")]
async fn log_hls_requests(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let path = req.uri().path().to_string();
    let is_hls = path.starts_with("/hls/");
    if !is_hls {
        return next.run(req).await;
    }
    let method = req.method().clone();
    let resp = next.run(req).await;
    tracing::info!(
        target: "hls-http",
        method = %method,
        path = %path,
        status = resp.status().as_u16(),
        "hls request"
    );
    resp
}

// ── HTML player: probe + HLS transmux ────────────────────────────────────
//
// Endpoints usados por la view `Player.tsx`:
//
//   GET /probe.json           → JSON con codec info (ffprobe cacheado)
//   GET /hls/playlist.m3u8    → playlist VOD estático (duración del
//                                probe → N segmentos enumerados)
//   GET /hls/seg-NNNNN.ts     → segmento transcodeado bajo demanda
//                                (ffmpeg arranca desde el idx pedido
//                                cuando el fichero no existe aún)
//
// El path fMP4 (`/play.mp4`) existió durante la fase inicial del player
// pero WKWebView rechaza fMP4 vía `<video src>` incluso con H.264 High
// estándar (solo lo acepta vía MSE con JS), así que se eliminó. Todo
// lo que no es `direct_playable` pasa por HLS.
//
// Todos leen la misma URL interna `http://127.0.0.1:PORT/video` que sirve
// el fichero raw del torrent con soporte Range — ffmpeg/ffprobe ya
// hablan HTTP nativamente. Con esto no duplicamos código de piece
// picking: librqbit sigue viendo un solo consumidor secuencial.

#[cfg(feature = "gui")]
async fn serve_probe(
    State(state): State<AppState>,
) -> Result<axum::Json<crate::ffmpeg::MediaInfo>, Response> {
    let mut info = match ensure_probe(&state).await {
        Ok(info) => info,
        Err(e) => {
            // Rama estructurada: timeout de ffprobe → 504 +
            // `{reason:"probe_stalled", bytes:0, elapsed_s:N}`.
            // El frontend distingue así "swarm sin seeders" (mensaje
            // "prueba otro release", botón Volver → lista de
            // torrents) de "ffmpeg roto" (mensaje "comprueba
            // ffmpeg"). Antes el timeout se hundía en un 500 con
            // mensaje libre y el frontend no podía diferenciar.
            if let Some(stalled) = e.downcast_ref::<crate::ffmpeg::ProbeStalled>() {
                tracing::warn!(
                    target: "probe",
                    reason = "probe_stalled",
                    elapsed_s = stalled.elapsed_s,
                    "returning 504"
                );
                let body = format!(
                    r#"{{"reason":"probe_stalled","bytes":0,"elapsed_s":{}}}"#,
                    stalled.elapsed_s
                );
                let resp = Response::builder()
                    .status(StatusCode::GATEWAY_TIMEOUT)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap_or_else(|_| Response::new(Body::empty()));
                return Err(resp);
            }
            // Fallo real de ffprobe/ffmpeg (binario ausente, JSON
            // corrupto, permission denied, exit != 0…): log con
            // causa a nivel `error!` y 500 genérico. El frontend
            // mantiene su mensaje "comprueba ffmpeg" en este caso.
            // `?e` usa el Debug de `anyhow::Error` que imprime la
            // cadena completa (`Caused by: …`), a diferencia de
            // `%e` que se queda con el mensaje más externo.
            tracing::error!(
                target: "probe",
                error = ?e,
                "probe failed"
            );
            let msg = e.to_string();
            let resp = Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Body::from(msg))
                .unwrap_or_else(|_| Response::new(Body::empty()));
            return Err(resp);
        }
    };
    // Audit §4: `direct_playable` se calcula por request con las
    // caps del cliente EN VIGOR (no las que había cuando se pobló
    // el `cached_probe`). Si el frontend registra caps DESPUÉS del
    // primer probe, el próximo `/probe.json` ya refleja el cambio.
    let caps = current_client_capabilities();
    info.direct_playable = crate::ffmpeg::compute_direct_playable(&info, &caps);
    Ok(axum::Json(info))
}

/// Devuelve el `MediaInfo` cacheado; si no está, lo genera con
/// `ffprobe` sobre el endpoint `/video` local. Idempotente y
/// thread-safe: si dos requests concurrentes piden probe la primera
/// coge el lock y las siguientes reusan el resultado.
#[cfg(feature = "gui")]
pub(in crate::stream) async fn ensure_probe(state: &AppState) -> Result<crate::ffmpeg::MediaInfo> {
    let mut guard = state.cached_probe.lock().await;
    if let Some(info) = guard.as_ref() {
        return Ok(info.clone());
    }
    let url = format!("http://{}/video", state.local_addr);
    let info = crate::ffmpeg::probe(&url).await?;
    *guard = Some(info.clone());
    Ok(info)
}

/// `POST /hls/audio?idx=<N>` — cambia la pista de audio activa del
/// stream HLS transmux. `N` es el índice del stream de audio en el
/// input tal cual lo reporta ffprobe (`MediaInfo.streams` filtrado
/// por `kind == "audio"`, orden original).
///
/// Semántica: mata el ffmpeg job actual (si lo hay), purga los
/// segmentos `.ts` producidos con la pista anterior, y guarda la
/// nueva selección en `HlsState.audio_idx`. La próxima petición de
/// segmento respawnea ffmpeg con `-map 0:v:0 -map 0:a:<idx>`.
///
/// El frontend debe:
///   1. Guardar `currentTime` antes del POST.
///   2. Esperar el 204.
///   3. `hls.destroy()` + `new Hls().loadSource(playlist)` de nuevo,
///      y hacer seek al `currentTime` guardado en `onCanPlay`.
///
/// Si se pide un idx igual al actual, es no-op (retorna 204 sin
/// tocar nada).
#[cfg(feature = "gui")]
#[derive(serde::Deserialize)]
struct AudioSwitchQuery {
    idx: usize,
}

#[cfg(feature = "gui")]
async fn set_hls_audio(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<AudioSwitchQuery>,
) -> Result<StatusCode, (StatusCode, String)> {
    // Asegura que el HlsState existe (aunque no haya empezado el
    // playback aún: el user puede abrir el panel de audio y cambiar
    // antes de darle a play).
    let _ = ensure_hls_dir(&state).await?;

    let (old_job, dir, changed) = {
        let mut guard = state.hls.lock().await;
        let hls = guard.as_mut().expect("hls state ensured");
        let changed = hls.audio_idx != Some(q.idx);
        if !changed {
            return Ok(StatusCode::NO_CONTENT);
        }
        hls.audio_idx = Some(q.idx);
        (hls.job.take(), hls.dir.clone(), changed)
    };

    if let Some(mut old) = old_job {
        // Igual que en `ensure_hls_job` — cancelar la Range GET del
        // ffmpeg viejo antes de matarlo, para que librqbit libere
        // el FileStream inmediatamente.
        {
            let mut req_guard = state.active_request.lock().await;
            if let Some((prev_id, token, _)) = req_guard.take() {
                token.cancel();
                tracing::info!(
                    target: "hls",
                    reason = "audio_switch",
                    cancelled_prev = prev_id,
                    "cancelling /video active_request before killing old ffmpeg"
                );
            }
        }
        let _ = old.child.kill().await;
        let _ = old.child.wait().await;
        tracing::info!(
            target: "hls",
            start_idx = old.start_idx,
            reason = "audio_switch",
            "killed old ffmpeg job"
        );
    }

    // Purgar los `.ts` producidos con la pista anterior. Si no lo
    // hacemos, hls.js pediría un segmento que existe en disco (con
    // audio viejo) → mezcla de audios entre segmentos consecutivos.
    if changed {
        if let Ok(iter) = std::fs::read_dir(&dir) {
            for entry in iter.flatten() {
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if s.starts_with("seg-") && (s.ends_with(".ts") || s.ends_with(".ts.tmp")) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /subs/embedded/<idx>` — extrae la pista de subtítulos
/// `<idx>` del contenedor y la devuelve como WebVTT text/plain UTF-8.
///
/// (Sin extensión `.vtt` en el path porque axum no permite mezclar
/// literal + capture en el mismo segmento; el `Content-Type: text/vtt`
/// del response identifica el formato.)
///
/// Solo funciona con subs "de texto" (SRT/ASS/SSA). Los subs de
/// imagen (PGS/DVBSUB/VobSub) NO se pueden convertir a VTT sin OCR;
/// ffmpeg falla y devolvemos 415 Unsupported Media Type para que el
/// frontend los oculte del panel de subs.
///
/// El `idx` es el índice del stream de subs en el input tal cual lo
/// reporta ffprobe (0..N-1 dentro del filter `-map 0:s:<idx>`).
///
/// Spawn one-shot (no persistente): abre input, extrae el stream,
/// pipea a stdout, muere. Coste ≈ 200-500ms para subs de peli
/// completa. El player cachea el VTT en un Blob del navegador, así
/// que solo se llama una vez por selección.
#[cfg(feature = "gui")]
async fn serve_embedded_subtitle(
    State(state): State<AppState>,
    axum::extract::Path(idx): axum::extract::Path<usize>,
) -> Result<Response, (StatusCode, String)> {
    let bin = crate::ffmpeg::ffmpeg_binary().ok_or((
        StatusCode::SERVICE_UNAVAILABLE,
        "ffmpeg no encontrado".to_string(),
    ))?;
    let input_url = format!("http://{}/video", state.local_addr);

    let output = {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-nostdin")
            .arg("-i")
            .arg(&input_url)
            // El input `/video` puede tardar en dar los primeros bytes
            // si el torrent está frío; `-analyzeduration` alto ayuda a
            // que ffmpeg no se rinda antes de encontrar la pista.
            .arg("-analyzeduration")
            .arg("60M")
            .arg("-probesize")
            .arg("50M")
            .arg("-map")
            .arg(format!("0:s:{idx}"))
            .arg("-c:s")
            .arg("webvtt")
            .arg("-f")
            .arg("webvtt")
            .arg("-")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        // Windows: sin `CREATE_NO_WINDOW` este spawn one-shot
        // parpadearía una consola cada vez que el user selecciona un
        // sub embebido. No-op fuera de Windows.
        cmd.hide_console();
        cmd.output().await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn ffmpeg: {e}"),
            )
        })?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Bitmap subs → ffmpeg da "Subtitle encoding currently only
        // possible from text to text or bitmap to bitmap". Distinguir
        // con un 415 al frontend para que oculte esta pista.
        let unsupported = stderr.contains("only possible")
            || stderr.contains("bitmap")
            || stderr.contains("Filter graph");
        let code = if unsupported {
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        // Antes tirábamos el stderr al vacío en la rama 415:
        // devolvíamos "no bitmap sub" sin evidencia real del motivo,
        // así que un fallo distinto (input inaccesible, filter roto)
        // se camuflaba de "unsupported" y no se diagnosticaba nunca.
        // Logueamos la cola completa a `warn!(target: "ffmpeg", ...)`.
        tracing::warn!(
            target: "ffmpeg",
            code = %output.status,
            idx,
            classified = if unsupported { "unsupported" } else { "internal" },
            stderr_tail = %stderr,
            "ffmpeg (subs embedded) exited"
        );
        return Err((code, format!("ffmpeg extraction failed: {stderr}")));
    }

    // Sanidad: el output debe empezar por `WEBVTT` (o \ufeff+WEBVTT)
    // para ser un track válido. Si no, ffmpeg devolvió algo raro
    // aunque saliese con status 0.
    let body = output.stdout;
    let head: String = body.iter().take(16).map(|&b| b as char).collect();
    if !head.contains("WEBVTT") {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "output no es WebVTT".to_string(),
        ));
    }

    let mut resp = Response::new(body.into());
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        "text/vtt; charset=utf-8".parse().unwrap(),
    );
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_start_and_end() {
        assert_eq!(parse_range("bytes=100-200"), Some((Some(100), Some(200))));
    }

    #[test]
    fn parse_range_start_open() {
        assert_eq!(parse_range("bytes=1000-"), Some((Some(1000), None)));
    }

    #[test]
    fn parse_range_suffix() {
        assert_eq!(parse_range("bytes=-500"), Some((None, Some(500))));
    }

    #[test]
    fn parse_range_rejects_both_empty() {
        // Necesario para que la rama `Some((None, None))` en
        // `serve_video` sea genuinamente inalcanzable — no relajar
        // sin actualizar el `unreachable!` de allí.
        assert_eq!(parse_range("bytes=-"), None);
    }

    #[test]
    fn parse_range_rejects_missing_prefix() {
        assert_eq!(parse_range("100-200"), None);
    }

    #[test]
    fn parse_range_rejects_non_numeric() {
        assert_eq!(parse_range("bytes=abc-xyz"), None);
    }

    // ── §4 audit series: select_file ─────────────────────────────

    fn mkfiles(items: &[(&str, u64)]) -> Vec<(usize, String, u64)> {
        items
            .iter()
            .enumerate()
            .map(|(i, (n, s))| (i, (*n).to_string(), *s))
            .collect()
    }

    #[test]
    fn select_file_default_picks_largest_video() {
        // Sin target: mayor vídeo. El README (2 MB, ni vídeo) se
        // ignora aunque sea único fichero .txt.
        let files = mkfiles(&[
            ("README.txt", 2 * 1024 * 1024),
            ("Movie.2019.1080p.mkv", 1500 * 1024 * 1024),
            ("sample.mkv", 30 * 1024 * 1024),
        ]);
        let (id, name, _) = select_file(&files, None).unwrap();
        assert_eq!(id, 1);
        assert!(name.contains("Movie.2019"));
    }

    #[test]
    fn select_file_target_matches_episode_in_pack() {
        use crate::torrents::FileSelector;
        let files = mkfiles(&[
            ("Fargo.S02E01.1080p.WEB-DL.x264-GRP.mkv", 900 * 1024 * 1024),
            ("Fargo.S02E02.1080p.WEB-DL.x264-GRP.mkv", 950 * 1024 * 1024),
            ("Fargo.S02E03.1080p.WEB-DL.x264-GRP.mkv", 800 * 1024 * 1024),
        ]);
        let (id, name, _) = select_file(&files, Some(FileSelector::Episode(2, 3))).unwrap();
        assert_eq!(id, 2);
        assert!(name.contains("S02E03"));
    }

    #[test]
    fn select_file_target_prefers_largest_of_dup_episodes() {
        use crate::torrents::FileSelector;
        // Pack con 720p y 1080p del mismo E03: gana el mayor.
        let files = mkfiles(&[
            ("Fargo.S02E03.720p.WEB-DL.x264.mkv", 400 * 1024 * 1024),
            ("Fargo.S02E03.1080p.WEB-DL.x264.mkv", 900 * 1024 * 1024),
        ]);
        let (id, _, _) = select_file(&files, Some(FileSelector::Episode(2, 3))).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn select_file_target_falls_back_to_largest_when_no_match() {
        use crate::torrents::FileSelector;
        // Pedimos S05E01 pero el pack solo tiene S02. En vez de
        // devolver None, cae al mayor — mejor un fichero incorrecto
        // que un error duro; el user puede corregir con list_files.
        let files = mkfiles(&[
            ("Fargo.S02E01.mkv", 900 * 1024 * 1024),
            ("Fargo.S02E02.mkv", 950 * 1024 * 1024),
        ]);
        let (id, _, _) = select_file(&files, Some(FileSelector::Episode(5, 1))).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn select_file_index_bypasses_heuristics() {
        // Con FileSelector::Index(i), el file elegido es el que dice
        // el provider — se salta hasta el filtro de samples porque
        // el provider (Torrentio) sabe mejor cuál es el bueno.
        use crate::torrents::FileSelector;
        let files = mkfiles(&[
            ("episode.mkv", 900 * 1024 * 1024),
            ("tiny.mkv", 10 * 1024 * 1024), // < 50 MB, normalmente sample
            ("huge.mkv", 3000 * 1024 * 1024),
        ]);
        let (id, _, _) = select_file(&files, Some(FileSelector::Index(1))).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn select_file_index_out_of_range_falls_back_to_largest() {
        use crate::torrents::FileSelector;
        let files = mkfiles(&[
            ("small.mkv", 100 * 1024 * 1024),
            ("big.mkv", 900 * 1024 * 1024),
        ]);
        let (id, _, _) = select_file(&files, Some(FileSelector::Index(99))).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn select_file_ignores_samples_under_50mb() {
        let files = mkfiles(&[
            ("Movie.sample.mkv", 40 * 1024 * 1024),
            ("Movie.1080p.mkv", 700 * 1024 * 1024),
        ]);
        let (id, _, _) = select_file(&files, None).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn select_file_falls_back_to_full_pool_when_all_filtered() {
        // Torrent con nombres no estándar (sin extensión conocida)
        // NO debe devolver None — se procesa el pool entero.
        let files = mkfiles(&[("videofile1", 1_000_000_000), ("videofile2", 500_000_000)]);
        let (id, _, _) = select_file(&files, None).unwrap();
        assert_eq!(id, 0);
    }

    #[test]
    fn select_file_empty_returns_none() {
        let files: Vec<(usize, String, u64)> = vec![];
        assert!(select_file(&files, None).is_none());
    }
}
