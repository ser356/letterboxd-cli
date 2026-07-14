import { invoke } from '@tauri-apps/api/core'

/**
 * Type-safe wrappers around Tauri commands exposed by src/gui.rs.
 *
 * `import.meta.env.TAURI_ENV_PLATFORM` is only defined when running under
 * the Tauri window; on plain `vite dev` outside Tauri, the invokes will
 * throw. `isTauri()` lets components render a fallback in that case.
 */
export function isTauri(): boolean {
  return typeof (window as unknown as { __TAURI_INTERNALS__?: unknown })
    .__TAURI_INTERNALS__ !== 'undefined'
}

export interface Movie {
  id: number
  title: string
  vote_average: number
  popularity: number
  release_date: string | null
}

export interface Recommendation {
  movie: Movie
  score: number
  frequency: number
  lb_rating: number | null
}

export async function getRecommendations(
  count: number,
  minRating: number,
): Promise<Recommendation[]> {
  return invoke<Recommendation[]>('get_recommendations', { count, minRating })
}

export async function login(
  username: string,
  password: string,
): Promise<void> {
  return invoke('login', { username, password })
}

export async function hasSession(): Promise<boolean> {
  return invoke<boolean>('has_session')
}
