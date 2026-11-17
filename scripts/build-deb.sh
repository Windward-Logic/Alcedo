#!/usr/bin/env bash
# Build the free `alcedo` .deb from a clone of the public repository.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

PUBLIC_REMOTE="https://github.com/Windward-Logic/Alcedo"
REVISION=1
ALLOW_DIRTY=0

usage() {
  cat <<'EOF2'
Build dist/alcedo_<version>-<rev>_<arch>.deb and its .sha256 file.

Usage: ./scripts/build-deb.sh [options]

Options:
  --revision N     Debian revision (default: 1)
  --allow-dirty    Build even with uncommitted changes
  -h, --help       Show this help

Environment:
  ALCEDO_DEB_TARGET_DIR   Cargo target dir (default: target/deb in this checkout)
EOF2
}

die() {
  echo "error: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

workspace_version() {
  awk '
    /^\[/ { in_pkg = ($0 == "[workspace.package]"); next }
    in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*"/, ""); sub(/"[[:space:]]*(#.*)?$/, ""); print; exit
    }
  ' "$ROOT/Cargo.toml"
}

ensure_c_include_path() {
  if [[ -f /usr/include/stddef.h ]] || ! command -v gcc >/dev/null 2>&1; then
    return
  fi
  local gcc_include
  gcc_include="$(gcc -print-file-name=include)"
  if [[ -n "$gcc_include" && -f "$gcc_include/stddef.h" ]]; then
    export C_INCLUDE_PATH="${gcc_include}${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}"
  fi
}

shlibs_depends() {
  local binary="$1" tmp
  tmp="$(mktemp -d)"
  mkdir "$tmp/debian"
  : >"$tmp/debian/control"
  (cd "$tmp" && dpkg-shlibdeps -O "$binary") |
    sed -n 's/^shlibs:Depends=//p'
  rm -rf "$tmp"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --revision)
      [[ $# -ge 2 ]] || die "--revision requires a number"
      REVISION="$2"
      [[ "$REVISION" =~ ^[0-9]+$ ]] || die "revision must be a number: $REVISION"
      shift 2
      ;;
    --allow-dirty)
      ALLOW_DIRTY=1
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

for cmd in git cargo rustc dpkg dpkg-deb dpkg-shlibdeps strip gzip python3 sha256sum; do
  need_cmd "$cmd"
done

origin="$(git remote get-url origin 2>/dev/null || true)"
[[ "${origin%.git}" == "$PUBLIC_REMOTE" ]] ||
  die "origin must be $PUBLIC_REMOTE (found '${origin:-none}'); build from a clone of the public repository"

if [[ "$ALLOW_DIRTY" -eq 0 && -n "$(git status --porcelain)" ]]; then
  die "working tree is dirty (commit, or pass --allow-dirty)"
fi

VERSION="$(workspace_version)"
[[ -n "$VERSION" ]] || die "no version in [workspace.package] of Cargo.toml"
ARCH="$(dpkg --print-architecture)"
NAME="alcedo_${VERSION}-${REVISION}_${ARCH}"

SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)"
export SOURCE_DATE_EPOCH
export CARGO_TARGET_DIR="${ALCEDO_DEB_TARGET_DIR:-$ROOT/target/deb}"
mkdir -p "$CARGO_TARGET_DIR"
[[ -O "$CARGO_TARGET_DIR" ]] || die "$CARGO_TARGET_DIR is not owned by $(id -un)"
CARGO_TARGET_DIR="$(cd "$CARGO_TARGET_DIR" && pwd -P)"
# rustc applies the last matching prefix; the target dir sits inside $ROOT by default.
export RUSTFLAGS="--remap-path-prefix=$ROOT=/usr/src/alcedo-$VERSION --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo --remap-path-prefix=$CARGO_TARGET_DIR=/target"
ensure_c_include_path

echo "Building alcedo $VERSION (release, locked)…"
cargo build --release --locked -p alcedo

STAGE="$CARGO_TARGET_DIR/deb/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN"

install -Dm755 "$CARGO_TARGET_DIR/release/alcedo" "$STAGE/usr/bin/alcedo"
strip --strip-unneeded "$STAGE/usr/bin/alcedo"
install -Dm644 "$ROOT/data/dev.alcedo.Mail.desktop" \
  "$STAGE/usr/share/applications/dev.alcedo.Mail.desktop"
install -Dm644 "$ROOT/data/dev.alcedo.Mail.gschema.xml" \
  "$STAGE/usr/share/glib-2.0/schemas/dev.alcedo.Mail.gschema.xml"
install -Dm644 "$ROOT/data/icons/hicolor/scalable/apps/dev.alcedo.Mail.svg" \
  "$STAGE/usr/share/icons/hicolor/scalable/apps/dev.alcedo.Mail.svg"

metainfo="$(mktemp)"
ALCEDO_SCREENSHOT_REF="v$VERSION" "$ROOT/scripts/render-metainfo.sh" "$metainfo"
install -Dm644 "$metainfo" "$STAGE/usr/share/metainfo/dev.alcedo.Mail.metainfo.xml"
rm -f "$metainfo"

doc="$STAGE/usr/share/doc/alcedo"
install -Dm644 "$ROOT/packaging/deb/copyright" "$doc/copyright"
gzip -9n <"$ROOT/packaging/deb/changelog" >"$doc/changelog.Debian.gz"
licenses="$(mktemp)"
python3 -I "$ROOT/scripts/third-party-licenses.py" \
  --manifest-path "$ROOT/Cargo.toml" --package alcedo --output "$licenses"
gzip -9n <"$licenses" >"$doc/THIRD-PARTY-LICENSES.gz"
rm -f "$licenses"
chmod 644 "$doc/changelog.Debian.gz" "$doc/THIRD-PARTY-LICENSES.gz"
find "$STAGE" -type d -exec chmod 755 {} +

depends="$(shlibs_depends "$STAGE/usr/bin/alcedo")"
[[ -n "$depends" ]] || die "dpkg-shlibdeps produced no Depends"
installed_size="$(du -sk --apparent-size --exclude=DEBIAN "$STAGE" | cut -f1)"

sed \
  -e "s|@VERSION@|$VERSION-$REVISION|" \
  -e "s|@ARCH@|$ARCH|" \
  -e "s|@INSTALLED_SIZE@|$installed_size|" \
  -e "s|@DEPENDS@|$(printf '%s' "$depends" | sed 's/[\\|&]/\\&/g')|" \
  "$ROOT/packaging/deb/control.in" >"$STAGE/DEBIAN/control"
chmod 644 "$STAGE/DEBIAN/control"

find "$STAGE" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +

mkdir -p "$ROOT/dist"
dpkg-deb --root-owner-group -Zxz --build "$STAGE" "$ROOT/dist/$NAME.deb"
(cd "$ROOT/dist" && sha256sum "$NAME.deb" >"$NAME.deb.sha256")

echo "Built:"
echo "  $ROOT/dist/$NAME.deb"
echo "  $ROOT/dist/$NAME.deb.sha256"
