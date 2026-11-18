# Helpers for scripts that build Alcedo .deb packages. Source this file; it only
# defines functions.

die() {
  echo "error: $*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing command: $1"
}

# Print the `version` from one section of a Cargo.toml, such as `package`.
cargo_section_version() {
  local section="$1" manifest="$2"
  awk -v section="[$section]" '
    /^\[/ { in_section = ($0 == section); next }
    in_section && /^[[:space:]]*version[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*"/, ""); sub(/"[[:space:]]*(#.*)?$/, ""); print; exit
    }
  ' "$manifest"
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

# Print the Debian shared-library dependencies of the binaries at these
# absolute paths.
shlibs_depends() {
  local tmp
  tmp="$(mktemp -d)"
  mkdir "$tmp/debian"
  : >"$tmp/debian/control"
  (cd "$tmp" && dpkg-shlibdeps -O "$@") |
    sed -n 's/^shlibs:Depends=//p'
  rm -rf "$tmp"
}
