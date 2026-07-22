import { useEffect, useState } from 'react'
import { ContextMenu } from './ContextMenu'
import { useT } from '../lib/i18n'

/**
 * Fallback global de menú contextual. Al hacer clic derecho en
 * cualquier lugar de la app, si NINGÚN handler descendiente llamó a
 * `e.preventDefault()`, mostramos un mini-menú con
 * `menu.noActions` ("No hay acciones a realizar") en el mismo estilo
 * visual que el resto de la app.
 *
 * Sin este componente, el right-click en zonas sin handler propio
 * abre el context menu nativo del WebView (Reload / Inspect en
 * Chromium, "Buscar en Google" en macOS…) que es feo, revela
 * plumbing y no encaja con el look.
 *
 * Convive con los menús ricos (Recommendations, Torrents, MovieCard):
 * esos handlers llaman a `preventDefault()` en su SyntheticEvent, lo
 * que marca el evento nativo como consumido y aquí lo respetamos.
 */
export function GlobalContextMenu() {
  const t = useT()
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null)

  useEffect(() => {
    const onCtx = (e: MouseEvent) => {
      // Un handler descendiente ya se hizo cargo (React synthetic
      // event pasa el preventDefault al nativo).
      if (e.defaultPrevented) return
      e.preventDefault()
      setMenu({ x: e.clientX, y: e.clientY })
    }
    document.addEventListener('contextmenu', onCtx)
    return () => document.removeEventListener('contextmenu', onCtx)
  }, [])

  if (!menu) return null
  return (
    <ContextMenu
      x={menu.x}
      y={menu.y}
      onClose={() => setMenu(null)}
      items={[
        {
          label: t('menu.noActions'),
          onClick: () => {},
          disabled: true,
        },
      ]}
    />
  )
}
