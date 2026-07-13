mod auth;
mod config;
mod keychain;
mod letterboxd;
mod progress;
mod recommend;
mod tmdb;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;

use config::Config;
use letterboxd::LetterboxdClient;
use progress::CliProgress;
use recommend::build_recommendations;
use tmdb::TmdbClient;

#[derive(Parser)]
#[command(
    name = "letterboxd-cli",
    about = "Recomendaciones de películas desde Letterboxd"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Genera recomendaciones de películas basadas en tu historial de Letterboxd
    Recommend {
        /// Número de recomendaciones a mostrar
        #[arg(short, long, default_value_t = 10)]
        count: usize,

        /// Rating mínimo propio para semillas (escala 0.5–5.0)
        #[arg(short = 'r', long, default_value_t = 4.0)]
        min_rating: f32,

        /// Imprime las recomendaciones como JSON en lugar de texto formateado
        #[arg(long)]
        json: bool,
    },

    /// Abre una interfaz interactiva (TUI) para explorar recomendaciones
    Tui {
        /// Número de recomendaciones a mostrar
        #[arg(short, long, default_value_t = 10)]
        count: usize,

        /// Rating mínimo propio para semillas (escala 0.5–5.0)
        #[arg(short = 'r', long, default_value_t = 4.0)]
        min_rating: f32,
    },

    /// Gestiona las credenciales guardadas en el Keychain de macOS
    Keychain {
        #[command(subcommand)]
        action: KeychainAction,
    },
}

#[derive(Subcommand)]
enum KeychainAction {
    /// Lee las credenciales actuales de .env y las guarda en el Keychain
    Import,
    /// Elimina las credenciales guardadas en el Keychain
    Clear,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Recommend {
            count,
            min_rating,
            json,
        } => {
            let config = Config::from_env()?;
            let http = reqwest::Client::builder()
                .user_agent("letterboxd-cli/0.1")
                .build()?;

            let token = auth::get_access_token(&http, &config).await?;

            let lb = LetterboxdClient::new(&http, &token);
            let tmdb = TmdbClient::new(&http, &config.tmdb_bearer_token);

            let recs =
                build_recommendations(&lb, &tmdb, count, min_rating, &CliProgress::new()).await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&recs)?);
                return Ok(());
            }

            println!(
                "\n  {}\n",
                format!("Recomendaciones para {}", config.username).bold()
            );

            for (i, rec) in recs.iter().enumerate() {
                let rating_str = match rec.lb_rating {
                    Some(r) => format!("{:.2}", r).yellow().to_string(),
                    None => format!("{:.2} (TMDB)", rec.movie.vote_average / 2.0)
                        .dimmed()
                        .to_string(),
                };
                println!(
                    "  {}  {:<42} ★ {}",
                    format!("{:>2}.", i + 1).dimmed(),
                    rec.movie.title.white().bold(),
                    rating_str,
                );
            }

            if recs.is_empty() {
                println!("  {}", "No se encontraron recomendaciones.".dimmed());
            }

            println!();
        }
        Commands::Tui { count, min_rating } => {
            let config = Config::from_env()?;
            let http = reqwest::Client::builder()
                .user_agent("letterboxd-cli/0.1")
                .build()?;
            tui::run(config, http, count, min_rating).await?;
        }
        Commands::Keychain { action } => match action {
            KeychainAction::Import => {
                config::load_env_files();

                let entries = [
                    ("LETTERBOXD_CLIENT_ID", keychain::CLIENT_ID),
                    ("LETTERBOXD_CLIENT_SECRET", keychain::CLIENT_SECRET),
                    ("LETTERBOXD_REFRESH_TOKEN", keychain::REFRESH_TOKEN),
                    ("LETTERBOXD_USERNAME", keychain::USERNAME),
                    ("TMDB_BEARER_TOKEN", keychain::TMDB_BEARER_TOKEN),
                ];

                let mut imported = 0usize;
                let mut skipped = Vec::new();
                for (env_key, kc) in entries {
                    match std::env::var(env_key) {
                        Ok(val) if !val.is_empty() => {
                            keychain::set(kc, &val)?;
                            imported += 1;
                            println!("  {} {} → {}", "✔".green(), env_key, kc);
                        }
                        _ => skipped.push(env_key),
                    }
                }

                if imported == 0 {
                    anyhow::bail!(
                        "No se encontró ninguna variable en el entorno ni en \
                         ~/.config/letterboxd-cli/.env. Crea un .env con al \
                         menos una de las variables antes de importar."
                    );
                }

                println!(
                    "\n{}",
                    format!("{imported} credencial(es) guardada(s) en el Keychain.").green()
                );
                if !skipped.is_empty() {
                    println!(
                        "  {} sin cambios (no estaban en .env): {}",
                        "•".dimmed(),
                        skipped.join(", ").dimmed()
                    );
                }
            }
            KeychainAction::Clear => {
                keychain::delete(keychain::CLIENT_ID)?;
                keychain::delete(keychain::CLIENT_SECRET)?;
                keychain::delete(keychain::REFRESH_TOKEN)?;
                keychain::delete(keychain::USERNAME)?;
                keychain::delete(keychain::TMDB_BEARER_TOKEN)?;

                println!(
                    "{}",
                    "Credenciales eliminadas del Keychain de macOS.".green()
                );
            }
        },
    }

    Ok(())
}
