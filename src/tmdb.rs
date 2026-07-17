use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const BASE_URL: &str = "https://api.themoviedb.org/3";
const RECS_CACHE_FILE: &str = "tmdb_recs_cache.json";
const RECS_CACHE_TTL_SECS: u64 = 24 * 3600;

// Caches adicionales anti-caída de TMDB. TTL más largo (7 días) porque
// los metadatos de una peli son ~inmutables: título, runtime, imdb_id,
// idioma original no cambian tras el estreno. Solo `overview` o
// `tagline` pueden ir mejorando con revisiones editoriales, pero eso
// es cosmético.
#[cfg(feature = "gui")]
const SEARCH_CACHE_FILE: &str = "tmdb_search_cache.json";
#[cfg(feature = "gui")]
const VIEW_CACHE_FILE: &str = "tmdb_view_cache.json";
#[cfg(feature = "gui")]
const DETAILS_CACHE_FILE: &str = "tmdb_details_cache.json";
#[cfg(feature = "gui")]
const LONG_CACHE_TTL_SECS: u64 = 7 * 24 * 3600;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TmdbMovie {
    /// ID interno de TMDB. `0` cuando el hit viene de un fallback que no
    /// pasa por TMDB (Cinemeta) y no hay TMDB id resoluble — en ese caso
    /// la GUI enruta la búsqueda de torrents por `imdb_id` / query directa.
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub vote_average: f32,
    #[allow(dead_code)]
    #[serde(default)]
    pub popularity: f32,
    #[serde(default)]
    pub release_date: Option<String>, // "YYYY-MM-DD"
    /// Ruta relativa del poster en TMDB (ej. `/abc123.jpg`), o URL absoluta
    /// cuando el hit viene de Cinemeta. La GUI (`tmdbPoster`) detecta si
    /// empieza por `http` y en ese caso lo usa tal cual.
    #[serde(default)]
    pub poster_path: Option<String>,
    /// IMDb ID (`tt…`) cuando lo conocemos. TMDB no lo devuelve en
    /// `/search/movie`; se rellena para hits de Cinemeta (que sí lo dan
    /// nativamente) y sirve para búsquedas de torrents por IMDb id
    /// cuando TMDB no está disponible.
    #[serde(default)]
    pub imdb_id: Option<String>,
}

impl TmdbMovie {
    /// Año extraído de `release_date`, si está presente y es parseable.
    pub fn year(&self) -> Option<u16> {
        self.release_date
            .as_deref()
            .and_then(|s| s.get(..4))
            .and_then(|s| s.parse().ok())
    }
}

#[derive(Debug, Deserialize)]
struct RecommendationsResponse {
    results: Vec<TmdbMovie>,
}

// ── Búsqueda por IMDb ID ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct FindResponse {
    #[serde(default)]
    movie_results: Vec<FindMovie>,
}

#[derive(Debug, Deserialize)]
struct FindMovie {
    #[allow(dead_code)]
    id: u64,
    title: String,
    #[serde(default)]
    release_date: String, // "YYYY-MM-DD"
}

/// Título y año resueltos desde un IMDb ID.
#[derive(Debug, Clone)]
pub struct ImdbLookup {
    pub title: String,
    pub year: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct CachedRecs {
    timestamp: u64,
    movies: Vec<TmdbMovie>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("El tiempo no puede ir hacia atrás")
        .as_secs()
}

fn cache_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("No se puede obtener el directorio de configuración")?
        .join("videodrome");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(RECS_CACHE_FILE))
}

fn load_cache() -> HashMap<u64, CachedRecs> {
    let Ok(path) = cache_path() else {
        return HashMap::new();
    };
    let Ok(data) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn save_cache(cache: &HashMap<u64, CachedRecs>) {
    if let Ok(path) = cache_path() {
        if let Ok(json) = serde_json::to_string(cache) {
            let _ = std::fs::write(path, json);
        }
    }
}

// ── Caches genéricos anti-caída ────────────────────────────────────────────
//
// Cada endpoint de TMDB que consumimos se cachea en disco con un TTL
// largo. Cuando TMDB tiene un incidente (como el del 2026-07 que nos
// motivó esta iteración), las queries que el user ya había hecho
// alguna vez siguen respondiendo desde caché — así el flujo de
// "Cartelera → clic → torrents" no se rompe entero.
//
// Write-through: cada `insert` guarda el HashMap completo en disco.
// Los JSONs son pequeños (<200KB tras uso normal), la escritura es
// insignificante comparada con la latencia del propio TMDB.

#[cfg(feature = "gui")]
fn generic_cache_path(filename: &str) -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("No se puede obtener el directorio de configuración")?
        .join("videodrome");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(filename))
}

