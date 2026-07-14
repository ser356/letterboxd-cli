import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { TopNav } from '../components/TopNav'
import { isTauri, login } from '../lib/api'

/**
 * Login. Reuses the existing OAuth password grant flow in src/auth.rs
 * exposed as the `login` Tauri command. On success, credentials.json is
 * written by the backend and we route to /recommendations.
 */
export function Login() {
  const nav = useNavigate()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  return (
    <div className="min-h-[100dvh] bg-canvas">
      <TopNav>
        <a href="/" className="hover:text-ink transition-colors">
          Volver
        </a>
      </TopNav>

      <main className="mx-auto flex max-w-[520px] flex-col px-8 pt-20 pb-20">
        <h1 className="text-[28px] font-semibold leading-tight text-ink">
          Inicia sesión en Letterboxd
        </h1>
        <p className="mt-2 text-[15px] leading-relaxed text-muted">
          Necesitamos tus credenciales para leer tu historial y watchlist.
          Se guardan en local; nada sale de tu máquina.
        </p>

        <form
          onSubmit={async (e) => {
            e.preventDefault()
            if (!isTauri()) {
              setError('Esta ventana solo funciona dentro de la app de escritorio')
              return
            }
            setBusy(true)
            setError(null)
            const data = new FormData(e.currentTarget)
            try {
              await login(
                data.get('username')?.toString().trim() ?? '',
                data.get('password')?.toString() ?? '',
              )
              nav('/recommendations')
            } catch (err) {
              setError(String(err))
            } finally {
              setBusy(false)
            }
          }}
          className="mt-10 flex flex-col gap-4"
        >
          <label className="flex flex-col gap-2 text-[13px] font-medium text-muted">
            Usuario
            <input
              name="username"
              autoComplete="username"
              required
              className="h-14 rounded-sm border border-hairline bg-canvas px-4 text-[16px] text-ink focus:border-ink focus:outline-none"
            />
          </label>

          <label className="flex flex-col gap-2 text-[13px] font-medium text-muted">
            Contraseña
            <input
              name="password"
              type="password"
              autoComplete="current-password"
              required
              className="h-14 rounded-sm border border-hairline bg-canvas px-4 text-[16px] text-ink focus:border-ink focus:outline-none"
            />
          </label>

          {error && (
            <p
              role="alert"
              className="rounded-sm border border-danger/30 bg-danger/5 px-4 py-3 text-[14px] text-danger"
            >
              {error}
            </p>
          )}

          <button
            type="submit"
            disabled={busy}
            className="mt-2 h-12 rounded-sm bg-accent text-[16px] font-medium text-on-accent transition-colors hover:bg-accent-hover disabled:bg-accent-disabled disabled:cursor-not-allowed active:scale-[0.98]"
          >
            {busy ? 'Verificando...' : 'Entrar'}
          </button>
        </form>
      </main>
    </div>
  )
}
