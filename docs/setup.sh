#!/usr/bin/env bash
# videodrome — installer universal para macOS y Linux
#
# Uso:
#   curl -fsSL https://ser356.github.io/videodrome/setup.sh | bash
#
# Qué hace:
#   1. Detecta el sistema (macOS / Linux / Windows via WSL/git-bash).
#   2. En macOS: instala Homebrew si no existe (script oficial) →
#      tap ser356/cask → brew install --cask videodrome. ffmpeg se
#      instala como dependencia obligatoria del cask (VLC ya no).
#   3. En Linux: descarga el tarball de la última release en
#      ~/.local/bin. Recuerda al user instalar ffmpeg con su gestor
#      (necesario para el player embebido).
#   4. En Windows (git-bash / WSL): imprime el one-liner de PowerShell
#      y sale — el flujo Windows es Scoop, no bash.
#
# Seguridad: es el patrón "curl | bash" estándar. Si te preocupa,
# descarga primero e inspecciona:
#   curl -fsSL https://ser356.github.io/videodrome/setup.sh -o setup.sh
#   less setup.sh
#   bash setup.sh

set -euo pipefail

# ── Estilo ───────────────────────────────────────────────────────────────
if [ -t 1 ]; then
  BOLD=$(printf '\033[1m'); DIM=$(printf '\033[2m')
  CYAN=$(printf '\033[36m'); GREEN=$(printf '\033[32m')
  YELLOW=$(printf '\033[33m'); RED=$(printf '\033[31m')
  RESET=$(printf '\033[0m')
else
  BOLD=""; DIM=""; CYAN=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi

step()  { printf "%s==>%s %s\n" "$CYAN" "$RESET" "$*"; }
ok()    { printf "%s✔%s %s\n" "$GREEN" "$RESET" "$*"; }
warn()  { printf "%s!%s %s\n" "$YELLOW" "$RESET" "$*" >&2; }
fail()  { printf "%s✗%s %s\n" "$RED" "$RESET" "$*" >&2; exit 1; }

REPO="ser356/videodrome"
TAP="ser356/cask"
TAP_URL="https://github.com/ser356/homebrew-cask"

# ── Detección de plataforma ──────────────────────────────────────────────
os_name="$(uname -s)"
arch_name="$(uname -m)"

case "$os_name" in
  Darwin) OS="macos" ;;
  Linux)
    if grep -qi microsoft /proc/version 2>/dev/null; then
      OS="wsl"
    else
      OS="linux"
    fi
    ;;
  MINGW*|MSYS*|CYGWIN*) OS="windows-bash" ;;
  *) fail "SO no soportado: $os_name" ;;
esac

case "$arch_name" in
  arm64|aarch64) ARCH="arm64" ;;
  x86_64|amd64)  ARCH="x86_64" ;;
  *) warn "Arquitectura no reconocida: $arch_name (se asumirá x86_64)" ; ARCH="x86_64" ;;
esac

printf "%svideodrome installer%s  %s(%s / %s)%s\n\n" \
  "$BOLD" "$RESET" "$DIM" "$OS" "$ARCH" "$RESET"

# ── Windows bash → redirige a Scoop ─────────────────────────────────────
if [ "$OS" = "windows-bash" ] || [ "$OS" = "wsl" ]; then
  warn "En Windows/WSL este script no aplica. Usa el installer nativo:"
  cat <<EOF

  Abre PowerShell (NO administrador) y ejecuta:

    ${BOLD}irm https://ser356.github.io/videodrome/install.ps1 | iex${RESET}

  Eso instala Scoop (si no existe), añade el bucket ser356 y
  descarga el binario prebuilt de videodrome.

EOF
  exit 0
fi

