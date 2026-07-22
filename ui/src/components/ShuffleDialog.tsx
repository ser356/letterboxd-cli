import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { ArrowRight, DiceFive, X } from '@phosphor-icons/react'
import { shufflePick, tmdbPoster, type Movie, type ShuffleFilters } from '../lib/api'
import { useHotkeys, type Hotkey } from '../lib/hotkeys'
import { useT } from '../lib/i18n'

/**
 * Dado atmosférico estilo Filmin. Cuatro preguntas de tono
 * ("¿qué te apetece?", "¿con quién?", "¿cuánto rato?", "¿fresca o
 * clásica?") que el componente mapea internamente a `ShuffleFilters`
 * (géneros TMDB + rangos de año/duración/rating) y le pide al backend
 * una peli al azar del pool resultante. Nunca se le pregunta al user
 * "género" o "década" en literal — el dado se siente como una
 * conversación con un amigo, no como un formulario de metadata.
 *
 * La última decisión SIEMPRE es aleatoria: aunque el user haya
 * elegido "acción, adrenalina, corta, novedad", el backend escoge una
 * página aleatoria dentro del pool y una peli aleatoria de ella, así
 * dos tiradas con los mismos filtros dan resultados distintos.
 */

// ── Tipos de las respuestas del user ──────────────────────────

type Mood =
  | 'laugh'
  | 'cry'
  | 'rush'
  | 'scare'
  | 'escape'
  | 'romance'
  | 'mystery'
  | 'surprise'

type Company = 'alone' | 'couple' | 'friends' | 'family'

type Duration = 'short' | 'medium' | 'long' | 'any'

type Era = 'new' | 'modern' | 'classic' | 'any'

interface Answers {
  mood: Mood | null
  company: Company | null
  duration: Duration | null
  era: Era | null
}

// ── Mapeo a filtros TMDB (DiscoverFilters del backend) ────────

/** IDs oficiales de género en TMDB (`/genre/movie/list`). Congelados
 * aquí para no depender de un fetch al abrir el dado. */
const GENRE = {
  action: 28,
  adventure: 12,
  comedy: 35,
  crime: 80,
  drama: 18,
  family: 10751,
  fantasy: 14,
  horror: 27,
  mystery: 9648,
  romance: 10749,
  scifi: 878,
  thriller: 53,
} as const

/** Traduce las 4 respuestas atmosféricas a los filtros concretos
 * que consume `discover_movies` del backend. Devuelve el DTO con la
 * misma forma que espera el `ShuffleFilters` del api.ts.
 *
 * Reglas:
 *   * `mood` decide los géneros (OR entre varios cuando el mood es
 *     ambiguo — p.ej. "escapar" = SciFi | Fantasy).
 *   * `company = family` fuerza además el género Family como MUST
 *     (el filtro con `|` en `withGenres` los combina como OR: bastan
 *     géneros del mood O Family).
 *   * `duration` marca ventanas de minutos absolutas.
 *   * `era` fija ventanas de año calendárico.
 *   * `voteAvgGte` sube discretamente si el mood implica que el user
 *     quiere calidad probada ("cry" / "mystery"); para el resto se
 *     queda en 6 como suelo razonable.
 */
function answersToFilters(a: Answers): ShuffleFilters {
  const genres = moodGenres(a.mood)
  if (a.company === 'family') {
    // Añade Family al pool de géneros aceptables (OR).
    genres.push(GENRE.family)
  }
  const now = new Date().getFullYear()
  const era = eraWindow(a.era, now)
  const dur = durationWindow(a.duration)

  return {
    withGenres: genres.length > 0 ? genres : undefined,
    releaseYearGte: era.gte,
    releaseYearLte: era.lte,
    runtimeGte: dur.gte,
    runtimeLte: dur.lte,
    voteAvgGte: a.mood === 'cry' || a.mood === 'mystery' ? 6.5 : 6,
  }
}

function moodGenres(mood: Mood | null): number[] {
  switch (mood) {
    case 'laugh':
      return [GENRE.comedy]
    case 'cry':
      return [GENRE.drama]
    case 'rush':
      return [GENRE.action, GENRE.thriller]
    case 'scare':
      return [GENRE.horror]
    case 'escape':
      return [GENRE.scifi, GENRE.fantasy, GENRE.adventure]
    case 'romance':
      return [GENRE.romance]
    case 'mystery':
      return [GENRE.mystery, GENRE.crime]
    case 'surprise':
    case null:
    default:
      return []
  }
}

function eraWindow(era: Era | null, now: number): { gte?: number; lte?: number } {
  switch (era) {
    case 'new':
      return { gte: now - 2 }
    case 'modern':
      return { gte: 2000, lte: now - 3 }
    case 'classic':
      return { lte: 1999 }
    default:
      return {}
  }
}