#[cfg(feature = "gui")]
fn load_generic<K, V>(filename: &str) -> HashMap<K, V>
where
    K: std::hash::Hash + Eq + serde::de::DeserializeOwned,
    V: serde::de::DeserializeOwned,
{
    let Ok(path) = generic_cache_path(filename) else {
        return HashMap::new();
    };
    let Ok(data) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

#[cfg(feature = "gui")]
fn save_generic<K, V>(filename: &str, map: &HashMap<K, V>)
where
    K: std::hash::Hash + Eq + Serialize,
    V: Serialize,
{
    if let Ok(path) = generic_cache_path(filename) {
        if let Ok(json) = serde_json::to_string(map) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Envoltorio serializable con timestamp para poder aplicar TTL sin
/// depender de mtime del fichero (que se toca en cada save).
#[cfg(feature = "gui")]
#[derive(Debug, Serialize, Deserialize, Clone)]
struct Timestamped<T> {
    timestamp: u64,
    value: T,
}

/// Devuelve `Some(v)` si la entrada existe y no ha expirado.
#[cfg(feature = "gui")]
fn get_fresh<K, V>(map: &HashMap<K, Timestamped<V>>, key: &K, ttl_secs: u64) -> Option<V>
where
    K: std::hash::Hash + Eq,
    V: Clone,
{
    let entry = map.get(key)?;
    if now_unix().saturating_sub(entry.timestamp) < ttl_secs {
        Some(entry.value.clone())
    } else {
        None
    }
}

pub struct TmdbClient<'a> {
    http: &'a reqwest::Client,
    bearer_token: &'a str,
    cache: Mutex<HashMap<u64, CachedRecs>>,
    /// Cache de `/search/movie` — clave: query normalizada
    /// (`trim().to_lowercase()`), valor: lista de hits + timestamp.
    /// Solo se popula cuando TMDB responde OK; si TMDB peta y hay
    /// entrada fresca en caché, la usamos. Sin ella, cae a Cinemeta.
    #[cfg(feature = "gui")]
    search_cache: Mutex<HashMap<String, Timestamped<Vec<TmdbMovie>>>>,
    /// Cache de `/movie/{id}` (vista de detalle del modal).
    #[cfg(feature = "gui")]
    view_cache: Mutex<HashMap<u64, Timestamped<MovieView>>>,
    /// Cache de `/movie/{id}?append_to_response=external_ids,translations`
    /// (detalles usados por la búsqueda de torrents: imdb_id,
    /// original_title, russian_title, language, runtime).
    #[cfg(feature = "gui")]
    details_cache: Mutex<HashMap<u64, Timestamped<MovieDetails>>>,
}

impl<'a> TmdbClient<'a> {
    pub fn new(http: &'a reqwest::Client, bearer_token: &'a str) -> Self {
        Self {
            http,
            bearer_token,
            cache: Mutex::new(load_cache()),
            #[cfg(feature = "gui")]
            search_cache: Mutex::new(load_generic(SEARCH_CACHE_FILE)),
            #[cfg(feature = "gui")]
            view_cache: Mutex::new(load_generic(VIEW_CACHE_FILE)),
            #[cfg(feature = "gui")]
            details_cache: Mutex::new(load_generic(DETAILS_CACHE_FILE)),
        }
    }

    /// Recomendaciones de TMDB para una película, cacheadas en disco (TTL 24h)
    /// para no repetir la misma consulta en ejecuciones sucesivas.
    pub async fn get_recommendations(&self, tmdb_id: u64) -> Result<Vec<TmdbMovie>> {
        if let Some(cached) = self.cache.lock().unwrap().get(&tmdb_id) {
            if now_unix().saturating_sub(cached.timestamp) < RECS_CACHE_TTL_SECS {
                return Ok(cached.movies.clone());
            }
        }

        let url = format!("{BASE_URL}/movie/{tmdb_id}/recommendations?language=es-ES&page=1");

        let resp = self
            .http
            .get(&url)
            .bearer_auth(self.bearer_token)
            .send()
            .await
            .with_context(|| format!("Error al obtener recomendaciones para tmdb_id={tmdb_id}"))?;

        let status = resp.status();
        if !status.is_success() {
            // 404 (película no encontrada) es benigno — la ignoramos como
            // fuente. 401 / 429 / 5xx en cambio son señales de que la
            // config está rota o el rate-limit ha saltado: hay que
            // propagar para que el user lo vea, no devolver [] silencioso
            // que se lee como "no hay recomendaciones".
            if status == reqwest::StatusCode::NOT_FOUND {
                return Ok(vec![]);
            }
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "TMDB devolvi\u{f3} {status} para tmdb_id={tmdb_id}: {}",
                body.chars().take(200).collect::<String>()
            );
        }

        let body: RecommendationsResponse = resp
            .json()
            .await
            .context("Error al parsear respuesta de TMDB")?;

        self.cache.lock().unwrap().insert(
            tmdb_id,
            CachedRecs {
                timestamp: now_unix(),
                movies: body.results.clone(),
            },
        );

        Ok(body.results)
    }

    /// Persiste en disco la caché de recomendaciones acumulada en esta sesión.
    pub fn save_cache(&self) {
        save_cache(&self.cache.lock().unwrap());
    }

    /// Resuelve un IMDb ID a título + año usando el endpoint `/find`.
    /// Devuelve `None` si TMDB no conoce ese ID.
    pub async fn find_by_imdb(&self, imdb_id: &str) -> Result<Option<ImdbLookup>> {
        let clean = imdb_id.trim();
        let url = format!("{BASE_URL}/find/{clean}?external_source=imdb_id&language=en-US");
        let resp = self
            .http
            .get(&url)
            .bearer_auth(self.bearer_token)
            .send()
            .await
            .context("Error al llamar a TMDB /find")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let body: FindResponse = resp
            .json()
            .await
            .context("Error al parsear respuesta de TMDB /find")?;

        Ok(body.movie_results.into_iter().next().map(|m| ImdbLookup {
            year: m.release_date.get(..4).and_then(|s| s.parse::<u16>().ok()),
            title: m.title,
        }))
    }

    /// Busca películas por texto libre, resiliente a caídas de TMDB.
    ///
    /// Estrategia (equivalente a lo que hace Stremio al no depender solo
    /// de TMDB):
    ///
    /// 1. TMDB `/search/movie` → matches por título.
    /// 2. Si vienen pocos resultados y TMDB está vivo, se enriquece con
    ///    TMDB `/search/person` + `/person/{id}/movie_credits` filtrado
    ///    por `job = "Director"`. Esto permite que el user teclee un
    ///    nombre de director ("tarantino") y obtenga su filmografía.
    /// 3. Si TMDB entero está caído (paso 1 devuelve `Err`), se cae a
    ///    Cinemeta (`v3-cinemeta.strem.io`, el backend público de
    ///    Stremio) usando IMDb IDs para no romper la búsqueda.
    ///
    /// El orden de relevancia de TMDB se preserva; los hits de director
    /// se anexan al final; los de Cinemeta se usan solo si no hay
    /// respuesta de TMDB.
    #[cfg(feature = "gui")]
    pub async fn search_movies(&self, query: &str) -> Result<Vec<TmdbMovie>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(vec![]);
        }

        // Fast path: cache fresco en disco (TTL 7d). Evita ida a TMDB
        // en queries repetidas y sobrevive caídas de TMDB para
        // términos ya buscados. Si hay cache fresco, ya no probamos
        // director tampoco — el director se resolvió en la primera
        // llamada y quedó en el mismo cache.
        if let Some(cached) = self.cached_search(q) {
            return Ok(cached);
        }

        // Paso 1: título. Si TMDB responde error, saltamos a Cinemeta.
        let title_res = self.search_movies_by_title(q).await;
        let title_hits = match title_res {
            Ok(hits) => hits,
            Err(err) => {
                // TMDB inalcanzable (DNS, 5xx, rate-limit persistente).
                // Cinemeta funciona sin API key y suele estar arriba
                // aunque TMDB esté caído.
                if let Ok(cine) = search_cinemeta_movies(self.http, q).await {
                    if !cine.is_empty() {
                        return Ok(cine);
                    }
                }
                return Err(err);
            }
        };

        // Paso 2: si hay pocos matches por título, probamos director.
        // Umbral bajo para no gastar 2 llamadas extra en queries obvias.
        const DIRECTOR_THRESHOLD: usize = 3;
        if title_hits.len() >= DIRECTOR_THRESHOLD {
            return Ok(title_hits);
        }

        let dir_hits = self.search_movies_by_director(q).await.unwrap_or_default();

        // Dedup por TMDB id preservando el orden (título primero,
        // director después). Sin director hits, devolvemos title tal
        // cual — no queremos gastar Cinemeta cuando TMDB SÍ respondió.
        if dir_hits.is_empty() {
            return Ok(title_hits);
        }
        let mut seen: std::collections::HashSet<u64> = title_hits.iter().map(|m| m.id).collect();
        let mut merged = title_hits;
        for m in dir_hits {
            if m.id != 0 && seen.insert(m.id) {
                merged.push(m);
            }
        }

        // Sobrescribe el cache con la lista mergeada (título + director).
        // Así en la siguiente búsqueda no gastamos las 2 llamadas
        // extra del director.
        {
            let key = q.to_lowercase();
            let mut guard = self.search_cache.lock().unwrap();
            guard.insert(
                key,
                Timestamped {
                    timestamp: now_unix(),
                    value: merged.clone(),
                },
            );
            save_generic(SEARCH_CACHE_FILE, &guard);
        }
        Ok(merged)
    }

    /// TMDB `/search/movie` — búsqueda por título. Puerto directo del
    /// comportamiento anterior de `search_movies`, extraído para poder
    /// componer fallbacks encima. Cache disk 7d anti-caída: cuando TMDB
    /// responde OK guardamos; cuando peta y hay entrada fresca la
    /// servimos en su lugar (evita que Cinemeta se dispare por queries
    /// que ya conocíamos).
    ///
    /// `include_adult=true` porque videodrome es un cliente personal:
    /// películas censuradas, NC-17, o marcadas como adult en TMDB
    /// (Salò, Irreversible, Antichrist, mucho cine de autor europeo,
    /// documentales explícitos) las quiere ver el user, no hay que
    /// filtrarlas silenciosamente. Sin esto, esos títulos NO
    /// aparecen en /search/movie por defecto.
    #[cfg(feature = "gui")]
    async fn search_movies_by_title(&self, query: &str) -> Result<Vec<TmdbMovie>> {
        let key = query.trim().to_lowercase();
        let url = format!(
            "{BASE_URL}/search/movie?query={}&language=es-ES&include_adult=true&page=1",
            urlencoding::encode(query)
        );
        let resp_result = self
            .http
            .get(&url)
            .bearer_auth(self.bearer_token)
            .send()
            .await;

        let resp = match resp_result {
            Ok(r) => r,
            Err(e) => {
                // Fallo de red antes de tener respuesta. Intentamos
                // servir cache aunque haya expirado (TTL infinito en
                // modo desespero) — el user prefiere resultados viejos
                // a un error.
                if let Some(cached) = self.search_cache.lock().unwrap().get(&key).cloned() {
                    return Ok(cached.value);
                }
                return Err(anyhow::Error::new(e).context(format!(
                    "Error al llamar a TMDB /search/movie para '{query}'"
                )));
            }
        };
        if !resp.status().is_success() {
            // 4xx/5xx: mismo fallback, sirve cache aunque expire.
            if let Some(cached) = self.search_cache.lock().unwrap().get(&key).cloned() {
                return Ok(cached.value);
            }
            anyhow::bail!("TMDB /search/movie devolvi\u{f3} {}", resp.status());
        }
        let body: RecommendationsResponse = resp
            .json()
            .await
            .context("Error al parsear respuesta de TMDB /search/movie")?;

        // Solo guardamos hits no vacíos: si la query no matchea nada
        // en TMDB, guardarlo en caché nos impediría volver a probar la
        // siguiente vez (por si el user tecleó mal o TMDB indexa la
        // peli después).
        if !body.results.is_empty() {
            let mut guard = self.search_cache.lock().unwrap();
            guard.insert(
                key,
                Timestamped {
                    timestamp: now_unix(),
                    value: body.results.clone(),
                },
            );
            save_generic(SEARCH_CACHE_FILE, &guard);
        }
        Ok(body.results)
    }

    /// Sirve resultados de `/search/movie` desde cache disk si están
    /// frescos (dentro del TTL). Se llama ANTES del fetch en
    /// `search_movies` para saltarse la ida a TMDB entera en queries
    /// repetidas. Devuelve `None` si no hay cache o está expirado.
    #[cfg(feature = "gui")]
    fn cached_search(&self, query: &str) -> Option<Vec<TmdbMovie>> {
        let key = query.trim().to_lowercase();
        get_fresh(
            &self.search_cache.lock().unwrap(),
            &key,
            LONG_CACHE_TTL_SECS,
        )
    }

    /// Busca personas en TMDB y devuelve la filmografía como director
    /// del hit más relevante (si es de departamento "Directing"). Dos
    /// llamadas: `/search/person` + `/person/{id}/movie_credits`.
    ///
    /// Devuelve `Ok(vec![])` cuando no hay persona relevante — no es un
    /// error, simplemente el texto no era un nombre de director.
    #[cfg(feature = "gui")]
    async fn search_movies_by_director(&self, query: &str) -> Result<Vec<TmdbMovie>> {
        #[derive(Deserialize)]
        struct PersonSearchResp {
            #[serde(default)]
            results: Vec<PersonHit>,
        }
        #[derive(Deserialize)]
        struct PersonHit {
            id: u64,
            #[serde(default)]
            known_for_department: Option<String>,
            #[serde(default)]
            popularity: f32,
        }

        let url = format!(
            "{BASE_URL}/search/person?query={}&language=es-ES&include_adult=true&page=1",
            urlencoding::encode(query)
        );
        let resp = self
            .http
            .get(&url)
            .bearer_auth(self.bearer_token)
            .send()
            .await
            .with_context(|| format!("Error al llamar a TMDB /search/person para '{query}'"))?;
        if !resp.status().is_success() {
            return Ok(vec![]);
        }
        let body: PersonSearchResp = resp
            .json()
            .await
            .context("Error al parsear respuesta de TMDB /search/person")?;

        // Solo consideramos personas cuyo departamento es "Directing".
        // Si el más popular no lo es, no seguimos: probablemente el user
        // buscó un actor y ya salió en title search.
        let Some(person) = body
            .results
            .into_iter()
            .filter(|p| {
                p.known_for_department.as_deref() == Some("Directing") && p.popularity > 0.0
            })
            .max_by(|a, b| {
                a.popularity
                    .partial_cmp(&b.popularity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            return Ok(vec![]);
        };

        #[derive(Deserialize)]
        struct CreditsResp {
            #[serde(default)]
            crew: Vec<CrewCredit>,
        }
        #[derive(Deserialize)]
        struct CrewCredit {
            id: u64,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            release_date: Option<String>,
            #[serde(default)]
            poster_path: Option<String>,
            #[serde(default)]
            vote_average: f32,
            #[serde(default)]
            popularity: f32,
            #[serde(default)]
            job: String,
        }

        let credits_url = format!(
            "{BASE_URL}/person/{}/movie_credits?language=es-ES",
            person.id
        );
        let credits_resp = self
            .http
            .get(&credits_url)
            .bearer_auth(self.bearer_token)
            .send()
            .await
            .with_context(|| {
                format!("Error al llamar a TMDB /person/{}/movie_credits", person.id)
            })?;
        if !credits_resp.status().is_success() {
            return Ok(vec![]);
        }
        let credits: CreditsResp = credits_resp
            .json()
            .await
            .context("Error al parsear TMDB /person/movie_credits")?;

        // Ordenados por año descendente (más recientes primero) para que
        // el user vea la filmografía reciente sin scrollear.
        let mut movies: Vec<TmdbMovie> = credits
            .crew
            .into_iter()
            .filter(|c| c.job == "Director")
            .filter_map(|c| {
                let title = c.title?;
                Some(TmdbMovie {
                    id: c.id,
                    title,
                    vote_average: c.vote_average,
                    popularity: c.popularity,
                    release_date: c.release_date,
                    poster_path: c.poster_path,
                    imdb_id: None,
                })
            })
            .collect();
        movies.sort_by_key(|m| std::cmp::Reverse(m.year()));
        Ok(movies)
    }

    /// Consulta `/movie/{tmdb_id}?append_to_response=external_ids,translations`
    /// para obtener en una sola llamada:
    /// * `imdb_id` — imprescindible para providers Torznab que lo aceptan.
    /// * `original_title` — para buscar torrents en el idioma original (el
    ///   que suelen usar las releases scene/P2P internacionales).
    /// * `russian_title` — usado como fallback: si Knaben no da hits con el
    ///   título original, muchos torrents rusos (RuTracker, rutor...) están
    ///   indexados con el título en cirílico.
    /// * `original_language` — código ISO 639-1 (`"en"`, `"es"`, `"ru"`...).
    ///   Se usa para heurística de detección de audio original vs doblaje.
    /// * `release_date` — para extraer el año.
    pub async fn get_movie_details(&self, tmdb_id: u64) -> Result<Option<MovieDetails>> {
        // Fast path: cache disk fresco.
        #[cfg(feature = "gui")]
        {
            if let Some(cached) = get_fresh(
                &self.details_cache.lock().unwrap(),
                &tmdb_id,
                LONG_CACHE_TTL_SECS,
            ) {
                return Ok(Some(cached));
            }
        }

        match self.fetch_movie_details_uncached(tmdb_id).await {
            Ok(Some(details)) => {
                #[cfg(feature = "gui")]
                {
                    let mut guard = self.details_cache.lock().unwrap();
                    guard.insert(
                        tmdb_id,
                        Timestamped {
                            timestamp: now_unix(),
                            value: details.clone(),
                        },
                    );
                    save_generic(DETAILS_CACHE_FILE, &guard);
                }
                Ok(Some(details))
            }
            Ok(None) => Ok(None),
            Err(err) => {
                // TMDB caído: sirve cache expirado si lo hay. La info
                // de detalles no cambia entre incidentes, no
                // arriesgamos casi nada dando algo viejo.
                #[cfg(feature = "gui")]
                {
                    if let Some(stale) = self.details_cache.lock().unwrap().get(&tmdb_id).cloned() {
                        return Ok(Some(stale.value));
                    }
                }
                Err(err)
            }
        }
    }

    /// Versión "fina" sin cache — solo pega a TMDB. Extraída para que
    /// `get_movie_details` pueda cachear + implementar fallback stale.
    async fn fetch_movie_details_uncached(&self, tmdb_id: u64) -> Result<Option<MovieDetails>> {
        #[derive(Deserialize)]
        struct DetailsResponse {
            #[serde(default)]
            imdb_id: Option<String>,
            #[serde(default)]
            original_title: Option<String>,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            release_date: Option<String>,
            #[serde(default)]
            original_language: Option<String>,
            #[serde(default)]
            runtime: Option<u32>,
            #[serde(default)]
            external_ids: Option<ExternalIdsNested>,
            #[serde(default)]
            translations: Option<TranslationsNested>,
            #[serde(default)]
            alternative_titles: Option<AlternativeTitlesNested>,
        }
        #[derive(Deserialize)]
        struct ExternalIdsNested {
            #[serde(default)]
            imdb_id: Option<String>,
        }
        #[derive(Deserialize)]
        struct TranslationsNested {
            #[serde(default)]
            translations: Vec<Translation>,
        }
        #[derive(Deserialize)]
        struct Translation {
            #[serde(default)]
            iso_639_1: String,
            #[serde(default)]
            data: TranslationData,
        }
        #[derive(Deserialize, Default)]
        struct TranslationData {
            #[serde(default)]
            title: String,
        }
        // Titulos alternativos: TMDB devuelve un array con
        // `iso_3166_1` (país) + `title` (+ opcional `type`).
        // Filtramos por país en Fase 3a: EN (US/GB), ES y el país
        // que coincida con el idioma original de la peli.
        #[derive(Deserialize)]
        struct AlternativeTitlesNested {
            #[serde(default)]
            titles: Vec<AltTitle>,
        }
        #[derive(Deserialize)]
        struct AltTitle {
            #[serde(default)]
            iso_3166_1: String,
            #[serde(default)]
            title: String,
        }

        let url = format!(
            "{BASE_URL}/movie/{tmdb_id}?append_to_response=external_ids,translations,alternative_titles&language=en-US"
        );
        let resp = self
            .http
            .get(&url)
            .bearer_auth(self.bearer_token)
            .send()
            .await
            .with_context(|| format!("Error al llamar a TMDB /movie/{tmdb_id}"))?;
        if !resp.status().is_success() {
            // 404 real (peli no existe) → Ok(None), sin fallback.
            // 5xx / 429 → propagar como Err para activar stale cache.
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            anyhow::bail!("TMDB /movie/{tmdb_id} devolvi\u{f3} {}", resp.status());
        }
        let body: DetailsResponse = resp
            .json()
            .await
            .context("Error al parsear TMDB /movie details")?;

        let imdb_id = body
            .imdb_id
            .or_else(|| body.external_ids.and_then(|e| e.imdb_id))
            .filter(|s| !s.is_empty() && s.starts_with("tt"));
        let original_title = body.original_title.filter(|s| !s.is_empty());
        let year = body
            .release_date
            .as_deref()
            .and_then(|s| s.get(..4))
            .and_then(|s| s.parse::<u16>().ok());
        let russian_title = body.translations.and_then(|t| {
            t.translations
                .into_iter()
                .find(|tr| tr.iso_639_1 == "ru")
                .map(|tr| tr.data.title)
                .filter(|s| !s.is_empty())
        });

        // Fase 3a: títulos alternativos. Filtramos por los países que
        // realmente nos ayudan a encontrar torrents:
        //   * `US`, `GB` — títulos en inglés (los que usa scene por
        //     defecto).
        //   * `ES` — títulos españoles (mercado hispano).
        //   * País mapeado al `original_language` (`FR` para
        //     `original_language = "fr"`, etc.) — el título nativo
        //     suele aparecer en indexers regionales.
        // Deduplicamos por título normalizado (con `to_lowercase +
        // trim`) para no meter la misma variante dos veces.
        let mut wanted_countries: Vec<&str> = vec!["US", "GB", "ES"];
        if let Some(orig) = body.original_language.as_deref() {
            let mapped = match orig {
                "fr" => Some("FR"),
                "it" => Some("IT"),
                "de" => Some("DE"),
                "ja" => Some("JP"),
                "ko" => Some("KR"),
                "zh" => Some("CN"),
                "pt" => Some("BR"),
                _ => None,
            };
            if let Some(c) = mapped {
                if !wanted_countries.contains(&c) {
                    wanted_countries.push(c);
                }
            }
        }
        let mut alt_titles: Vec<String> = Vec::new();
        let mut seen_alt: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(alt) = body.alternative_titles {
            for t in alt.titles {
                if t.title.is_empty() {
                    continue;
                }
                if !wanted_countries.contains(&t.iso_3166_1.as_str()) {
                    continue;
                }
                let key = t.title.trim().to_lowercase();
                if seen_alt.insert(key) {
                    alt_titles.push(t.title);
                }
                // Cap a 6 para no soplar `title_variants` — la Fase 3b
                // limita a ≤3 variantes en `search_all` de todas
                // formas, pero mantener aquí un tope bajo mantiene la
                // caché ligera.
                if alt_titles.len() >= 6 {
                    break;
                }
            }
        }

        Ok(Some(MovieDetails {
            imdb_id,
            original_title,
            fallback_title: body.title,
            russian_title,
            original_language: body.original_language.filter(|s| !s.is_empty()),
            year,
            runtime: body.runtime.filter(|r| *r > 0),
            release_date: body.release_date.filter(|s| !s.is_empty()),
            alt_titles,
        }))
    }

    /// Vista de detalle para el modal estilo Stremio: sinopsis, backdrop,
    /// runtime, géneros, etc. Endpoint distinto de `get_movie_details`
    /// para no acoplar la búsqueda de torrents con la UI de detalle.
    ///
    /// Cache disk 7d anti-caída de TMDB. Cuando TMDB falla y no hay
    /// cache, intenta Cinemeta si tenemos `imdb_id` cacheado desde
    /// `get_movie_details` para este mismo `tmdb_id` — mapeamos la
    /// respuesta de Cinemeta a `MovieView` (menos rica: sin backdrop
    /// generalmente, poster en URL absoluta de metahub, textos en
    /// inglés) pero suficiente para que el modal se abra.
    #[cfg(feature = "gui")]
    pub async fn get_movie_view(&self, tmdb_id: u64) -> Result<Option<MovieView>> {
        // Fast path: cache fresco.
        if let Some(cached) = get_fresh(
            &self.view_cache.lock().unwrap(),
            &tmdb_id,
            LONG_CACHE_TTL_SECS,
        ) {
            return Ok(Some(cached));
        }

        match self.fetch_movie_view_uncached(tmdb_id).await {
            Ok(Some(view)) => {
                let mut guard = self.view_cache.lock().unwrap();
                guard.insert(
                    tmdb_id,
                    Timestamped {
                        timestamp: now_unix(),
                        value: view.clone(),
                    },
                );
                save_generic(VIEW_CACHE_FILE, &guard);
                Ok(Some(view))
            }
            Ok(None) => Ok(None),
            Err(err) => {
                // 1) Sirve stale cache si lo hay.
                if let Some(stale) = self.view_cache.lock().unwrap().get(&tmdb_id).cloned() {
                    return Ok(Some(stale.value));
                }
                // 2) Cinemeta fallback: solo posible si tenemos imdb_id
                //    cacheado desde una llamada previa a get_movie_details.
                let imdb = self
                    .details_cache
                    .lock()
                    .unwrap()
                    .get(&tmdb_id)
                    .and_then(|d| d.value.imdb_id.clone());
                if let Some(imdb_id) = imdb {
                    if let Ok(Some(view)) = fetch_cinemeta_view(self.http, tmdb_id, &imdb_id).await
                    {
                        return Ok(Some(view));
                    }
                }
                Err(err)
            }
        }
    }

    #[cfg(feature = "gui")]
    async fn fetch_movie_view_uncached(&self, tmdb_id: u64) -> Result<Option<MovieView>> {
        #[derive(Deserialize)]
        struct Resp {
            id: u64,
            title: String,
            #[serde(default)]
            original_title: Option<String>,
            #[serde(default)]
            overview: Option<String>,
            #[serde(default)]
            tagline: Option<String>,
            #[serde(default)]
            poster_path: Option<String>,
            #[serde(default)]
            backdrop_path: Option<String>,
            #[serde(default)]
            release_date: Option<String>,
            #[serde(default)]
            runtime: Option<u32>,
            #[serde(default)]
            vote_average: f32,
            #[serde(default)]
            genres: Vec<Genre>,
        }
        #[derive(Deserialize)]
        struct Genre {
            #[serde(default)]
            name: String,
        }

        let url = format!("{BASE_URL}/movie/{tmdb_id}?language=es-ES");
        // Timeout específico corto (4s). El HTTP client global tiene
        // 20s, demasiado para el modal — el user prefiere ver "sin
        // sinopsis" al instante que un spinner colgado. Si TMDB
        // tarda >4s, `get_movie_view` cae a Cinemeta o stale cache.
        let start = std::time::Instant::now();
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(4),
            self.http.get(&url).bearer_auth(self.bearer_token).send(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("TMDB /movie/{tmdb_id} (view) timeout tras 4s"))?
        .with_context(|| format!("Error al llamar a TMDB /movie/{tmdb_id} (view)"))?;
        eprintln!(
            "[tmdb] get_movie_view tmdb_id={tmdb_id} -> {} en {}ms",
            resp.status(),
            start.elapsed().as_millis()
        );
        if !resp.status().is_success() {
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            anyhow::bail!(
                "TMDB /movie/{tmdb_id} (view) devolvi\u{f3} {}",
                resp.status()
            );
        }
        let body: Resp = resp
            .json()
            .await
            .context("Error al parsear TMDB /movie (view)")?;

        Ok(Some(MovieView {
            id: body.id,
            title: body.title,
            original_title: body.original_title.filter(|s| !s.is_empty()),
            overview: body.overview.filter(|s| !s.is_empty()),
            tagline: body.tagline.filter(|s| !s.is_empty()),
            poster_path: body.poster_path,
            backdrop_path: body.backdrop_path,
            release_date: body.release_date.filter(|s| !s.is_empty()),
            runtime: body.runtime.filter(|r| *r > 0),
            vote_average: body.vote_average,
            genres: body
                .genres
                .into_iter()
                .map(|g| g.name)
                .filter(|s| !s.is_empty())
                .collect(),
        }))
    }
}

/// Vista de detalle de una película para el modal de la GUI. Se
/// deserializa también para poder round-tripear a través del cache
/// en disco (`tmdb_view_cache.json`).
#[cfg(feature = "gui")]
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MovieView {
    pub id: u64,
    pub title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub release_date: Option<String>,
    pub runtime: Option<u32>,
    pub vote_average: f32,
    pub genres: Vec<String>,
}

/// Detalles útiles de una película para búsquedas en providers de torrents.
/// Se serializa para poder cachear en disco (`tmdb_details_cache.json`)
/// — así, si TMDB se cae después de que el user haya abierto una peli
/// alguna vez, la búsqueda de torrents sigue funcionando desde caché.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovieDetails {
    pub imdb_id: Option<String>,
    /// Título en el idioma original (típicamente inglés) — el que aparece en
    /// releases de scene/P2P. Puede faltar en pelis muy oscuras.
    pub original_title: Option<String>,
    /// Título en el idioma de la petición (fallback si `original_title` es
    /// None).
    pub fallback_title: Option<String>,
    /// Título en ruso (cirílico), útil como fallback para torrents rusos.
    pub russian_title: Option<String>,
    /// Idioma original de la película (ISO 639-1: `"en"`, `"es"`, ...).
    pub original_language: Option<String>,
    pub year: Option<u16>,
    /// Runtime en minutos (para calcular resume-seconds desde una
    /// fracción de bytes). `None` cuando TMDB no lo expone o es 0.
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub runtime: Option<u32>,
    /// Fecha de estreno TMDB (`YYYY-MM-DD`). Se usa para el mensaje
    /// "todavía en cines" cuando la búsqueda de torrents da vacío
    /// (Fase 4b del audit — sirve para distinguir "no hay releases"
    /// de "aún no ha salido en digital"). `None` si TMDB no la expone.
    #[serde(default)]
    pub release_date: Option<String>,
    /// Títulos alternativos filtrados (endpoint
    /// `/movie/{id}/alternative_titles`). Se guardan los del país
    /// original + ES/US/GB, deduplicados normalizados. Alimenta el
    /// `title_variants` de `MovieQuery` en la búsqueda de torrents
    /// (Fase 3a/3b del audit — mejora recall en pelis no inglesas
    /// o con subtítulos largos).
    ///
    /// `#[serde(default)]` para compatibilidad con caches antiguos
    /// (`tmdb_details_cache.json` pre-Fase 3): al deserializar una
    /// entrada vieja quedará vacío y la próxima vez que se refresque
    /// se poblará.
    #[serde(default)]
    pub alt_titles: Vec<String>,
}

