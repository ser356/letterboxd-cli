import { useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { HotkeyBar } from '../components/HotkeyBar'
import { TopNav } from '../components/TopNav'
import { isTauri, login } from '../lib/api'
import { useHotkeys, type Hotkey } from '../lib/hotkeys'
import { useT } from '../lib/i18n'

/**
 * Login. `grant_type=password` contra la API de Letterboxd (reusa
 * `auth::login_with_password` en el backend). Al éxito se persiste
 * refresh_token en credentials.json y se redirige a `?next=...` o
 * a `/recs` por defecto.
 */
export function Login() {
  const nav = useNavigate()
  const t = useT()
  const [params] = useSearchParams()
  const next = params.get('next') ?? '/recs'
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const submitForm = () => {
    const form = document.getElementById('login-form') as HTMLFormElement | null
    form?.requestSubmit()
  }

  const hotkeys: Hotkey[] = [
    {
      key: 'Escape',
      hint: t('common.back'),
      run: () => nav('/'),
      ignoreInInput: false,
    },
    { key: 'Enter', hint: t('login.submit'), run: submitForm, ignoreInInput: false },
  ]
  useHotkeys(hotkeys, [])

  return (
    <div className="flex min-h-[100dvh] flex-col bg-canvas">
      <TopNav />

      <main className="mx-auto flex w-full max-w-[440px] flex-1 flex-col justify-center px-8">
        <h1 className="text-[24px] font-semibold text-ink">
          {t('login.title')}
        </h1>
        <p className="mt-2 text-[14px] leading-relaxed text-muted">
          {t('login.hint')}
        </p>

        <form
          id="login-form"
          onSubmit={async (e) => {
            e.preventDefault()
            if (!isTauri()) {
              setError(t('login.onlyDesktop'))
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
              nav(next)
            } catch (err) {
              setError(String(err))
            } finally {
              setBusy(false)
            }
          }}
          className="mt-8 flex flex-col gap-3"
        >
          <label className="flex flex-col gap-1.5 text-[12px] font-medium uppercase tracking-wide text-dim">
            {t('login.username')}
            <input
              name="username"
              autoComplete="username"
              autoFocus
              required
              className="focus-ring h-11 rounded-md border border-hairline bg-surface px-3 text-[15px] text-ink placeholder:text-dim"
            />
          </label>

          <label className="flex flex-col gap-1.5 text-[12px] font-medium uppercase tracking-wide text-dim">
            {t('login.password')}
            <input
              name="password"
              type="password"
              autoComplete="current-password"
              required
              className="focus-ring h-11 rounded-md border border-hairline bg-surface px-3 text-[15px] text-ink placeholder:text-dim"
            />
          </label>

          {error && (
            <p
              role="alert"
              className="mt-1 rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-[13px] text-danger"
            >
              {error}
            </p>
          )}

          <button
            type="submit"
            disabled={busy}
            className="focus-ring mt-2 h-11 rounded-full bg-accent text-[15px] font-semibold text-on-accent transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
          >
            {busy ? t('login.verifying') : t('login.submit')}
          </button>
        </form>
      </main>

      <HotkeyBar hotkeys={hotkeys} />
    </div>
  )
}
