//! Wrappers finos sobre `ffmpeg` y `ffprobe` para el player HTML nativo.
//!
//! El player embebido (view `Player.tsx` en el frontend) consume un
//! `<video>` que apunta a un endpoint HTTP local. Detrás de ese endpoint
//! spawneamos ffmpeg en modo transmux (`-c copy`) para repackagear el
//! contenedor del torrent (típicamente MKV) a fMP4 fragmentado — que sí
//! reproducen WKWebView / WebView2 / WebKitGTK sin plugins.
//!
//! El binario se busca primero en PATH y, si falla (típico en macOS al
//! abrir la app desde Launchpad/Finder — el PATH heredado no incluye
//! `/opt/homebrew/bin`), en las rutas fijas [`FALLBACK_DIRS`]. Al
//! arranque comprobamos con [`is_available`]; si falla, el frontend
//! cae al fallback VLC. La distribución (Homebrew cask, Scoop, Nix)
//! declara `ffmpeg` como dependencia para que el user no tenga que
//! instalarlo a mano.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Nombre del binario según el SO. Windows añade `.exe`.
#[cfg(target_os = "windows")]
const FFMPEG_BIN: &str = "ffmpeg.exe";
#[cfg(target_os = "windows")]
const FFPROBE_BIN: &str = "ffprobe.exe";
#[cfg(not(target_os = "windows"))]
const FFMPEG_BIN: &str = "ffmpeg";
#[cfg(not(target_os = "windows"))]
const FFPROBE_BIN: &str = "ffprobe";

/// Rutas fijas donde buscar los binarios cuando `which::which` falla.
///
/// Motivo: en macOS las apps GUI (Launchpad / Finder / `open`) heredan
/// un PATH stub (`/usr/bin:/bin:/usr/sbin:/sbin`) que NO incluye
/// `/opt/homebrew/bin` ni `/usr/local/bin`, así que aunque el user
/// tenga `brew install ffmpeg`, `which` desde dentro del bundle
/// devuelve `None` y caemos a VLC sin motivo. Miramos las rutas
/// canónicas de Homebrew (arm64 + Intel) y MacPorts como fallback.
/// En Linux/BSD el problema es raro pero cubrimos `/usr/local/bin` por
/// si acaso.
#[cfg(not(target_os = "windows"))]
const FALLBACK_DIRS: &[&str] = &[
    "/opt/homebrew/bin", // Homebrew arm64
    "/usr/local/bin",    // Homebrew Intel + fallback Linux
    "/opt/local/bin",    // MacPorts
    "/usr/bin",          // system (Linux distros)
];

/// Rutas fijas para Windows. Cubre las tres formas típicas de
/// instalar ffmpeg cuando el user NO lo tiene en PATH:
///   * winget (`winget install Gyan.FFmpeg`) — crea shims en
///     `%LOCALAPPDATA%\Microsoft\WinGet\Links`.
///   * scoop (`scoop install ffmpeg`) — shims en `~\scoop\shims`.
///   * Instalación manual desde gyan.dev / BtbN — el usuario
///     descomprime el zip en `C:\ffmpeg\bin` (convención más
///     común aunque no oficial).
///
/// La lista se computa en runtime porque las rutas dependen de
/// variables de entorno (`LOCALAPPDATA`, `USERPROFILE`) que no se
/// pueden expresar como `&'static str`.
#[cfg(target_os = "windows")]
fn windows_fallback_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut v: Vec<PathBuf> = Vec::new();
    // Convención zip manual (más común).
    v.push(PathBuf::from(r"C:\ffmpeg\bin"));
    // winget shims (Gyan.FFmpeg / BtbN.FFmpeg registran binarios
    // aquí — un solo directorio con los .exe symlinkeados).
    if let Ok(lad) = std::env::var("LOCALAPPDATA") {
        v.push(PathBuf::from(lad).join(r"Microsoft\WinGet\Links"));
    }
    // scoop shims (`~\scoop\shims\ffmpeg.exe`).
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(r"scoop\shims"));
    }
    // Chocolatey.
    if let Ok(cd) = std::env::var("ChocolateyInstall") {
        v.push(PathBuf::from(cd).join("bin"));
    } else {
        v.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
    }
    v
}

