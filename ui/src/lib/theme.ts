/**
 * Aplica el valor de "liquid glass opacity" (0..=100) a la variable CSS
 * `--glass-opaque` en `:root`. Todas las utilidades `.glass`,
 * `.glass-strong` y `.popover` lo leen para interpolar entre el look
 * translúcido por defecto (0) y superficies casi sólidas (100).
 */
export function applyGlassOpacity(value: number) {
  const clamped = Math.min(100, Math.max(0, value))
  document.documentElement.style.setProperty(
    '--glass-opaque',
    (clamped / 100).toFixed(3),
  )
}
