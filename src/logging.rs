//! Logging estructurado con `tracing` — stderr + fichero opcional.
//!
//! Motivación (audit "Logging a fichero e instrumentación"):
//!
//!   * En Windows la app corre con `windows_subsystem = "windows"` en
//!     modo GUI. Los `eprintln!` van a un handle de stderr inválido y
//!     se pierden silenciosamente — ni redirigiendo desde PowerShell
//!     se capturan. Antes de este cambio, en producción Windows no
//!     había NINGUNA visibilidad de bugs intermitentes.
//!
//!   * `tracing_appender::non_blocking` desacopla el write a fichero
//!     del await point de `axum` / `tokio` — un `info!` desde el
//!     handler `/video` no bloquea la respuesta HTTP ni el probe.
//!     El `WorkerGuard` que devuelve DEBE mantenerse vivo hasta el
//!     exit del proceso, o las últimas líneas se pierden en el drop.
//!
//! ## Activación
//!
//!   * Capa stderr: SIEMPRE activa salvo en modo TUI (donde
//!     corrompería la alternate screen igual que `eprintln!`).
//!   * Capa fichero: activa si `VIDEODROME_LOG` está definido (valor
//!     como ruta, o `1`/vacío para el default). En builds `debug`
//!     también se activa por defecto en el directorio de datos del
//!     usuario, para que un `cargo run` local ya deje log.
//!
//! ## Formato
//!
//!   * Timestamps con hora de pared local + milisegundos (`uptime`
//!     no sirve para correlacionar con reports del user).
//!   * Target del módulo (`video`, `hls`, `warmup`, `probe`, …).
//!   * Nivel controlado por `VIDEODROME_LOG_LEVEL` (`EnvFilter`
//!     format: `info,videodrome::stream=debug`). Default `info`.

use std::path::PathBuf;

pub use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::time::OffsetTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Destino de la capa `tracing`. Se elige en `main.rs` según el modo:
///
///   * `Enabled`: modo GUI / CLI puro / dispatch de subcomando.
///   * `Suppressed`: modo TUI (`ratatui` con alternate screen). Se
///     usa SOLO la capa fichero — si el user no ha pedido
///     `VIDEODROME_LOG`, no habrá ningún sitio donde vaya el log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StderrPolicy {
    Enabled,
    Suppressed,
}

/// Inicializa el subscriber global de `tracing`. Idempotente — si ya
/// se llamó, silenciosamente no hace nada.
///
/// Return: `(path, guard)`.
///
///   * `path` = `Some` si se activó la capa fichero (para que
///     `main.rs` pueda loguearla al arrancar).
///   * `guard` = `Some(WorkerGuard)` cuando hay capa fichero. El
///     caller (`main`) DEBE mantenerlo vivo hasta el final del
///     proceso. Al drop del guard se flushea el canal MPSC interno
///     del appender no-blocking; si se dropea antes de tiempo, o
///     nunca (p.ej. guardado en un `static` — los statics no
///     dropean al retornar de main), las últimas líneas se pierden
///     silenciosamente y el log queda amputado.
pub fn init(stderr_policy: StderrPolicy) -> anyhow::Result<(Option<PathBuf>, Option<WorkerGuard>)> {
    let filter =
        EnvFilter::try_from_env("VIDEODROME_LOG_LEVEL").unwrap_or_else(|_| EnvFilter::new("info"));

    // Timer con hora local. `OffsetTime::local_rfc_3339` puede fallar
    // si el proceso lanza threads antes de leer el offset (linux
    // multi-thread + soundness bug conocido de `chrono`). Inicializamos
    // ANTES de spawnear cualquier task, así que es seguro; si aun así
    // fallara, caemos a UTC.
    let timer = OffsetTime::local_rfc_3339().unwrap_or_else(|_| {
        OffsetTime::new(
            time::UtcOffset::UTC,
            time::format_description::well_known::Rfc3339,
        )
    });

    let stderr_enabled = matches!(stderr_policy, StderrPolicy::Enabled);
    let file_target = resolve_file_target();

    // Componer capas del subscriber con un `match` explícito sobre
    // (stderr, file). Las 4 ramas construyen el subscriber final in
    // situ para que Rust pueda inferir el parámetro `S` de cada
    // `fmt::Layer` en cadena — factorizarlo a funciones/closures fija
    // `S = Registry` y rompe la composición con `Layered<...>`.
    let (file_result, guard_result) = match (stderr_enabled, file_target) {
        (true, Some(path)) => {
            let (writer, path, guard) = open_file_writer(path);
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr)
                        .with_ansi(atty_stderr())
                        .with_target(true)
                        .with_timer(timer.clone()),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(writer)
                        .with_ansi(false)
                        .with_target(true)
                        .with_timer(timer),
                )
                .try_init()
                .ok();
            (Some(path), Some(guard))
        }
        (true, None) => {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::io::stderr)
                        .with_ansi(atty_stderr())
                        .with_target(true)
                        .with_timer(timer),
                )
                .try_init()
                .ok();
            (None, None)
        }
        (false, Some(path)) => {
            let (writer, path, guard) = open_file_writer(path);
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(writer)
                        .with_ansi(false)
                        .with_target(true)
                        .with_timer(timer),
                )
                .try_init()
                .ok();
            (Some(path), Some(guard))
        }
        (false, None) => {
            tracing_subscriber::registry().with(filter).try_init().ok();
            (None, None)
        }
    };

    Ok((file_result, guard_result))
}

