//! Sub-módulo HLS: playlist + segmentos + pipeline ffmpeg. Extraído
//! de `stream.rs` en el refactor. En paso 4a se importan los
//! primeros submódulos (`argv`, `evict`, `grid`) y los helpers puros
//! (`parse_seg_idx`, `is_valid_hls_filename`, `max_produced_idx`)
//! que ya se necesitan cross-módulo. Los handlers HTTP + el
//! `spawn_hls` se moverán en paso 4b.

use std::path::Path;

pub(super) mod argv;
pub(super) mod evict;
pub(super) mod grid;

/// Parsea `seg-NNNNN.ts` → `NNNNN` como u64. `None` si el nombre no
/// respeta el formato exacto (validación fuerte, path traversal-safe).
pub(in crate::stream) fn parse_seg_idx(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("seg-")?;
    let idx = rest.strip_suffix(".ts")?;
    if idx.is_empty() || !idx.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    idx.parse().ok()
}

/// Whitelist para `/hls/{file}`. Solo acepta `seg-<digits>.ts` con
/// nombre de longitud sensata. Rechaza separadores (`/` y `\` — este
/// último es válido en Windows y `dir.join()` lo interpretaría como
/// sub-path), `..`, NUL y cualquier char no numérico. `playlist.m3u8`
/// no entra aquí: se sirve en una ruta separada registrada antes.
pub(in crate::stream) fn is_valid_hls_filename(name: &str) -> bool {
    parse_seg_idx(name).is_some() && name.len() <= 32
}

/// Escanea el tempdir compartido buscando el máximo idx de segmento
/// ya producido por el job activo (idx >= `floor`, que es
/// `job.start_idx`). Si aún no hay ninguno producido devuelve
/// `floor - 1` — de forma que el chequeo `idx > produced + LOOKAHEAD`
/// solo dispare restart cuando el idx pedido está muy por delante,
/// no por defecto.
///
/// Sync `std::fs::read_dir` a propósito: los tempdirs de HLS tienen
/// pocos miles de entradas y la operación es de <5ms típico; evita
/// la maquinaria async y el context switch. Solo se llama al decidir
/// si spawnear un job — no en el fast path (fichero existe).
pub(in crate::stream) fn max_produced_idx(dir: &Path, floor: u64) -> u64 {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return floor.saturating_sub(1),
    };
    let mut max: Option<u64> = None;
    for entry in entries.flatten() {
        let name_os = entry.file_name();
        let name = match name_os.to_str() {
            Some(s) => s,
            None => continue,
        };
        if let Some(idx) = parse_seg_idx(name) {
            if idx >= floor && max.map(|m| idx > m).unwrap_or(true) {
                max = Some(idx);
            }
        }
    }
    match max {
        Some(m) => m,
        None => floor.saturating_sub(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_hls_filename_rejects_playlist() {
        // El playlist va por su propia ruta (`/hls/playlist.m3u8` →
        // `serve_hls_playlist`). Este handler solo debe ver segments.
        assert!(!is_valid_hls_filename("playlist.m3u8"));
        // El `live.m3u8` que escribe ffmpeg tampoco se sirve nunca.
        assert!(!is_valid_hls_filename("live.m3u8"));
    }

    #[test]
    fn is_valid_hls_filename_accepts_segments() {
        assert!(is_valid_hls_filename("seg-00000.ts"));
        assert!(is_valid_hls_filename("seg-00042.ts"));
        assert!(is_valid_hls_filename("seg-99999.ts"));
        // Longitudes distintas al padding %05d también valen (parseamos
        // el idx como u64 sin exigir 5 dígitos).
        assert!(is_valid_hls_filename("seg-0.ts"));
        assert!(is_valid_hls_filename("seg-1234567.ts"));
    }

    #[test]
    fn is_valid_hls_filename_rejects_traversal() {
        assert!(!is_valid_hls_filename("../etc/passwd"));
        assert!(!is_valid_hls_filename("..\\etc\\passwd"));
        assert!(!is_valid_hls_filename("seg-00000.ts/../foo"));
        assert!(!is_valid_hls_filename("seg-00000.ts\\foo"));
    }

    #[test]
    fn is_valid_hls_filename_rejects_wrong_shape() {
        assert!(!is_valid_hls_filename(""));
        assert!(!is_valid_hls_filename("playlist.m3u"));
        assert!(!is_valid_hls_filename("seg-.ts"));
        // El formato antiguo `seg-<sid>-<idx>.ts` YA NO es válido —
        // el modelo VOD estático usa nombres estables sin sid.
        assert!(!is_valid_hls_filename("seg-1-0000.ts"));
        assert!(!is_valid_hls_filename("seg-a.ts"));
        assert!(!is_valid_hls_filename("seg-00000.tsx"));
    }

    #[test]
    fn parse_seg_idx_extracts_number() {
        assert_eq!(parse_seg_idx("seg-00000.ts"), Some(0));
        assert_eq!(parse_seg_idx("seg-00042.ts"), Some(42));
        assert_eq!(parse_seg_idx("seg-99999.ts"), Some(99999));
        assert_eq!(parse_seg_idx("seg-1234567.ts"), Some(1234567));
        assert_eq!(parse_seg_idx("seg-a.ts"), None);
        assert_eq!(parse_seg_idx("seg-.ts"), None);
        assert_eq!(parse_seg_idx("playlist.m3u8"), None);
    }

    #[test]
    fn max_produced_idx_ignores_below_floor_and_defaults_below_floor() {
        // Sin ningún fichero producido, el helper devuelve `floor - 1`
        // — de forma que el chequeo `idx > produced + LOOKAHEAD` solo
        // dispara restart cuando el idx pedido está muy por delante.
        let td = tempfile::tempdir().unwrap();
        assert_eq!(max_produced_idx(td.path(), 100), 99);

        // Con segmentos por debajo del floor, se ignoran (son residuos
        // de un job anterior sobre el mismo tempdir compartido).
        std::fs::write(td.path().join("seg-00050.ts"), b"").unwrap();
        std::fs::write(td.path().join("seg-00099.ts"), b"").unwrap();
        assert_eq!(max_produced_idx(td.path(), 100), 99);

        // Con segmentos >= floor, devuelve el máximo.
        std::fs::write(td.path().join("seg-00100.ts"), b"").unwrap();
        std::fs::write(td.path().join("seg-00105.ts"), b"").unwrap();
        std::fs::write(td.path().join("seg-00103.ts"), b"").unwrap();
        assert_eq!(max_produced_idx(td.path(), 100), 105);

        // Ficheros con extensión distinta (.tmp de temp_file, .m3u8)
        // NO cuentan: solo `seg-NNNN.ts` completos.
        std::fs::write(td.path().join("seg-00200.ts.tmp"), b"").unwrap();
        std::fs::write(td.path().join("live.m3u8"), b"").unwrap();
        assert_eq!(max_produced_idx(td.path(), 100), 105);
    }
}