/// Busca `name` primero por PATH y, si falla, en los `FALLBACK_DIRS`
/// de la plataforma. Solo devuelve `Some` si la ruta existe como
/// fichero.
fn locate_bin(name: &str) -> Option<PathBuf> {
    if let Ok(p) = which::which(name) {
        return Some(p);
    }
    #[cfg(not(target_os = "windows"))]
    {
        for dir in FALLBACK_DIRS {
            let candidate = PathBuf::from(dir).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        for dir in windows_fallback_dirs() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Ruta al binario `ffmpeg`. `None` si no está instalado. Se usa
/// `which::which` en vez de intentar spawnear para poder distinguir
/// "no está" (mostrar diálogo con instrucciones) de "está pero peta".
pub fn ffmpeg_binary() -> Option<PathBuf> {
    locate_bin(FFMPEG_BIN)
}

/// Ruta al binario `ffprobe`. Ambos se instalan juntos en todas
/// las distros que conocemos, pero comprobamos por separado por si
/// alguien tiene un ffmpeg mínimo sin ffprobe.
pub fn ffprobe_binary() -> Option<PathBuf> {
    locate_bin(FFPROBE_BIN)
}

/// `true` sii ambos binarios están disponibles. Es el gate para
/// activar el player HTML — si falla, el frontend usa VLC.
pub fn is_available() -> bool {
    ffmpeg_binary().is_some() && ffprobe_binary().is_some()
}

// ── ffprobe ────────────────────────────────────────────────────────────────

/// Info que ffprobe devuelve sobre un stream. Solo mapeamos los campos
/// que necesita el player para decidir transmux vs transcode y para
/// mostrar la lista de audio/subs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration_seconds: Option<f64>,
    pub streams: Vec<StreamInfo>,
    /// Formato de contenedor del input tal cual lo reporta ffprobe
    /// (`matroska,webm` para .mkv, `mov,mp4,m4a,3gp,3g2,mj2` para .mp4).
    /// Se usa solo para logging — la decisión transmux/transcode se
    /// toma por códec de cada stream, no por contenedor.
    pub container: Option<String>,
    /// `true` si el frontend puede alimentar `<video src>` con `/video`
    /// directo, sin pasar por ffmpeg. Es cierto sólo cuando el source
    /// es MP4/MOV con H.264/HEVC + AAC/MP3 — el WebView los reproduce
    /// nativamente con seek por HTTP Range. Se calcula al final de
    /// `probe()` para tenerlo listo cuando el frontend decide el src.
    #[serde(default)]
    pub direct_playable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: u32,
    pub kind: StreamKind,
    /// Nombre del códec según ffmpeg (`h264`, `hevc`, `aac`, `subrip`…).
    pub codec: String,
    /// Idioma ISO 639-2 si el track lo declara (`eng`, `spa`, `jpn`).
    /// Los MKV suelen tenerlo; los MP4 casi nunca.
    pub language: Option<String>,
    /// `title` tag del stream (raro pero útil, ej. "Director's commentary").
    pub title: Option<String>,
    /// Solo para video: `width`.
    pub width: Option<u32>,
    /// Solo para video: `height`.
    pub height: Option<u32>,
    /// Solo para video: pixel format (`yuv420p`, `yuv420p10le`, ...).
    /// Se usa para detectar 10-bit — WKWebView y WebView2 solo
    /// decodifican yuv420p 8-bit vía `<video>`, así que 10-bit
    /// requiere transcode.
    #[serde(default)]
    pub pix_fmt: Option<String>,
    /// Solo para video: profile (`Main`, `Main 10`, `High`, ...).
    /// Redundante con `pix_fmt` para HEVC pero ffprobe a veces
    /// solo lo reporta por aquí.
    #[serde(default)]
    pub profile: Option<String>,
    /// Solo para audio: número de canales (1=mono, 2=stereo, 6=5.1,
    /// 8=7.1). Se usa en `spawn_hls` para elegir bitrate AAC sin
    /// forzar downmix a estéreo (preservación de multicanal).
    #[serde(default)]
    pub channels: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

/// Corre `ffprobe -v error -print_format json -show_streams -show_format
/// <url>` y parsea el JSON. `url` es normalmente el endpoint HTTP local
/// del stream de librqbit — ffprobe lo consume vía Range requests y solo
/// lee los primeros MB (cabecera + índice), así que no bloquea la
/// descarga completa.
pub async fn probe(url: &str) -> Result<MediaInfo> {
    let bin = ffprobe_binary().context("ffprobe no est\u{e1} en PATH")?;
    let out = Command::new(bin)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
            url,
        ])
        .output()
        .await
        .context("Error al spawnear ffprobe")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "ffprobe devolvi\u{f3} {} \u{2014} {}",
            out.status,
            stderr.trim()
        );
    }

    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        streams: Vec<RawStream>,
        #[serde(default)]
        format: Option<RawFormat>,
    }
    #[derive(Deserialize)]
    struct RawFormat {
        #[serde(default)]
        duration: Option<String>,
        #[serde(default)]
        format_name: Option<String>,
    }
    #[derive(Deserialize)]
    struct RawStream {
        index: u32,
        codec_type: String,
        #[serde(default)]
        codec_name: Option<String>,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        #[serde(default)]
        pix_fmt: Option<String>,
        #[serde(default)]
        profile: Option<String>,
        #[serde(default)]
        channels: Option<u32>,
        #[serde(default)]
        tags: Option<Tags>,
    }
    #[derive(Deserialize)]
    struct Tags {
        #[serde(default)]
        language: Option<String>,
        #[serde(default)]
        title: Option<String>,
    }

    let raw: Raw = serde_json::from_slice(&out.stdout).context("ffprobe JSON inv\u{e1}lido")?;
    let duration_seconds = raw
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .and_then(|s| s.parse::<f64>().ok());
    let container = raw.format.and_then(|f| f.format_name);
    let streams = raw
        .streams
        .into_iter()
        .map(|s| {
            let kind = match s.codec_type.as_str() {
                "video" => StreamKind::Video,
                "audio" => StreamKind::Audio,
                "subtitle" => StreamKind::Subtitle,
                _ => StreamKind::Other,
            };
            let (language, title) = match s.tags {
                Some(t) => (t.language, t.title),
                None => (None, None),
            };
            StreamInfo {
                index: s.index,
                kind,
                codec: s.codec_name.unwrap_or_default(),
                language,
                title,
                width: s.width,
                height: s.height,
                pix_fmt: s.pix_fmt,
                profile: s.profile,
                channels: s.channels,
            }
        })
        .collect();

    let mut info = MediaInfo {
        duration_seconds,
        streams,
        container,
        direct_playable: false,
    };
    // Rellenamos `direct_playable` derivándolo del propio MediaInfo.
    // Se recalcula aquí (en vez de exigir al caller que lo haga) para
    // que el JSON que sale por `/probe.json` ya venga con la respuesta.
    info.direct_playable = compute_direct_playable(&info);
    Ok(info)
}

