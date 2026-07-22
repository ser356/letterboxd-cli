import type { PropsWithChildren, ReactNode } from 'react'
import { useState } from 'react'
import { DiceFive, Gear } from '@phosphor-icons/react'
import { Link, useLocation, useNavigate } from 'react-router-dom'
import { useHotkeys, type Hotkey } from '../lib/hotkeys'
import { useT } from '../lib/i18n'
import { ShuffleDialog } from './ShuffleDialog'

/**
 * Top navigation. Layout:
 *
 *   [traffic-lights space (macOS)] [back?] [logo] .... [children] [gear]
 *
 * `back` es un slot opcional que se pinta ANTES del wordmark — así el
 * botón de volver queda a la izquierda del todo (convención de macOS
 * / iOS / la mayoría de apps nativas), no mezclado con los controles
 * de la vista. Antes vivía en `children` y quedaba a la derecha, cerca
 * de la barra de búsqueda, lo cual era contraintuitivo.
 *
 * En macOS, `tauri.conf.json` usa `titleBarStyle: Overlay` para que la
 * ventana no tenga barra nativa. Los traffic lights (cerrar / minimizar
 * / maximizar) quedan flotando arriba-izquierda, así que dejamos ~86px
 * de padding-left cuando estamos en macOS para no taparlos.
 *
 * Además, el `data-tauri-drag-region` permite arrastrar la ventana
 * agarrando la barra vacía (sustituto de la titlebar nativa).
 *
 * Muestra un icono de engranaje que navega a `/settings` en todas las
 * vistas EXCEPTO Home (donde ya hay un botón "Ajustes" explícito) y la
 * propia vista de Ajustes (evita el "botón que va a la página actual").
 */
export function TopNav({
  back,
  children,
}: PropsWithChildren<{ back?: ReactNode }>) {
  const t = useT()
  const isMac =
    typeof navigator !== 'undefined' &&
    navigator.platform.toLowerCase().includes('mac')

  const location = useLocation()
  const nav = useNavigate()
  const showGear = location.pathname !== '/' && location.pathname !== '/settings'

  // Dado atmosférico (estilo Filmin): visible en cualquier pantalla
  // salvo el player fullscreen (que no monta TopNav) y la propia
  // /settings (allí no encaja con el foco de configuración).
  const showShuffle = location.pathname !== '/settings'
  const [shuffleOpen, setShuffleOpen] = useState(false)

  // Hotkey global "," (coma) para saltar a Ajustes desde cualquier vista
  // que monte el TopNav. Se registra aquí para no tener que replicarla
  // en el array de hotkeys de cada view. La convención "," proviene de
  // Cmd+, en macOS.
  const gearHotkey: Hotkey[] = showGear
    ? [{ key: ',', hint: '', run: () => nav('/settings') }]
    : []
  // Hotkey global "d" (dado) para abrir el shuffle desde teclado.
  const shuffleHotkey: Hotkey[] = showShuffle
    ? [{ key: 'd', hint: '', run: () => setShuffleOpen(true) }]
    : []
  useHotkeys([...gearHotkey, ...shuffleHotkey], [showGear, showShuffle])

  return (
    <>
      <header
        data-tauri-drag-region
        className="glass sticky top-0 z-30 h-[56px] rounded-none"
      >
        <div
          data-tauri-drag-region
          className="mx-auto flex h-full max-w-[1400px] items-center gap-4 px-8"
          style={isMac ? { paddingLeft: '86px' } : undefined}
        >
          {back}
          <Link
            to="/"
            className="focus-ring rounded-md text-[17px] font-semibold tracking-tight text-ink"
            aria-label={t('nav.home')}
          >
            videodrome
          </Link>
          <nav className="ml-auto flex items-center gap-3 text-[14px] text-muted">
            {children}
            {showShuffle && (
              <button
                onClick={() => setShuffleOpen(true)}
                aria-label={t('nav.shuffle')}
                title={`${t('nav.shuffle')} (d)`}
                className="focus-ring flex h-9 w-9 items-center justify-center rounded-full border border-hairline text-body transition-colors hover:border-border-strong hover:text-accent"
              >
                <DiceFive size={16} weight="bold" />
              </button>
            )}
            {showGear && (
              <button
                onClick={() => nav('/settings')}
                aria-label={t('nav.settings')}
                title={`${t('nav.settings')} (,)`}
                className="focus-ring flex h-9 w-9 items-center justify-center rounded-full border border-hairline text-body transition-colors hover:border-border-strong hover:text-ink"
              >
                <Gear size={16} weight="bold" />
              </button>
            )}
          </nav>
        </div>
      </header>
      {/* ShuffleDialog OUT of `<header>`: el header tiene
        * `backdrop-blur-lg`, que en Chromium crea un containing
        * block nuevo para `position: fixed` (spec CSS
        * "will-change/filter creates a new stacking context AND
        * containing block for fixed"). Si el modal viviera dentro,
        * su `fixed inset-0` se acotaba al header de 56px y el
        * di\u00e1logo aparec\u00eda pegado a la barra en vez de
        * ocupando toda la pantalla. */}
      {shuffleOpen && <ShuffleDialog onClose={() => setShuffleOpen(false)} />}
    </>
  )
}

