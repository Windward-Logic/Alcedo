#!/usr/bin/env python3
"""Write the licence texts of the normal (non-dev, non-build) third-party
dependencies of a workspace package, for shipping in a binary package.

Usage: python3 -I scripts/third-party-licenses.py --manifest-path Cargo.toml \
           --package alcedo --output THIRD-PARTY-LICENSES
"""
import argparse
import fnmatch
import json
import os
import subprocess
import sys

LICENCE_PATTERNS = ("LICEN[CS]E*", "COPYING*", "NOTICE*")


def host_triple():
    out = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
    for line in out.splitlines():
        if line.startswith("host:"):
            return line.split(":", 1)[1].strip()
    sys.exit("third-party-licenses: no host triple in rustc -vV")


def cargo_metadata(manifest_path):
    cmd = [
        "cargo", "metadata", "--format-version", "1", "--locked",
        "--filter-platform", host_triple(), "--manifest-path", manifest_path,
    ]
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        sys.exit(f"third-party-licenses: cargo metadata failed ({result.returncode})")
    return json.loads(result.stdout)


def normal_closure(metadata, package_name):
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    root = next(p["id"] for p in metadata["packages"] if p["name"] == package_name)
    seen = {root}
    stack = [root]
    while stack:
        for dep in nodes[stack.pop()]["deps"]:
            if not any(k["kind"] is None for k in dep["dep_kinds"]):
                continue
            if dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                stack.append(dep["pkg"])
    return seen


def licence_files(package):
    root = os.path.dirname(package["manifest_path"])
    names = {
        n for n in os.listdir(root)
        if os.path.isfile(os.path.join(root, n))
        and any(fnmatch.fnmatchcase(n.upper(), pat) for pat in LICENCE_PATTERNS)
    }
    declared = package.get("license_file")
    if declared and os.path.isfile(os.path.join(root, declared)):
        names.add(declared)
    return [os.path.join(root, n) for n in sorted(names)]


def render(package):
    header = "{} {} — {} — {}".format(
        package["name"], package["version"],
        package.get("license") or "unknown", package.get("repository") or "no repository",
    )
    lines = [header, "=" * len(header), ""]
    files = licence_files(package)
    if not files:
        lines.append("(no licence file shipped in this crate)")
    for path in files:
        lines.append("--- " + os.path.basename(path) + " ---")
        with open(path, encoding="utf-8", errors="replace") as handle:
            lines.append(handle.read().rstrip("\n"))
        lines.append("")
    lines.append("")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest-path", required=True)
    parser.add_argument("--package", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    metadata = cargo_metadata(args.manifest_path)
    members = set(metadata["workspace_members"])
    wanted = normal_closure(metadata, args.package) - members
    packages = sorted(
        (p for p in metadata["packages"] if p["id"] in wanted),
        key=lambda p: (p["name"], p["version"]),
    )
    with open(args.output, "w", encoding="utf-8") as out:
        out.write("".join(render(p) + "\n" for p in packages))


if __name__ == "__main__":
    main()
