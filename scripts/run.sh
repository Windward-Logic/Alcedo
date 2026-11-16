#!/usr/bin/env bash
# Launch Alcedo with real sync (no ALCEDO_MOCK).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -z "${C_INCLUDE_PATH:-}" ]] && command -v gcc >/dev/null 2>&1; then
  gcc_include="$(gcc -print-file-name=include)"
  if [[ -n "$gcc_include" && -f "$gcc_include/stddef.h" ]]; then
    export C_INCLUDE_PATH="$gcc_include${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}"
  fi
fi

unset ALCEDO_MOCK

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
OAUTH_TOML="$CONFIG_HOME/alcedo/oauth.toml"
if [[ ! -f "$OAUTH_TOML" ]]; then
  echo "Note: no OAuth config at $OAUTH_TOML" >&2
  echo "      Copy oauth.toml.example there for Google/Microsoft sign-in." >&2
fi

export RUST_LOG="${RUST_LOG:-alcedo=info}"

exec cargo run -p alcedo -- "$@"