// ── Cinemeta (fallback anti-caída de TMDB) ─────────────────────────────────
//
// Cinemeta es el catálogo público de metadatos de Stremio
// (`v3-cinemeta.strem.io`). Sirve búsqueda por título indexada por IMDb ID,
// sin API key ni auth, y sigue funcionando cuando TMDB tiene un incidente.
// Solo se usa como fallback: si TMDB responde OK (aunque sea con 0 hits),
// nos quedamos con TMDB — Cinemeta no tiene búsqueda por director.

#[cfg(feature = "gui")]
const CINEMETA_BASE: &str = "https://v3-cinemeta.strem.io";

#[cfg(feature = "gui")]
#[derive(Debug, Deserialize)]
struct CinemetaResp {
    #[serde(default)]
    metas: Vec<CinemetaMeta>,
}

#[cfg(feature = "gui")]
#[derive(Debug, Deserialize)]
struct CinemetaMeta {
    /// IMDb ID (`tt…`).
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    poster: Option<String>,
    /// Cinemeta puede devolver el año como string ("1999") o como
    /// rango ("1999-2003") en series. Tratamos ambos como texto y
    /// extraemos los primeros 4 chars.
    #[serde(default)]
    year: Option<String>,
    /// Rating IMDb como string ("8.7"). En su ausencia queda 0.0.
    #[serde(default, rename = "imdbRating")]
    imdb_rating: Option<String>,
}

