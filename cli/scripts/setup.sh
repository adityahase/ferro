#!/usr/bin/env bash
# setup.sh — install the Ferro toolchain dependencies.
#
# Modelled on Frappe's `bench`/easy-install: it brings a fresh machine to the point
# where `ferro build` and `ferro serve` work. Installs (idempotently):
#   - a C toolchain, git, pkg-config, sqlite, libjemalloc        (system package manager)
#   - Rust (rustup) if `cargo` is absent
#   - a CPython built with --enable-shared (for ferrod's embedded interpreter), via pyenv,
#     only if no embeddable Python is already present  (skip with --no-python)
#   - Node.js + Yarn (for the app frontends)                     (skip with --no-node)
#
# Usage:  scripts/setup.sh [--yes] [--no-python] [--no-node] [--python-version X.Y.Z]
set -euo pipefail

YES=0; DO_PYTHON=1; DO_NODE=1; PYVER=3.13.13
while [ $# -gt 0 ]; do
  case "$1" in
    --yes|-y) YES=1 ;;
    --no-python) DO_PYTHON=0 ;;
    --no-node) DO_NODE=0 ;;
    --python-version) shift; PYVER="${1:-$PYVER}" ;;
    --python-version=*) PYVER="${1#*=}" ;;
    *) ;;
  esac
  shift
done

c() { [ -t 2 ] && printf "\033[%sm%s\033[0m" "$1" "$2" || printf "%s" "$2"; }
info()  { printf "%s %s\n" "$(c 36 '==>')" "$*" >&2; }
good()  { printf "%s %s\n" "$(c 32 ' ok')" "$*" >&2; }
warn()  { printf "%s %s\n" "$(c 33 '  !')" "$*" >&2; }
die()   { printf "%s %s\n" "$(c 31 'err')" "$*" >&2; exit 1; }

HOME_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"  # repo root (cli/scripts/ -> ../..)

# ---- sudo / package manager detection -------------------------------------
SUDO=""
if [ "$(id -u)" != "0" ]; then command -v sudo >/dev/null 2>&1 && SUDO="sudo"; fi
PM=""
for p in apt-get dnf yum pacman brew zypper; do command -v "$p" >/dev/null 2>&1 && { PM="$p"; break; }; done
info "package manager: ${PM:-none detected}"

pkg_install() {
  [ -z "$PM" ] && { warn "no package manager found; install manually: $*"; return 0; }
  case "$PM" in
    apt-get) $SUDO apt-get update -y && $SUDO apt-get install -y "$@" ;;
    dnf|yum) $SUDO "$PM" install -y "$@" ;;
    pacman)  $SUDO pacman -Sy --noconfirm "$@" ;;
    zypper)  $SUDO zypper install -y "$@" ;;
    brew)    brew install "$@" ;;
  esac
}

# ---- 1. system build deps + jemalloc + sqlite -----------------------------
# Ferro's Rust deps are self-contained (rusqlite bundles SQLite; no OpenSSL link), so the only
# HARD requirement is a C toolchain + git. jemalloc is dlopened via LD_PRELOAD at runtime (not
# linked), and pkg-config/sqlite-dev are conveniences — so the extras are best-effort: one
# unresolved package name must not abort the whole setup.
info "installing system build dependencies"
WARN_DEPS='some optional system packages failed to install; continuing (a C compiler + git are the only hard requirements)'
case "$PM" in
  apt-get) pkg_install build-essential pkg-config git curl ca-certificates \
             libssl-dev libsqlite3-dev libjemalloc-dev libjemalloc2 || warn "$WARN_DEPS" ;;
  dnf|yum) pkg_install gcc gcc-c++ make pkgconf-pkg-config git curl openssl-devel \
             sqlite-devel jemalloc jemalloc-devel || warn "$WARN_DEPS" ;;
  pacman)  pkg_install base-devel git curl openssl sqlite jemalloc || warn "$WARN_DEPS" ;;
  brew)    pkg_install pkg-config git curl jemalloc sqlite || warn "$WARN_DEPS" ;;
  *)       warn "skipping system packages (unknown PM); ensure a C toolchain + jemalloc are present" ;;
