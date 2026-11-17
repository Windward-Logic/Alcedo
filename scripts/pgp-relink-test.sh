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

Steps: build the alcedo-pgp-shared test binary once, run it against the normal
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

if [[ -n "${RELINK_TARGET_DIR:-}" ]]; then
  TARGET_DIR="$RELINK_TARGET_DIR"
else
  TARGET_DIR="$(mktemp -d)"
  trap 'rm -rf "$TARGET_DIR"' EXIT
fi

echo "==> 1/5 build the client test binary"
build_log="$(mktemp)"
cargo test -p alcedo-pgp-shared --no-run 2>&1 | tee "$build_log"
client_bin="$(sed -n 's/^ *Executable tests\/parity\.rs (\(.*\))$/\1/p' "$build_log" | tail -n 1)"
rm -f "$build_log"
[[ -n "$client_bin" && -f "$client_bin" ]] || die "could not find the parity test binary"
client_bin="$(readlink -f "$client_bin")"
stamp_before="$(stat -c '%n %Y.%y %s' "$client_bin")"

echo "==> 2/5 run parity tests against the normal library"
cargo test -p alcedo-pgp-shared

echo "==> 3/5 build a modified library from the same source"
build_id="relink-$(date -u +%Y%m%dT%H%M%SZ)"
build_cmd=(cargo build -p alcedo-pgp-ffi --release --target-dir "$TARGET_DIR")
if [[ -n "${SEQUOIA_OPENPGP_SRC:-}" ]]; then
  [[ -d "$SEQUOIA_OPENPGP_SRC" ]] || die "SEQUOIA_OPENPGP_SRC is not a directory: $SEQUOIA_OPENPGP_SRC"
  build_cmd+=(--config "patch.crates-io.sequoia-openpgp.path=\"$SEQUOIA_OPENPGP_SRC\"")
fi
ALCEDO_PGP_BUILD_ID="$build_id" "${build_cmd[@]}"
new_lib="$TARGET_DIR/release/libalcedo_pgp_ffi.so"
[[ -f "$new_lib" ]] || die "rebuilt library not found: $new_lib"

echo "==> 4/5 run the same client tests with the rebuilt library"
ALCEDO_PGP_LIB="$new_lib" ALCEDO_PGP_EXPECT_BUILD_ID="$build_id" \
  cargo test -p alcedo-pgp-shared

echo "==> 5/5 check the client was not relinked"
stamp_after="$(stat -c '%n %Y.%y %s' "$client_bin")"
if [[ "$stamp_before" != "$stamp_after" ]]; then
  die "client test binary changed: before '$stamp_before', after '$stamp_after'"
fi

echo
echo "PASS: rebuilt OpenPGP library works under an unchanged client"
echo "  normal library:  found next to the test binary, build id unmodified"
echo "  rebuilt library: $new_lib, build id $build_id"
echo "  client binary:   $client_bin (path and mtime unchanged, not relinked)"