/// Busca películas en Cinemeta por texto libre. Devuelve `TmdbMovie`s con
/// `id = 0` (no hay TMDB id) y `imdb_id = Some("tt…")` para que la GUI
/// pueda encaminar la búsqueda de torrents por IMDb / query directa.
#[cfg(feature = "gui")]
pub async fn search_cinemeta_movies(http: &reqwest::Client, query: &str) -> Result<Vec<TmdbMovie>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(vec![]);
    }
    let url = format!(
        "{CINEMETA_BASE}/catalog/movie/top/search={}.json",
        urlencoding::encode(q)
    );
    let resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("Error al llamar a Cinemeta para '{q}'"))?;
    if !resp.status().is_success() {
        anyhow::bail!("Cinemeta devolvi\u{f3} {}", resp.status());
    }
    let body: CinemetaResp = resp
        .json()
        .await
        .context("Error al parsear respuesta de Cinemeta")?;

    Ok(body
        .metas
        .into_iter()
        .filter(|m| m.id.starts_with("tt") && !m.name.is_empty())
        .map(|m| {
            let year = m
                .year
                .as_deref()
                .and_then(|s| s.get(..4))
                .and_then(|s| s.parse::<u16>().ok());
            let vote = m
                .imdb_rating
                .as_deref()
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(0.0);
            TmdbMovie {
                id: 0,
                title: m.name,
                vote_average: vote,
                popularity: 0.0,
                release_date: year.map(|y| format!("{y}-01-01")),
                poster_path: m.poster,
                imdb_id: Some(m.id),
            }
        })
        .collect())
}