# ── macOS: Homebrew + cask ──────────────────────────────────────────────
install_macos() {
  if ! command -v brew >/dev/null 2>&1; then
    # Homebrew necesita sudo (crea /opt/homebrew y hace chown). Cuando
    # este script se ejecuta vía `curl | bash`, stdin es un pipe, no un
    # TTY → sudo no puede pedir password y falla con "a terminal is
    # required". Detectamos ese caso y redirigimos stdin a /dev/tty
    # antes de invocar al installer oficial.
    step "Homebrew no encontrado. Vamos a instalarlo (te pedirá sudo)."

    local brew_installer
    brew_installer=$(mktemp)
    curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh \
      -o "$brew_installer" \
      || fail "No pude descargar el installer de Homebrew."

    if [ -t 0 ]; then
      # Terminal interactivo: dejar que el script pregunte lo que
      # necesite. No poner NONINTERACTIVE — el usuario tiene TTY.
      /bin/bash "$brew_installer" \
        || { rm -f "$brew_installer"; fail "Fallo instalando Homebrew."; }
    elif [ -e /dev/tty ]; then
      # Stdin es pipe (curl|bash) pero hay /dev/tty accesible.
      # Redirigimos stdin del installer al terminal real para que sudo
      # pueda leer el password.
      warn "Estás ejecutando por pipe (curl|bash). Redirigiendo la entrada a tu terminal para el prompt de sudo..."
      /bin/bash "$brew_installer" </dev/tty \
        || { rm -f "$brew_installer"; fail "Fallo instalando Homebrew."; }
    else
      rm -f "$brew_installer"
      cat >&2 <<EOF

${YELLOW}!${RESET} No tienes Homebrew y no hay terminal interactivo (${DIM}no /dev/tty${RESET}).
  Instala Homebrew manualmente y reejecuta este script:

    ${BOLD}/bin/bash -c "\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"${RESET}
    ${BOLD}curl -fsSL https://ser356.github.io/videodrome/setup.sh | bash${RESET}

EOF
      exit 1
    fi
    rm -f "$brew_installer"

    # Añadir brew al PATH del shell actual — el script de Homebrew
    # deja `brew shellenv` en el profile pero no lo carga en esta sesión.
    if [ -x /opt/homebrew/bin/brew ]; then
      eval "$(/opt/homebrew/bin/brew shellenv)"
    elif [ -x /usr/local/bin/brew ]; then
      eval "$(/usr/local/bin/brew shellenv)"
    fi
  else
    ok "Homebrew ya instalado."
  fi

  step "Añadiendo tap $TAP..."
  # Homebrew 4.5+ pide `brew tap --force-auto-update` o `brew trust`
  # para taps de terceros. El comando estándar `brew tap` sigue
  # funcionando; si el tap ya existe es no-op.
  brew tap "$TAP" "$TAP_URL" >/dev/null || true

  # `brew trust` es requerido desde Homebrew 4.5+ para taps de terceros:
  # sin él, `brew install <tap>/<cask>` puede fallar con "untrusted tap"
  # o pedir confirmación interactiva (que rompe el flujo curl|bash).
  # El subcomando puede no existir en instalaciones muy viejas → || true.
  step "Confirmando confianza en el tap..."
  brew trust "$TAP" >/dev/null 2>&1 || true

  step "Instalando videodrome (arrastra ffmpeg como dep si no lo tienes)..."
  brew install --cask videodrome

  ok "Listo. Abre Videodrome desde Launchpad o ejecuta 'videodrome' en el terminal."
}

# ── Linux: tarball + ~/.local/bin ───────────────────────────────────────
install_linux() {
  local target_dir="${VIDEODROME_PREFIX:-$HOME/.local/bin}"
  mkdir -p "$target_dir"

  step "Resolviendo la última release en GitHub..."
  local tag
  tag=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' | head -n1 | cut -d '"' -f 4)
  [ -n "$tag" ] || fail "No pude leer la última release desde la API de GitHub."
  ok "Última release: $tag"

  # Los assets de Linux se llaman videodrome-{TAG}-linux-{ARCH}.tar.gz
  local url="https://github.com/${REPO}/releases/download/${tag}/videodrome-${tag}-linux-${ARCH}.tar.gz"
  local tmp; tmp=$(mktemp -d)
  step "Descargando $(basename "$url")..."
  if ! curl -fL --progress-bar "$url" -o "$tmp/videodrome.tar.gz"; then
    rm -rf "$tmp"
    fail "No pude descargar el tarball. ¿Existe un asset para linux-${ARCH} en $tag?"
  fi

  step "Extrayendo en $target_dir..."
  tar -xzf "$tmp/videodrome.tar.gz" -C "$tmp"
  # El tarball trae el binario suelto (`videodrome`).
  if [ -f "$tmp/videodrome" ]; then
    install -m 0755 "$tmp/videodrome" "$target_dir/videodrome"
  else
    fail "Tarball inesperado (no encontré 'videodrome' dentro)."
  fi
  rm -rf "$tmp"

  ok "Binario en $target_dir/videodrome"

  if ! command -v videodrome >/dev/null 2>&1; then
    case ":$PATH:" in
      *":$target_dir:"*) : ;;
      *) warn "Añade $target_dir a tu PATH:"
         printf "  echo 'export PATH=\"%s:\$PATH\"' >> ~/.bashrc\n" "$target_dir"
         ;;
    esac
  fi

  # Dependencia REAL: ffmpeg. El player embebido lo usa para
  # transmux (MKV/HEVC/VP9 → fMP4 fragmentado sobre HLS). Sin él
  # la reproducción no arranca.
  if ! command -v ffmpeg >/dev/null 2>&1 || ! command -v ffprobe >/dev/null 2>&1; then
    warn "ffmpeg no está instalado. Necesario para el player embebido."
    if command -v apt-get >/dev/null 2>&1; then
      printf "     %ssudo apt install ffmpeg%s\n" "$BOLD" "$RESET"
    elif command -v dnf >/dev/null 2>&1; then
      printf "     %ssudo dnf install ffmpeg%s\n" "$BOLD" "$RESET"
    elif command -v pacman >/dev/null 2>&1; then
      printf "     %ssudo pacman -S ffmpeg%s\n" "$BOLD" "$RESET"
    else
      printf "     Instálalo desde tu gestor de paquetes o https://ffmpeg.org\n"
    fi
  fi

  ok "Listo. Ejecuta 'videodrome' o 'videodrome tui'."
}

case "$OS" in
  macos) install_macos ;;
  linux) install_linux ;;
esac