function durationWindow(d: Duration | null): { gte?: number; lte?: number } {
  switch (d) {
    case 'short':
      return { lte: 95 }
    case 'medium':
      return { gte: 95, lte: 130 }
    case 'long':
      return { gte: 130 }
    default:
      return {}
  }
}

// ── Componente ─────────────────────────────────────────────────

interface Step<T extends string> {
  key: keyof Answers
  question: string
  options: { value: T; label: string }[]
}

export function ShuffleDialog({ onClose }: { onClose: () => void }) {
  const t = useT()
  const nav = useNavigate()

  const [step, setStep] = useState(0)
  const [answers, setAnswers] = useState<Answers>({
    mood: null,
    company: null,
    duration: null,
    era: null,
  })
  const [rolling, setRolling] = useState(false)
  const [result, setResult] = useState<Movie | null>(null)
  const [empty, setEmpty] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Los pasos se declaran DENTRO del render para que la i18n se
  // resuelva con el locale actual (cambio en Ajustes → los labels
  // rehidratan). Cada paso conoce su key y las opciones válidas.
  const steps: [
    Step<Mood>,
    Step<Company>,
    Step<Duration>,
    Step<Era>,
  ] = useMemo(
    () => [
      {
        key: 'mood',
        question: t('shuffle.q.mood'),
        options: [
          { value: 'laugh', label: t('shuffle.mood.laugh') },
          { value: 'cry', label: t('shuffle.mood.cry') },
          { value: 'rush', label: t('shuffle.mood.rush') },
          { value: 'scare', label: t('shuffle.mood.scare') },
          { value: 'escape', label: t('shuffle.mood.escape') },
          { value: 'romance', label: t('shuffle.mood.romance') },
          { value: 'mystery', label: t('shuffle.mood.mystery') },
          { value: 'surprise', label: t('shuffle.mood.surprise') },
        ],
      },
      {
        key: 'company',
        question: t('shuffle.q.company'),
        options: [
          { value: 'alone', label: t('shuffle.company.alone') },
          { value: 'couple', label: t('shuffle.company.couple') },
          { value: 'friends', label: t('shuffle.company.friends') },
          { value: 'family', label: t('shuffle.company.family') },
        ],
      },
      {
        key: 'duration',
        question: t('shuffle.q.duration'),
        options: [
          { value: 'short', label: t('shuffle.duration.short') },
          { value: 'medium', label: t('shuffle.duration.medium') },
          { value: 'long', label: t('shuffle.duration.long') },
          { value: 'any', label: t('shuffle.duration.any') },
        ],
      },
      {
        key: 'era',
        question: t('shuffle.q.era'),
        options: [
          { value: 'new', label: t('shuffle.era.new') },
          { value: 'modern', label: t('shuffle.era.modern') },
          { value: 'classic', label: t('shuffle.era.classic') },
          { value: 'any', label: t('shuffle.era.any') },
        ],
      },
    ],
    [t],
  )

  const totalSteps = steps.length
  const isPickPhase = result != null || rolling || empty || error != null

  // Cerrar con Escape también cuando ya hay resultado.
  const hotkeys: Hotkey[] = [
    { key: 'Escape', hint: '', run: onClose, ignoreInInput: false },
  ]
  useHotkeys(hotkeys, [])

  const roll = async (currentAnswers: Answers) => {
    setRolling(true)
    setError(null)
    setEmpty(false)
    setResult(null)
    try {
      const movie = await shufflePick(answersToFilters(currentAnswers))
      if (!movie) {
        setEmpty(true)
      } else {
        setResult(movie)
      }
    } catch (e) {
      setError(String(e))
    } finally {
      setRolling(false)
    }
  }

  const chooseOption = <T extends Mood | Company | Duration | Era>(
    stepIdx: number,
    value: T,
  ) => {
    const s = steps[stepIdx]
    const next = { ...answers, [s.key]: value } as Answers
    setAnswers(next)
    if (stepIdx + 1 < totalSteps) {
      setStep(stepIdx + 1)
    } else {
      void roll(next)
    }
  }

  const openResult = () => {
    if (!result) return
    onClose()
    // Series NO llegan por `/discover/movie` (endpoint es solo pelis),
    // así que siempre navegamos a la ruta de torrents de peli.
    const year = result.release_date?.slice(0, 4)
    const suffix = year ? `&year=${year}` : ''
    nav(
      `/torrents/tmdb/${result.id}?title=${encodeURIComponent(result.title)}${suffix}`,
    )
  }

  return (
    <div
      onClick={onClose}
      className="fixed inset-0 z-50 flex items-center justify-center bg-scrim/70 backdrop-blur-sm"
      role="dialog"
      aria-label={t('shuffle.title')}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        className="glass-strong flex w-full max-w-[560px] flex-col gap-5 rounded-xl p-6"
      >
        <header className="flex items-start justify-between gap-4">
          <div>
            <p className="flex items-center gap-2 text-[11px] uppercase tracking-wide text-dim">
              <DiceFive size={14} weight="fill" className="text-accent" />
              {t('shuffle.title')}
            </p>
            <h2 className="mt-1 text-[18px] font-semibold text-ink">
              {isPickPhase ? t('shuffle.rolling') : steps[step].question}
            </h2>
            {!isPickPhase && (
              <p className="mt-1 text-[11px] text-muted">
                {t('shuffle.subtitle')}
              </p>
            )}
          </div>
          <button
            onClick={onClose}
            aria-label={t('shuffle.close')}
            className="flex h-8 w-8 items-center justify-center rounded-full text-muted hover:bg-surface hover:text-ink"
          >
            <X size={16} weight="bold" />
          </button>
        </header>

        {!isPickPhase && (
          <>
            <div className="grid grid-cols-2 gap-2">
              {steps[step].options.map((opt) => (
                <button
                  key={opt.value}
                  onClick={() => chooseOption(step, opt.value)}
                  className="focus-ring group flex items-center justify-between gap-3 rounded-lg border border-hairline bg-surface px-4 py-3 text-left text-[13px] text-body transition-colors hover:border-accent/50 hover:bg-accent/10 hover:text-ink"
                >
                  <span className="truncate">{opt.label}</span>
                  <ArrowRight
                    size={14}
                    weight="bold"
                    className="shrink-0 text-dim transition-transform group-hover:translate-x-0.5 group-hover:text-accent"
                  />
                </button>
              ))}
            </div>

            <footer className="flex items-center justify-between border-t border-hairline pt-3 text-[11px] text-dim">
              <span>{t('shuffle.step', { n: step + 1, total: totalSteps })}</span>
              <div className="flex gap-1">
                {steps.map((_, i) => (
                  <span
                    key={i}
                    className={`h-1.5 w-4 rounded-full transition-colors ${
                      i <= step ? 'bg-accent' : 'bg-hairline'
                    }`}
                  />
                ))}
              </div>
            </footer>
          </>
        )}

        {isPickPhase && rolling && (
          <div className="flex items-center justify-center py-8">
            <div className="h-10 w-10 animate-spin rounded-full border-2 border-accent border-t-transparent" />
          </div>
        )}

        {isPickPhase && !rolling && empty && (
          <div className="flex flex-col items-center gap-3 py-6 text-center">
            <p className="text-[14px] text-body">{t('shuffle.empty')}</p>
            <button
              onClick={() => {
                // Volver al primer paso para relajar filtros.
                setEmpty(false)
                setResult(null)
                setStep(0)
              }}
              className="focus-ring rounded-full border border-accent/40 bg-accent/10 px-4 py-1.5 text-[13px] font-medium text-accent hover:bg-accent/20"
            >
              {t('shuffle.retry')}
            </button>
          </div>
        )}

        {isPickPhase && !rolling && error && (
          <div className="rounded-md border border-danger/40 bg-danger/10 p-3 text-[13px] text-danger">
            {error}
          </div>
        )}

        {isPickPhase && !rolling && result && (
          <div className="flex items-start gap-4">
            {result.poster_path && (
              <img
                src={tmdbPoster(result.poster_path) ?? ''}
                alt={`Poster de ${result.title}`}
                loading="lazy"
                draggable={false}
                className="pointer-events-none h-40 w-28 shrink-0 select-none rounded-md object-cover"
              />
            )}
            <div className="flex min-w-0 flex-1 flex-col gap-2">
              <h3 className="text-[16px] font-semibold text-ink">{result.title}</h3>
              <p className="text-[12px] text-muted">
                {result.release_date?.slice(0, 4) ?? '—'}
                {result.vote_average > 0 && (
                  <>
                    <span className="mx-1.5 text-dim">·</span>
                    <span>★ {result.vote_average.toFixed(1)}</span>
                  </>
                )}
              </p>
              <div className="mt-auto flex flex-wrap gap-2">
                <button
                  onClick={openResult}
                  className="focus-ring rounded-full bg-accent px-4 py-1.5 text-[13px] font-medium text-on-accent transition-colors hover:brightness-110"
                >
                  {t('shuffle.play')}
                </button>
                <button
                  onClick={() => void roll(answers)}
                  className="focus-ring rounded-full border border-hairline px-4 py-1.5 text-[13px] text-body transition-colors hover:border-border-strong hover:text-ink"
                >
                  {t('shuffle.retry')}
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