/// Abre el fichero destino como `NonBlocking` writer y devuelve el
/// writer + el path efectivo (que puede diferir del input si el
/// parent dir no existía y se creó) + el `WorkerGuard` — el caller
/// lo propaga hasta `main` para mantenerlo vivo. Antes lo guardaba
/// en un `static OnceLock`, pero los statics NUNCA dropean al
/// retornar de `main`, así que el flush del buffer no-blocking no
/// se ejecutaba y las últimas líneas del log se perdían.
fn open_file_writer(
    path: PathBuf,
) -> (
    tracing_appender::non_blocking::NonBlocking,
    PathBuf,
    WorkerGuard,
) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("debug.log"));
    // `never`: sin rotación — un fichero único que crece. Con nivel
    // `info` estamos en cientos de KB por sesión típica.
    let appender = tracing_appender::rolling::never(dir, name);
    let (writer, guard) = tracing_appender::non_blocking(appender);
    (writer, path, guard)
}

/// Determina la ruta destino del log a fichero. Reglas:
///
///   1. `VIDEODROME_LOG=/ruta/absoluta.log` → usa esa ruta literal.
///   2. `VIDEODROME_LOG=1` (o vacío / cualquier otro valor no-ruta)
///      → `dirs::data_local_dir()/videodrome/debug.log` (en Windows
///      esto es `%LOCALAPPDATA%\videodrome\debug.log`).
///   3. Sin la env var pero build `debug`: mismo default que (2), para
///      que `cargo run` local ya deje traza sin exportar nada. En
///      builds `release` sin la var, NO se activa el fichero.
///   4. Si `dirs::data_local_dir()` no devuelve nada, silencio.
fn resolve_file_target() -> Option<PathBuf> {
    let env = std::env::var("VIDEODROME_LOG").ok();
    let want_file = env.is_some() || cfg!(debug_assertions);
    if !want_file {
        return None;
    }
    if let Some(val) = env.as_deref() {
        let looks_like_path = val.contains(std::path::MAIN_SEPARATOR)
            || val.contains('/')
            || val.contains('\\')
            || val.ends_with(".log");
        if looks_like_path && !val.is_empty() {
            return Some(PathBuf::from(val));
        }
    }
    let base = dirs::data_local_dir()?;
    Some(base.join("videodrome").join("debug.log"))
}

/// Detección barata de tty en stderr. Evita añadir `atty`/`is-terminal`
/// como dep — `std::io::IsTerminal` existe estable desde 1.70.
fn atty_stderr() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}