esac
good "system deps done"

# ---- 2. Rust (rustup) -----------------------------------------------------
if command -v cargo >/dev/null 2>&1 || [ -x "$HOME/.cargo/bin/cargo" ]; then
  good "rust present: $(command -v cargo || echo "$HOME/.cargo/bin/cargo")"
else
  info "installing Rust via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1090
  . "$HOME/.cargo/env" 2>/dev/null || true
  good "rust installed"
fi

# ---- 3. embeddable CPython (for ferrod) -----------------------------------
have_embed_python() {
  for py in "${PYO3_PYTHON:-}" "$HOME"/.pyenv/versions/3.1[123]*/bin/python3 python3.13 python3.12; do
    [ -n "$py" ] && command -v "$py" >/dev/null 2>&1 || [ -x "$py" ] || continue
    "$py" -c 'import sysconfig,sys; sys.exit(0 if sysconfig.get_config_var("Py_ENABLE_SHARED")==1 else 1)' 2>/dev/null && { echo "$py"; return 0; }
  done
  return 1
}
if [ "$DO_PYTHON" = 1 ]; then
  if EP="$(have_embed_python)"; then
    good "embeddable CPython present: $EP"
  else
    info "no shared-libpython CPython found; building one with pyenv (--enable-shared)"
    if ! command -v pyenv >/dev/null 2>&1 && [ ! -d "$HOME/.pyenv" ]; then
      curl -fsSL https://pyenv.run | bash || warn "pyenv install failed; install a python built --enable-shared manually"
    fi
    export PYENV_ROOT="$HOME/.pyenv"; export PATH="$PYENV_ROOT/bin:$PATH"
    eval "$(pyenv init - 2>/dev/null)" || true
    case "$PM" in
      apt-get) pkg_install make libssl-dev zlib1g-dev libbz2-dev libreadline-dev \
                 libsqlite3-dev libffi-dev liblzma-dev tk-dev || true ;;
    esac
    info "compiling CPython $PYVER (this takes a few minutes)…"
    PYTHON_CONFIGURE_OPTS="--enable-shared" pyenv install -s "$PYVER" \
      && good "CPython $PYVER (shared) installed under pyenv" \
      || warn "pyenv build failed — ferrod (embedded-Python) won't build; pure-ferro still will"
  fi
else
  warn "skipping Python (--no-python): ferrod won't build, pure-ferro/ferro-native still will"
fi

# ---- 4. Node + Yarn (for app frontends) -----------------------------------
if [ "$DO_NODE" = 1 ]; then
  if command -v node >/dev/null 2>&1; then
    good "node present: $(node --version)"
  else
    info "installing Node.js"
    # run as root whether or not sudo is needed (empty $SUDO must not leave a dangling `-E`)
    as_root() { if [ -n "$SUDO" ]; then $SUDO "$@"; else "$@"; fi; }
    case "$PM" in
      apt-get) if curl -fsSL https://deb.nodesource.com/setup_22.x -o /tmp/nodesource_setup.sh \
                  && as_root bash /tmp/nodesource_setup.sh; then
                 pkg_install nodejs
               else
                 warn "NodeSource setup failed; install Node.js manually for frontend dev"
               fi ;;
      *)       pkg_install nodejs npm || warn "install Node.js manually for frontend dev" ;;
    esac
  fi
  if command -v yarn >/dev/null 2>&1; then
    good "yarn present: $(yarn --version)"
  elif command -v corepack >/dev/null 2>&1; then
    corepack enable && good "yarn enabled via corepack" || warn "enable yarn manually"
  elif command -v npm >/dev/null 2>&1; then
    $SUDO npm install -g yarn && good "yarn installed via npm" || warn "install yarn manually"
  fi
else
  warn 'skipping Node (--no-node): "ferro dev" (frontends) unavailable; backend unaffected'
fi

echo
good "setup complete — running doctor"
"$HOME_DIR/cli/ferro" doctor || true
echo
info "next:  ferro build   &&   ferro bootstrap   (or: ferro init <forge> && ferro new-site …)"
