#!/usr/bin/env bash
# Rebuild the OpenPGP shared library from this source and swap it in under an
# unchanged alcedo-pgp-shared test binary.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'USAGE'
Check that a rebuilt OpenPGP library works under an unchanged client.

Usage: ./scripts/pgp-relink-test.sh [-h|--help]

Steps: check the pgp-shared app build, then build the alcedo-pgp-shared test binary once, run it against the normal
library, build a second library with a different build id, run the same binary
again with ALCEDO_PGP_LIB pointing at the new library, and fail if the test
binary changed in between.

Environment:
  RELINK_TARGET_DIR     Cargo target dir for the rebuilt library (default: a
                        temporary directory, removed on exit)
  SEQUOIA_OPENPGP_SRC   Directory with a modified Sequoia (sequoia-openpgp)
                        source tree; when set, it is compiled into the rebuilt
                        library through a cargo patch.crates-io override
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

case "${1:-}" in
  -h | --help) usage; exit 0 ;;
  "") ;;
  *) usage >&2; die "unknown argument: $1" ;;
esac

# nettle-sys bindgen needs the compiler's own headers (same as scripts/run.sh).
if [[ -z "${C_INCLUDE_PATH:-}" ]] && command -v gcc >/dev/null 2>&1; then
  gcc_include="$(gcc -print-file-name=include)"
  if [[ -d "$gcc_include" ]]; then
    export C_INCLUDE_PATH="$gcc_include"
  fi
fi

# A user's own ALCEDO_PGP_LIB must not decide which library step 3 loads.
unset ALCEDO_PGP_LIB ALCEDO_PGP_EXPECT_BUILD_ID

# One EXIT trap covers every temp path; the target dir is removed only when
# this script created it.
build_log="$(mktemp)"
created_target_dir=""
cleanup() {
  rm -f "$build_log"
  if [[ -n "$created_target_dir" ]]; then
    rm -rf "$created_target_dir"
  fi
}
trap cleanup EXIT

if [[ -n "${RELINK_TARGET_DIR:-}" ]]; then
  TARGET_DIR="$RELINK_TARGET_DIR"
else
  TARGET_DIR="$(mktemp -d)"
  created_target_dir="$TARGET_DIR"
fi

echo "==> 1/6 check the pgp-shared app build links no Sequoia or Nettle"
cargo check -p alcedo --no-default-features --features pgp-shared
shared_deps="$(cargo tree -p alcedo --no-default-features --features pgp-shared -e normal --prefix none --format '{p}' | awk '{print $1}')"
if grep -qxE 'sequoia-openpgp|sequoia-autocrypt|buffered-reader|nettle|nettle-sys' <<<"$shared_deps"; then
  die "the pgp-shared build of alcedo still depends on Sequoia or Nettle"
fi
grep -qx 'alcedo-pgp-shared' <<<"$shared_deps" || die "the pgp-shared build of alcedo does not depend on alcedo-pgp-shared"

echo "==> 2/6 build the client test binary"
cargo test -p alcedo-pgp-shared --no-run 2>&1 | tee "$build_log"
client_bin="$(sed -n 's/^ *Executable tests\/parity\.rs (\(.*\))$/\1/p' "$build_log" | tail -n 1)"
[[ -n "$client_bin" && -f "$client_bin" ]] || die "could not find the parity test binary"
client_bin="$(readlink -f "$client_bin")"
stamp_before="$(stat -c '%n %Y.%y %s' "$client_bin")"

echo "==> 3/6 run parity tests against the normal library"
cargo test -p alcedo-pgp-shared

echo "==> 4/6 build a modified library from the same source"
build_id="relink-$(date -u +%Y%m%dT%H%M%SZ)"
build_cmd=(cargo build -p alcedo-pgp-ffi --release --target-dir "$TARGET_DIR")
if [[ -n "${SEQUOIA_OPENPGP_SRC:-}" ]]; then
  [[ -d "$SEQUOIA_OPENPGP_SRC" ]] || die "SEQUOIA_OPENPGP_SRC is not a directory: $SEQUOIA_OPENPGP_SRC"
  build_cmd+=(--config "patch.crates-io.sequoia-openpgp.path=\"$SEQUOIA_OPENPGP_SRC\"")
fi
ALCEDO_PGP_BUILD_ID="$build_id" "${build_cmd[@]}"
new_lib="$TARGET_DIR/release/libalcedo_pgp_ffi.so"
[[ -f "$new_lib" ]] || die "rebuilt library not found: $new_lib"

echo "==> 5/6 run the same client tests with the rebuilt library"
ALCEDO_PGP_LIB="$new_lib" ALCEDO_PGP_EXPECT_BUILD_ID="$build_id" \
  cargo test -p alcedo-pgp-shared

echo "==> 6/6 check the client was not relinked"
stamp_after="$(stat -c '%n %Y.%y %s' "$client_bin")"
if [[ "$stamp_before" != "$stamp_after" ]]; then
  die "client test binary changed: before '$stamp_before', after '$stamp_after'"
fi

echo
echo "PASS: rebuilt OpenPGP library works under an unchanged client"
echo "  normal library:  found next to the test binary, build id unmodified"
echo "  rebuilt library: $new_lib, build id $build_id"
echo "  client binary:   $client_bin (path and mtime unchanged, not relinked)"
