"""Behaviour tests for scripts/flatpak-cargo-sources.py.

Usage: python3 -m unittest discover -s scripts/tests
"""

import importlib.util
import json
import tomllib
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "flatpak-cargo-sources.py"
SPEC = importlib.util.spec_from_file_location("flatpak_cargo_sources", SCRIPT)
sources_tool = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sources_tool)

CHECKSUM = "a" * 64


def registry_package(name, version="1.0.0"):
    return {
        "name": name,
        "version": version,
        "source": sources_tool.CRATES_IO,
        "checksum": CHECKSUM,
    }


class FlatpakSourcesTest(unittest.TestCase):
    def test_pins_a_crate_download_to_its_lockfile_checksum(self):
        archive = sources_tool.flatpak_sources({"package": [registry_package("serde")]})[0]
        self.assertEqual(
            archive["url"], "https://static.crates.io/crates/serde/serde-1.0.0.crate"
        )
        self.assertEqual(archive["sha256"], CHECKSUM)
        self.assertEqual(archive["dest"], "cargo/vendor/serde-1.0.0")

    def test_marks_each_vendored_crate_with_its_checksum(self):
        marker = sources_tool.flatpak_sources({"package": [registry_package("serde")]})[1]
        self.assertEqual(marker["dest-filename"], ".cargo-checksum.json")
        self.assertEqual(json.loads(marker["contents"]), {"package": CHECKSUM, "files": {}})

    def test_keeps_two_versions_of_a_crate_apart(self):
        lock = {"package": [registry_package("rand", "0.8.5"), registry_package("rand", "0.9.0")]}
        destinations = {
            source["dest"]
            for source in sources_tool.flatpak_sources(lock)
            if source["type"] == "archive"
        }
        self.assertEqual(destinations, {"cargo/vendor/rand-0.8.5", "cargo/vendor/rand-0.9.0"})

    def test_skips_workspace_packages(self):
        lock = {"package": [{"name": "alcedo", "version": "1.0.0"}]}
        archives = [s for s in sources_tool.flatpak_sources(lock) if s["type"] == "archive"]
        self.assertEqual(archives, [])

    def test_points_cargo_at_the_vendored_crates(self):
        config = sources_tool.flatpak_sources({"package": []})[-1]
        self.assertEqual((config["dest"], config["dest-filename"]), ("cargo", "config.toml"))
        parsed = tomllib.loads(config["contents"])
        self.assertEqual(parsed["source"]["crates-io"]["replace-with"], "vendored-sources")
        self.assertEqual(parsed["source"]["vendored-sources"]["directory"], "cargo/vendor")

    def test_output_does_not_depend_on_lockfile_order(self):
        first, second = registry_package("a"), registry_package("b")
        self.assertEqual(
            sources_tool.render({"package": [first, second]}),
            sources_tool.render({"package": [second, first]}),
        )

    def test_rejects_a_git_dependency(self):
        package = registry_package("forked")
        package["source"] = "git+https://example.com/forked.git#abc"
        with self.assertRaises(sources_tool.UnsupportedSource):
            sources_tool.flatpak_sources({"package": [package]})

    def test_rejects_a_registry_package_without_a_checksum(self):
        package = registry_package("unsigned")
        del package["checksum"]
        with self.assertRaises(sources_tool.UnsupportedSource):
            sources_tool.flatpak_sources({"package": [package]})


if __name__ == "__main__":
    unittest.main()