/// Cinemeta `/meta/movie/{imdbId}.json` → mapeado a `MovieView`. Es el
/// fallback del modal de detalle cuando TMDB está caído y no hay
/// cache. Perdemos calidad respecto a TMDB:
///
///   * Textos en inglés (Cinemeta no localiza).
///   * `poster_path` viene como URL absoluta de metahub — el frontend
///     ya detecta URLs con `http://` en `tmdbPoster()` y pasa a través.
///   * `backdrop_path` a veces ausente.
///   * `runtime` viene como string ("136 min"), lo parseamos.
///
/// El `tmdb_id` viene solo por preservar el identificador del caller
/// (Cinemeta no lo conoce).
#[cfg(feature = "gui")]
async fn fetch_cinemeta_view(
    http: &reqwest::Client,
    tmdb_id: u64,
    imdb_id: &str,
) -> Result<Option<MovieView>> {
    #[derive(Deserialize)]
    struct Wrap {
        #[serde(default)]
        meta: Option<Meta>,
    }
    #[derive(Deserialize)]
    struct Meta {
        #[serde(default)]
        name: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        tagline: Option<String>,
        #[serde(default)]
        poster: Option<String>,
        #[serde(default)]
        background: Option<String>,
        #[serde(default)]
        year: Option<String>,
        #[serde(default)]
        runtime: Option<String>,
        #[serde(default, rename = "imdbRating")]
        imdb_rating: Option<String>,
        #[serde(default)]
        genres: Vec<String>,
    }

    let url = format!("{CINEMETA_BASE}/meta/movie/{imdb_id}.json");
    let resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("Error al llamar a Cinemeta /meta para {imdb_id}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("Cinemeta /meta devolvi\u{f3} {}", resp.status());
    }
    let body: Wrap = resp
        .json()
        .await
        .context("Error al parsear Cinemeta /meta")?;
    let Some(meta) = body.meta else {
        return Ok(None);
    };
    if meta.name.is_empty() {
        return Ok(None);
    }

    let runtime = meta
        .runtime
        .as_deref()
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|r| *r > 0);
    let release_date = meta
        .year
        .as_deref()
        .and_then(|y| y.get(..4))
        .map(|y| format!("{y}-01-01"));
    let vote_average = meta
        .imdb_rating
        .as_deref()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.0);

    Ok(Some(MovieView {
        id: tmdb_id,
        title: meta.name,
        original_title: None,
        overview: meta.description.filter(|s| !s.is_empty()),
        tagline: meta.tagline.filter(|s| !s.is_empty()),
        poster_path: meta.poster,
        backdrop_path: meta.background,
        release_date,
        runtime,
        vote_average,
        genres: meta.genres.into_iter().filter(|s| !s.is_empty()).collect(),
    }))
}
