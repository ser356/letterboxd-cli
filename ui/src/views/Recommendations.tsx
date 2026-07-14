import { useEffect, useState } from 'react'
import { TopNav } from '../components/TopNav'
import {
  getRecommendations,
  isTauri,
  type Recommendation,
} from '../lib/api'

/**
 * Recommendations. Photo-first card grid (Airbnb pattern), 4-up at
 * desktop, dropping columns responsively. Empty and loading states are
 * skeletons matching the final card shape.
 */
export function Recommendations() {
  const [items, setItems] = useState<Recommendation[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!isTauri()) {
      setError('Esta vista requiere la app de escritorio (Tauri).')
      return
    }
    getRecommendations(20, 4.0)
      .then(setItems)
      .catch((e) => setError(String(e)))
  }, [])

  return (
    <div className="min-h-[100dvh] bg-canvas">
      <TopNav>
        <a href="/" className="hover:text-ink transition-colors">
          Inicio
        </a>
      </TopNav>

      <main className="mx-auto max-w-[1280px] px-8 pt-12 pb-24">
        <div className="mb-10 flex items-end justify-between">
          <h1 className="text-[28px] font-semibold leading-tight text-ink">
            Para ti
          </h1>
          <p className="text-[14px] text-muted">
            Basado en tus pelis mejor valoradas (rating &gt;= 4.0)
          </p>
        </div>

        {error && (
          <div className="rounded-md border border-hairline bg-surface-soft p-6 text-center text-[15px] text-muted">
            {error}
          </div>
        )}

        {!error && items === null && <SkeletonGrid />}

        {items && items.length === 0 && (
          <div className="rounded-md border border-hairline bg-surface-soft p-12 text-center">
            <p className="text-[16px] text-ink">Todavía no hay resultados.</p>
            <p className="mt-1 text-[14px] text-muted">
              Necesitamos al menos una peli tuya con rating &gt;= 4.0 en Letterboxd.
            </p>
          </div>
        )}

        {items && items.length > 0 && (
          <ul className="grid grid-cols-1 gap-x-6 gap-y-10 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
            {items.map((rec) => (
              <MovieCard key={rec.movie.id} rec={rec} />
            ))}
          </ul>
        )}
      </main>
    </div>
  )
}

function MovieCard({ rec }: { rec: Recommendation }) {
  const { movie } = rec
  const year = movie.release_date?.slice(0, 4) ?? ''
  const rating = rec.lb_rating ?? movie.vote_average / 2

  return (
    <li className="group">
      <div className="aspect-[2/3] w-full overflow-hidden rounded-md bg-surface-strong">
        <img
          src={`https://image.tmdb.org/t/p/w500/${(movie as unknown as { poster_path?: string }).poster_path ?? ''}`}
          alt=""
          loading="lazy"
          className="h-full w-full object-cover transition-transform duration-500 group-hover:scale-[1.02]"
          onError={(e) => {
            const el = e.currentTarget
            el.style.display = 'none'
          }}
        />
      </div>
      <div className="mt-3 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-[15px] font-medium text-ink">
            {movie.title}
          </p>
          {year && <p className="mt-0.5 text-[13px] text-muted">{year}</p>}
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          <span className="inline-block h-2 w-2 rounded-full bg-accent" />
          <span className="font-mono text-[13px] font-medium text-ink">
            {rating.toFixed(2)}
          </span>
        </div>
      </div>
    </li>
  )
}

function SkeletonGrid() {
  return (
    <ul className="grid grid-cols-1 gap-x-6 gap-y-10 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
      {Array.from({ length: 8 }).map((_, i) => (
        <li key={i}>
          <div className="aspect-[2/3] w-full animate-pulse rounded-md bg-surface-strong" />
          <div className="mt-3 h-4 w-3/4 animate-pulse rounded bg-surface-strong" />
          <div className="mt-2 h-3 w-1/3 animate-pulse rounded bg-surface-strong" />
        </li>
      ))}
    </ul>
  )
}