// ── Compatibilidad con el player HTML ──────────────────────────────────────

/// Códecs de video que WKWebView/WebView2/WebKitGTK reproducen nativamente
/// dentro de MP4/MOV al apuntar `<video src>` a `/video` raw. Fuera de
/// esta lista → hay que pasar por el path HLS (transmux con ffmpeg).
const DIRECT_VIDEO_CODECS: &[&str] = &["h264", "hevc"];

/// Códecs de audio compatibles con el path DIRECT. El resto
/// (opus, flac, vorbis, ac3, eac3, dts, truehd…) obliga a pasar
/// por transmux.
const DIRECT_AUDIO_CODECS: &[&str] = &["aac", "mp3"];

/// `true` si el source es MP4/MOV con códecs ya WebView-compatibles
/// y sin banderas raras (10-bit, 4:2:2/4:4:4, perfiles High 10…). En
/// ese caso el player HTML apunta `<video src>` a `/video` directo —
/// sin subprocess, sin remux, con Range HTTP para seek nativo.
///
/// Todo lo que no cumpla esta whitelist entra por el path HLS
/// (`spawn_hls` transcodifica a H.264 8-bit High + AAC).
fn compute_direct_playable(info: &MediaInfo) -> bool {
    let Some(video) = info.streams.iter().find(|s| s.kind == StreamKind::Video) else {
        return false;
    };
    if !DIRECT_VIDEO_CODECS.contains(&video.codec.as_str()) {
        return false;
    }
    let audio_ok = info
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Audio)
        .map(|s| DIRECT_AUDIO_CODECS.contains(&s.codec.as_str()))
        .unwrap_or(false);
    if !audio_ok {
        return false;
    }
    // Escape hatch: aunque el códec figure en la whitelist,
    // WKWebView/WebView2 solo decodifican vía `<video>` con
    // chroma yuv420p 8-bit. HEVC "Main 10" (10-bit HDR/BluRay UHD),
    // H.264 High 10 y 4:2:2 leen OK del fichero pero fallan al
    // decodificar → los tratamos como no-direct y los mandamos a
    // HLS con transcode.
    let pix_bad = video
        .pix_fmt
        .as_deref()
        .map(|p| {
            let p = p.to_ascii_lowercase();
            p.contains("10le")
                || p.contains("10be")
                || p.contains("12le")
                || p.contains("12be")
                // yuv422p, yuv444p, yuvj422p, etc.
                || p.contains("422p")
                || p.contains("444p")
        })
        .unwrap_or(false);
    let profile_bad = video
        .profile
        .as_deref()
        .map(|p| {
            let p = p.to_ascii_lowercase();
            p.contains("main 10") || p.contains("high 10") || p.contains("high 4:")
        })
        .unwrap_or(false);
    if pix_bad || profile_bad {
        return false;
    }
    // El contenedor de origen tiene que ser MP4/MOV (o similares).
    // MKV/AVI aunque lleven H.264 no van por `<video src>` directo
    // — WKWebView solo remuxa MP4 nativamente.
    info.container
        .as_deref()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            c.split(',')
                .any(|part| matches!(part.trim(), "mp4" | "mov" | "m4a" | "3gp" | "3g2" | "mj2"))
        })
        .unwrap_or(false)
}
