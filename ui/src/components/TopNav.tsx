import type { PropsWithChildren } from 'react'

/**
 * Top navigation shell. Airbnb-inspired: 72px height, hairline bottom,
 * left brand, center empty (search moves into the hero on Home), right
 * account entry.
 */
export function TopNav({ children }: PropsWithChildren) {
  return (
    <header className="sticky top-0 z-40 h-[72px] bg-canvas border-b border-hairline-soft">
      <div className="mx-auto flex h-full max-w-[1280px] items-center justify-between px-8">
        <a href="/" className="flex items-center gap-2 font-semibold text-ink">
          <span
            aria-hidden
            className="inline-block h-3 w-3 rounded-full bg-accent"
          />
          <span className="text-[17px]">letterboxd</span>
        </a>
        <nav className="flex items-center gap-6 text-[15px] text-muted">
          {children}
        </nav>
      </div>
    </header>
  )
}
