#!/usr/bin/env bash
# Render data/dev.alcedo.Mail.metainfo.xml.in to OUTPUT, filling in values that
# live in the workspace Cargo.toml so they are configured in one place:
#   @REPOSITORY_URL@       <- [workspace.package] repository
#   @SCREENSHOT_BASE_URL@  <- raw GitHub URL for data/screenshots/ on the
#                             default branch (override with ALCEDO_SCREENSHOT_REF)
#
# Screenshot PNGs stay in git for AppStream/Flathub to fetch; they are not
# installed by scripts/install-ubuntu-26.sh or the Flatpak manifest.
#
# Usage: scripts/render-metainfo.sh OUTPUT
# Used by scripts/install-ubuntu-26.sh and build-aux/dev.alcedo.Mail.Devel.json.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATE="$ROOT/data/dev.alcedo.Mail.metainfo.xml.in"
CARGO_TOML="$ROOT/Cargo.toml"
# Flathub prefers a tag or commit. The default branch is main; releases pass
# ALCEDO_SCREENSHOT_REF=v<version> to pin screenshots to the release tag.
SCREENSHOT_REF="${ALCEDO_SCREENSHOT_REF:-main}"

die() {
  echo "render-metainfo: $*" >&2
  exit 1
}

xml_sed_escape() {
  local xml_value="${1//&/&amp;}"
  printf '%s' "$xml_value" | sed -e 's/[\\|&]/\\&/g'
}

[[ $# -eq 1 ]] || die "usage: $0 OUTPUT"
output="$1"

# Read `repository = "..."` from the [workspace.package] table only.
repository="$(
  awk '
    /^\[/ { in_pkg = ($0 == "[workspace.package]"); next }
    in_pkg && /^[[:space:]]*repository[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*"/, ""); sub(/"[[:space:]]*(#.*)?$/, ""); print; exit
    }
  ' "$CARGO_TOML"
)"

[[ -n "$repository" ]] || die "no repository in [workspace.package] of $CARGO_TOML"
[[ "$repository" =~ ^https://[^[:space:]\"\<\>]+$ ]] ||
  die "repository must be an https URL without spaces, quotes or angle brackets: $repository"

# https://github.com/OWNER/REPO[.git]/ -> https://raw.githubusercontent.com/OWNER/REPO/REF/data/screenshots
repo_url="${repository%/}"
repo_url="${repo_url%.git}"
if [[ ! "$repo_url" =~ ^https://github.com/([^/]+)/([^/]+)$ ]]; then
  die "repository must be a GitHub https URL (got $repository)"
fi
owner="${BASH_REMATCH[1]}"
repo="${BASH_REMATCH[2]}"
[[ "$SCREENSHOT_REF" =~ ^[A-Za-z0-9._/-]+$ ]] ||
  die "invalid ALCEDO_SCREENSHOT_REF: $SCREENSHOT_REF"
screenshot_base="https://raw.githubusercontent.com/${owner}/${repo}/${SCREENSHOT_REF}/data/screenshots"

sed \
  -e "s|@REPOSITORY_URL@|$(xml_sed_escape "$repository")|g" \
  -e "s|@SCREENSHOT_BASE_URL@|$(xml_sed_escape "$screenshot_base")|g" \
  "$TEMPLATE" >"$output"

if grep -q '@[A-Z_]*@' "$output"; then
  die "unfilled placeholder left in $output"
fi
