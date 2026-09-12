#!/usr/bin/env python3
"""Bump the qdrust release version in every place that carries it.

Usage:
    python scripts/bump-version.py 0.1.11    # rewrite every file + sync lockfiles
    python scripts/bump-version.py --check   # verify everything agrees (CI gate)

Single source of truth: the `version` key under [workspace.package] in the
root Cargo.toml. Everything else is derived from it:

    Cargo.toml               [workspace.package] version    <- the one to edit
                             + the two internal entries in
                               [workspace.dependencies] (they must carry the
                               same version: deny.toml denies wildcard reqs
                               and cargo-deny cannot resolve versionless
                               workspace dependencies)
    Cargo.lock               synced via `cargo update --workspace`
    webui/package.json       "version"
    webui/package-lock.json  root package "version" (two spots)
    docs/openapi-v1.json     info.version

The four workspace crates inherit the version through
`version.workspace = true` in their [package] tables, so they carry no
version literals of their own.

Bumps are transactional: every replacement is computed and verified in
memory first; only when all of them succeed are the files written.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CARGO_TOML = ROOT / "Cargo.toml"
PACKAGE_JSON = ROOT / "webui" / "package.json"
PACKAGE_LOCK = ROOT / "webui" / "package-lock.json"
OPENAPI_JSON = ROOT / "docs" / "openapi-v1.json"

SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")


INTERNAL_DEPS = ("qdrust-core", "qdrust-plugin-browser")


def workspace_section(text: str, header: str) -> str:
    match = re.search(
        rf"^\[{re.escape(header)}\]\s*$(.*?)(?=^\[|\Z)", text, re.S | re.M
    )
    if not match:
        sys.exit(f"error: [{header}] section not found in Cargo.toml")
    return match.group(1)


def workspace_version() -> str:
    """Read the version from [workspace.package] in the root Cargo.toml."""
    match = re.search(r'^version\s*=\s*"([^"]+)"', workspace_section(CARGO_TOML.read_text(encoding="utf-8"), "workspace.package"), re.M)
    if not match:
        sys.exit("error: version key not found under [workspace.package]")
    return match.group(1)


def workspace_dep_versions() -> dict[str, str]:
    """Versions of the internal crates in [workspace.dependencies]."""
    body = workspace_section(CARGO_TOML.read_text(encoding="utf-8"), "workspace.dependencies")
    out: dict[str, str] = {}
    for name in INTERNAL_DEPS:
        match = re.search(rf'^{re.escape(name)}\s*=.*?version\s*=\s*"([^"]+)"', body, re.M)
        if not match:
            sys.exit(f"error: {name} in [workspace.dependencies] carries no version")
        out[name] = match.group(1)
    return out


def plan_cargo_toml(old: str, new: str) -> str:
    text = CARGO_TOML.read_text(encoding="utf-8")

    def swap_package(match: re.Match[str]) -> str:
        return re.sub(r'^(version\s*=\s*)"[^"]+"', rf'\g<1>"{new}"', match.group(0), count=1, flags=re.M)

    def swap_deps(match: re.Match[str]) -> str:
        body = match.group(0)
        for name in INTERNAL_DEPS:
            body, count = re.subn(
                rf'^({re.escape(name)}\s*=.*?version\s*=\s*)"[^"]+"',
                rf'\g<1>"{new}"',
                body,
                count=1,
                flags=re.M,
            )
            if count != 1:
                sys.exit(f"error: failed to plan a version rewrite for {name}")
        return body

    updated = re.sub(
        r"^\[workspace\.package\]\s*$(.*?)(?=^\[|\Z)", swap_package, text, count=1, flags=re.S | re.M
    )
    updated = re.sub(
        r"^\[workspace\.dependencies\]\s*$(.*?)(?=^\[|\Z)", swap_deps, updated, count=1, flags=re.S | re.M
    )
    if updated == text:
        sys.exit("error: failed to plan any [workspace.package] version rewrite")
    return updated


def webui_versions() -> dict[str, str]:
    """All version spots inside webui/package.json + package-lock.json."""
    pkg = json.loads(PACKAGE_JSON.read_text(encoding="utf-8"))
    lock = json.loads(PACKAGE_LOCK.read_text(encoding="utf-8"))
    packages = lock.get("packages") or {}
    return {
        "package.json": pkg.get("version", ""),
        "package-lock.json (root)": packages.get("", {}).get("version", ""),
        "package-lock.json (top)": lock.get("version", ""),
    }


def plan_package_json(old: str, new: str) -> str:
    pkg = json.loads(PACKAGE_JSON.read_text(encoding="utf-8"))
    if pkg.get("version") != old:
        sys.exit(f"error: webui/package.json is {pkg.get('version')!r}, expected {old!r}")
    pkg["version"] = new
    return json.dumps(pkg, indent=2) + "\n"


def plan_package_lock(old: str, new: str) -> str:
    """Rewrite only the two root-package spots; the dependency tree is untouched.

    npm lockfile v3 layout:
        line 3:      "version": "x.y.z",   <- 2-space indent, top level
        packages."": "version": "x.y.z",   <- 6-space indent
    """
    text = PACKAGE_LOCK.read_text(encoding="utf-8")
    old_q = re.escape(f'"{old}"')
    top = re.compile(rf'^(\s{{2}}"version": ){old_q}(,?)$', re.M)
    nested = re.compile(rf'^(\s{{6}}"version": ){old_q}(,?)$', re.M)
    text, n_top = top.subn(rf'\g<1>"{new}"\g<2>', text, count=1)
    text, n_nested = nested.subn(rf'\g<1>"{new}"\g<2>', text, count=1)
    if (n_top, n_nested) != (1, 1):
        sys.exit(
            "error: unexpected package-lock.json layout "
            f"(top-level matches: {n_top}, nested matches: {n_nested}); "
            "run `npm --prefix webui install --package-lock-only` and retry"
        )
    return text


def openapi_version() -> str:
    text = OPENAPI_JSON.read_text(encoding="utf-8")
    match = re.search(r'"info"\s*:\s*\{[^{}]*?"version"\s*:\s*"([^"]+)"', text, re.S)
    return match.group(1) if match else ""


def plan_openapi(old: str, new: str) -> str:
    text = OPENAPI_JSON.read_text(encoding="utf-8")
    pattern = re.compile(r'("info"\s*:\s*\{[^{}]*?"version"\s*:\s*")[^"]+(")', re.S)
    updated, count = pattern.subn(rf"\g<1>{new}\g<2>", text, count=1)
    if count != 1:
        sys.exit("error: info.version not found in docs/openapi-v1.json")
    return updated


def sync_cargo_lock() -> None:
    result = subprocess.run(
        ["cargo", "update", "--workspace"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        sys.exit(f"error: cargo update --workspace failed:\n{result.stderr.strip()}")


def main() -> None:
    argv = sys.argv[1:]
    if argv in (["--check"], []):
        current = workspace_version()
        versions = {
            "Cargo.toml [workspace.package]": current,
            **{f"Cargo.toml [workspace.dependencies] {k}": v for k, v in workspace_dep_versions().items()},
            **{f"webui/{k}": v for k, v in webui_versions().items()},
            "docs/openapi-v1.json info": openapi_version(),
        }
        print("Release versions:")
        for label, value in versions.items():
            print(f"  {label:<32} {value}")
        drift = {k: v for k, v in versions.items() if v != current}
        if drift:
            print(f"\nDRIFT detected against {current}: {drift}")
            sys.exit(1)
        print("all agree")
        return

    if len(argv) != 1:
        sys.exit("usage: python scripts/bump-version.py <x.y.z> | --check")
    target = argv[0]
    if not SEMVER_RE.match(target):
        sys.exit(f"error: {target!r} is not a valid semver version")

    current = workspace_version()
    if target == current:
        sys.exit(f"error: version is already {current}")

    # Transactional: plan every rewrite in memory; write only when all succeed.
    new_cargo = plan_cargo_toml(current, target)
    new_pkg = plan_package_json(current, target)
    new_lock = plan_package_lock(current, target)
    new_openapi = plan_openapi(openapi_version(), target)

    CARGO_TOML.write_text(new_cargo, encoding="utf-8")
    PACKAGE_JSON.write_text(new_pkg, encoding="utf-8")
    PACKAGE_LOCK.write_text(new_lock, encoding="utf-8")
    OPENAPI_JSON.write_text(new_openapi, encoding="utf-8")
    sync_cargo_lock()

    print(f"Bumped {current} -> {target}:")
    for label in (
        "Cargo.toml [workspace.package]",
        "Cargo.toml [workspace.dependencies] (2 internal deps)",
        "webui/package.json",
        "webui/package-lock.json (root + top)",
        "docs/openapi-v1.json info",
    ):
        print(f"  {label:<32} {target}")
    print("  Cargo.lock                       synced (cargo update --workspace)")
    print("\nNext: review `git diff`, commit, then tag vX.Y.Z to trigger the release workflow.")


if __name__ == "__main__":
    main()
