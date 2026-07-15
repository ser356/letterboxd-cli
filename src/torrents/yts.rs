//! Provider YTS (yts.mx). API JSON pública, sin auth. Solo cine.
//! Docs: <https://yts.mx/api>

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use super::{build_magnet, MovieQuery, Torrent, TorrentProvider};

const BASE: &str = "https://yts.mx/api/v2/list_movies.json";

pub struct Yts;

#[derive(Debug, Deserialize)]
struct YtsResponse {
    data: YtsData,
}

#[derive(Debug, Deserialize, Default)]
struct YtsData {
    #[serde(default)]
    movies: Option<Vec<YtsMovie>>,
}

#[derive(Debug, Deserialize)]
struct YtsMovie {
    title_long: String,
    #[serde(default)]
    year: Option<u16>,
    #[serde(default)]
    imdb_code: String,
    #[serde(default)]
    torrents: Vec<YtsTorrent>,
}

#[derive(Debug, Deserialize)]
struct YtsTorrent {
    hash: String,
    #[serde(default)]
    quality: String,
    #[serde(default)]
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    seeds: u32,
    #[serde(default)]
    peers: u32,
    #[serde(default)]
    size_bytes: u64,
}

#[async_trait]
impl TorrentProvider for Yts {
    fn name(&self) -> &'static str {
        "yts"
    }

    async fn search(&self, http: &reqwest::Client, q: &MovieQuery) -> Result<Vec<Torrent>> {
        // Preferimos IMDb ID (mucho más preciso). Si no, caemos al título.
        let query_term = q.imdb_id.clone().unwrap_or_else(|| q.title.clone());

        let url = format!(
            "{BASE}?query_term={}&limit=5&sort_by=seeds&order_by=desc",
            urlencoding::encode(&query_term)
        );

        let resp: YtsResponse = http
            .get(&url)
            .send()
            .await
            .context("Error de red hacia YTS")?
            .json()
            .await
            .context("Error al parsear respuesta de YTS")?;

        // YTS puede devolver varias películas con títulos parecidos
        // ("Alien", "Aliens", "Alien 3"…). Antes se pusheaban torrents
        // de todas ellas antes de decidir cuál era la buscada, así que
        // sin --year el resultado mezclaba pelis distintas. Ahora
        // seleccionamos una sola película y solo devolvemos sus
        // torrents:
        //   - Si viene `imdb_id`, la que coincida por IMDb.
        //   - Si no, la primera cuyo título normalizado coincida
        //     exactamente con el buscado (después del filtro de año).
        let target = norm_title(&q.title);
        let movies = resp.data.movies.unwrap_or_default();
        let picked = movies.into_iter().find(|m| {
            if let (Some(want), Some(got)) = (q.year, m.year) {
                if (want as i32 - got as i32).abs() > 1 {
                    return false;
                }
            }
            if let Some(imdb) = q.imdb_id.as_deref() {
                m.imdb_code == imdb
            } else {
                norm_title(&m.title_long) == target
            }
        });

        let Some(m) = picked else {
            return Ok(Vec::new());
        };

        let mut out = Vec::with_capacity(m.torrents.len());
        for t in m.torrents {
            let display = format!("{} [{}] {}", m.title_long, t.quality, t.kind);
            let magnet = build_magnet(&t.hash, &display);
            out.push(Torrent {
                title: display,
                magnet,
                size_bytes: t.size_bytes,
                seeders: t.seeds,
                leechers: t.peers,
                quality: Some(t.quality),
                source: "yts",
                infohash: t.hash.to_ascii_uppercase(),
            });
        }

        Ok(out)
    }
}

/// Normaliza un título para comparar YTS con la query del user:
/// lowercase, quita el año trailing (`(1979)`), y colapsa todo lo no
/// alfanumérico a espacios simples. `norm_title("Alien (1979)") ==
/// "alien"` y `norm_title("Aliens") == "aliens"`.
fn norm_title(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .filter(|w| !(w.len() == 4 && w.chars().all(|c| c.is_ascii_digit())))
        .collect::<Vec<_>>()
        .join(" ")
}
