import { MagnifyingGlass } from '@phosphor-icons/react'
import { useNavigate } from 'react-router-dom'
import { TopNav } from '../components/TopNav'

/**
 * Home. Hero-driven: pill search bar centered, single primary CTA below.
 * The search bar is the app's discovery entry point; recommendations
 * button is the alternate flow for users who want curated results.
 */
export function Home() {
  const nav = useNavigate()

  return (
    <div className="min-h-[100dvh] bg-canvas">
      <TopNav>
        <a href="/recommendations" className="hover:text-ink transition-colors">
          Recomendaciones
        </a>
        <button
          onClick={() => nav('/login')}
          className="rounded-full border border-hairline px-4 py-2 text-ink hover:border-border-strong transition-colors"
        >
          Iniciar sesión
        </button>
      </TopNav>

      <main className="mx-auto max-w-[1280px] px-8 pt-24 pb-32">
        <section className="mx-auto max-w-[720px] text-center">
          <h1 className="text-[44px] font-semibold leading-[1.1] tracking-[-0.02em] text-ink">
            Descubre qué ver esta noche
          </h1>
          <p className="mx-auto mt-4 max-w-[52ch] text-[17px] leading-relaxed text-muted">
            Recomendaciones basadas en tu historial de Letterboxd, con torrent
            listo y streaming embebido cuando quieras verlo ya.
          </p>

          <form
            onSubmit={(e) => {
              e.preventDefault()
              const q = new FormData(e.currentTarget).get('q')?.toString().trim()
              if (q) nav(`/recommendations?q=${encodeURIComponent(q)}`)
            }}
            className="mx-auto mt-10 flex h-[64px] w-full max-w-[560px] items-center rounded-full border border-hairline bg-canvas pl-6 pr-2 shadow-card focus-within:border-border-strong"
          >
            <MagnifyingGlass size={20} className="text-muted" weight="bold" />
            <input
              name="q"
              type="text"
              placeholder="Buscar por título, director, año..."
              className="mx-3 flex-1 bg-transparent text-[15px] text-ink placeholder:text-muted focus:outline-none"
            />
            <button
              type="submit"
              className="flex h-12 w-12 items-center justify-center rounded-full bg-accent text-on-accent transition-colors hover:bg-accent-hover active:scale-[0.96]"
              aria-label="Buscar"
            >
              <MagnifyingGlass size={18} weight="bold" />
            </button>
          </form>

          <button
            onClick={() => nav('/recommendations')}
            className="mt-6 rounded-sm bg-ink px-6 py-3 text-[15px] font-medium text-on-accent transition-colors hover:bg-body active:scale-[0.98]"
          >
            Ver recomendaciones para ti
          </button>
        </section>
      </main>
    </div>
  )
}
