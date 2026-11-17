#!/usr/bin/env bash
# Install Alcedo from source on Ubuntu 26.04 (Resolute / GNOME 50).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PREFIX="${PREFIX:-$HOME/.local}"
INSTALL_DEPS=1
BUILD=1
BUILD_FLAGS=(--release)
FORCE_OS=0

MSRV="1.93"
UBUNTU_MAJOR="26"

usage() {
  cat <<'EOF'
Install Alcedo on Ubuntu 26.04.

Usage: ./scripts/install-ubuntu-26.sh [options]

Options:
  --prefix DIR     Install prefix (default: $HOME/.local)
  --deps-only      Install apt packages and exit
  --skip-deps      Skip apt; only build and install
  --debug          Build without --release
  --force          Continue on a non-Ubuntu-26 host
  -h, --help       Show this help

Environment:
  PREFIX           Same as --prefix
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

run_root() {
  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
  else
    sudo "$@"
  fi
}

install_file() {
  local mode="$1" src="$2" dest="$3"
  local dest_dir
  dest_dir="$(dirname "$dest")"
  if mkdir -p "$dest_dir" 2>/dev/null && [[ -w "$dest_dir" ]]; then
    install -Dm"$mode" "$src" "$dest"
  else
    run_root install -Dm"$mode" "$src" "$dest"
  fi
}

version_ge() {
  local have="$1" need="$2"
  [[ "$(printf '%s\n%s\n' "$need" "$have" | sort -V | head -n1)" == "$need" ]]
}

rustc_version() {
  rustc --version 2>/dev/null | awk '{print $2}' | cut -d- -f1
}

check_os() {
  local id="" version_id=""
  if [[ -f /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    id="${ID:-}"
    version_id="${VERSION_ID:-}"
  fi

  local major="${version_id%%.*}"
  if [[ "$id" == "ubuntu" && "$major" == "$UBUNTU_MAJOR" ]]; then
    return 0
  fi

  echo "This script targets Ubuntu ${UBUNTU_MAJOR}.04 (found ${id:-unknown} ${version_id:-unknown})." >&2
  if [[ "$FORCE_OS" -eq 1 ]]; then
    echo "Continuing because --force was set." >&2
    return 0
  fi
  die "use --force to install anyway"
}

install_deps() {
  need_cmd apt-get

  local packages=(
    build-essential
    clang
    libclang-dev
    pkg-config
    cmake
    nasm
    rustc
    cargo
    libgtk-4-dev
    libadwaita-1-dev
    libwebkitgtk-6.0-dev
    libspelling-1-dev
    libenchant-2-dev
    libgtksourceview-5-dev
    libglib2.0-dev
    librsvg2-dev
    nettle-dev
    desktop-file-utils
  )

  echo "Installing build dependencies…"
  run_root apt-get update
  run_root apt-get install -y "${packages[@]}"
}

ensure_rust() {
  if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
  fi

  if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
    die "rustc/cargo not found; re-run without --skip-deps or install rustc and cargo"
  fi

  local have
  have="$(rustc_version)"
  [[ -n "$have" ]] || die "could not parse rustc version"
  version_ge "$have" "$MSRV" || die "rustc $have is too old (need $MSRV or newer; Ubuntu 26.04 ships 1.93.1)"
  echo "Using rustc $have"
}

ensure_c_include_path() {
  if [[ -f /usr/include/stddef.h ]]; then
    return
  fi
  if ! command -v gcc >/dev/null 2>&1; then
    return
  fi
  local gcc_include
  gcc_include="$(gcc -print-file-name=include)"
  if [[ -n "$gcc_include" && -f "$gcc_include/stddef.h" ]]; then
    export C_INCLUDE_PATH="${gcc_include}${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}"
    echo "Using C_INCLUDE_PATH=$C_INCLUDE_PATH (nettle-sys bindgen)"
  fi
}

build_alcedo() {
  ensure_c_include_path
  echo "Building alcedo (${BUILD_FLAGS[*]:-debug})…"
  cargo build -p alcedo "${BUILD_FLAGS[@]}"
}

binary_path() {
  if [[ "${BUILD_FLAGS[*]-}" == *"--release"* ]]; then
    echo "$ROOT/target/release/alcedo"
  else
    echo "$ROOT/target/debug/alcedo"
  fi
}

compile_installed_schemas() {
  local schema_dir="$1"
  if command -v glib-compile-schemas >/dev/null 2>&1; then
    if [[ -w "$schema_dir" ]]; then
      glib-compile-schemas "$schema_dir"
    else
      run_root glib-compile-schemas "$schema_dir"
    fi
  fi
}

refresh_desktop() {
  local apps="$PREFIX/share/applications"
  local icons="$PREFIX/share/icons/hicolor"
  if command -v update-desktop-database >/dev/null 2>&1 && [[ -d "$apps" ]]; then
    update-desktop-database "$apps" 2>/dev/null || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1 && [[ -d "$icons" ]]; then
    gtk-update-icon-cache -f -t "$icons" 2>/dev/null || true
  fi
}

install_alcedo() {
  local bin
  bin="$(binary_path)"
  [[ -x "$bin" ]] || die "build did not produce $bin"

  echo "Installing to $PREFIX…"
  install_file 755 "$bin" "$PREFIX/bin/alcedo"
  install_file 644 "$ROOT/data/dev.alcedo.Mail.desktop" \
    "$PREFIX/share/applications/dev.alcedo.Mail.desktop"
  local metainfo
  metainfo="$(mktemp)"
  "$ROOT/scripts/render-metainfo.sh" "$metainfo"
  install_file 644 "$metainfo" "$PREFIX/share/metainfo/dev.alcedo.Mail.metainfo.xml"
  rm -f "$metainfo"
  install_file 644 "$ROOT/data/dev.alcedo.Mail.gschema.xml" \
    "$PREFIX/share/glib-2.0/schemas/dev.alcedo.Mail.gschema.xml"
  install_file 644 "$ROOT/data/icons/hicolor/scalable/apps/dev.alcedo.Mail.svg" \
    "$PREFIX/share/icons/hicolor/scalable/apps/dev.alcedo.Mail.svg"

  compile_installed_schemas "$PREFIX/share/glib-2.0/schemas"
  if command -v desktop-file-validate >/dev/null 2>&1; then
    desktop-file-validate "$PREFIX/share/applications/dev.alcedo.Mail.desktop" || true
  fi
  refresh_desktop
}

print_done() {
  local config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
  cat <<EOF

Alcedo installed:

  $PREFIX/bin/alcedo

Make sure $PREFIX/bin is on your PATH, then launch from GNOME or:

  alcedo

Gmail needs no config: use an app password with Provider set to Other email.
Google/Microsoft OAuth (advanced, optional): copy oauth.toml.example to
  $config_home/alcedo/oauth.toml
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)
      [[ $# -ge 2 ]] || die "--prefix requires a directory"
      PREFIX="$2"
      shift 2
      ;;
    --deps-only)
      BUILD=0
      shift
      ;;
    --skip-deps)
      INSTALL_DEPS=0
      shift
      ;;
    --debug)
      BUILD_FLAGS=()
      shift
      ;;
    --force)
      FORCE_OS=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
done

check_os

if [[ "$INSTALL_DEPS" -eq 1 ]]; then
  install_deps
fi

if [[ "$BUILD" -eq 0 ]]; then
  echo "Dependencies installed."
  exit 0
fi

ensure_rust
build_alcedo
install_alcedo
print_done
