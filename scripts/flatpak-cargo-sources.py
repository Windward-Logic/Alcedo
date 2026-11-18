#!/usr/bin/env python3
"""Write the crate downloads a Flatpak build needs, pinned by Cargo.lock.

Flatpak builds have no network, so flatpak-builder fetches every crate first and
Cargo reads them from a vendor directory.

Usage: scripts/flatpak-cargo-sources.py [--check]
"""

import argparse
import json
import sys
import tomllib
from pathlib import Path

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
ROOT = Path(__file__).resolve().parent.parent
LOCK_FILE = ROOT / "Cargo.lock"
OUTPUT_FILE = ROOT / "build-aux" / "cargo-sources.json"
CARGO_CONFIG = """[source.vendored-sources]
directory = "cargo/vendor"

[source.crates-io]
replace-with = "vendored-sources"
"""


class UnsupportedSource(ValueError):
    """Cargo.lock names a package that cannot be vendored from crates.io."""


def crate_sources(package):
    name, version = package["name"], package["version"]
    checksum = package.get("checksum")
    if package["source"] != CRATES_IO or not checksum:
        raise UnsupportedSource(f"{name} {version} is not a checksummed crates.io package")
    dest = f"cargo/vendor/{name}-{version}"
    return [
        {
            "type": "archive",
            "archive-type": "tar-gzip",
            "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
            "sha256": checksum,
            "dest": dest,
        },
        {
            "type": "inline",
            "contents": json.dumps({"package": checksum, "files": {}}),
            "dest": dest,
            "dest-filename": ".cargo-checksum.json",
        },
    ]


def flatpak_sources(lock):
    """Sources for every registry package in a parsed Cargo.lock, in a stable order."""
    registry_packages = [package for package in lock["package"] if "source" in package]
    registry_packages.sort(key=lambda package: (package["name"], package["version"]))
    sources = [source for package in registry_packages for source in crate_sources(package)]
    sources.append(
        {
            "type": "inline",
            "contents": CARGO_CONFIG,
            "dest": "cargo",
            "dest-filename": "config.toml",
        }
    )
    return sources


def render(lock):
    return json.dumps(flatpak_sources(lock), indent=2) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if the output file is stale")
    args = parser.parse_args()

    rendered = render(tomllib.loads(LOCK_FILE.read_text(encoding="utf-8")))
    if args.check:
        if not OUTPUT_FILE.is_file() or OUTPUT_FILE.read_text(encoding="utf-8") != rendered:
            print(f"{OUTPUT_FILE} is out of date; run {Path(__file__).name}", file=sys.stderr)
            return 1
        return 0
    OUTPUT_FILE.write_text(rendered, encoding="utf-8")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except UnsupportedSource as err:
        sys.exit(f"flatpak-cargo-sources: {err}")
