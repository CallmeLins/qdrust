#!/usr/bin/env python3
"""Gate: a migration that already shipped must never change.

`sqlx::migrate!` embeds every file in `migrations/` and `migrations-mysql/` at
compile time, and sqlx records a sha384 checksum of it in `_sqlx_migrations`.
On the next start the runner recomputes that checksum and refuses to boot when
the two disagree:

    Error: migration 202609060001 was previously applied but has been modified

Nothing relaxes that check at runtime, so a migration is frozen the moment a
release tag contains it: the only way to change the schema afterwards is a new
dated file. A comment-only edit breaks upgrades exactly as thoroughly as a
column change -- which is how 202609060001 broke, when a doc reference in its
header comment was updated in place as part of a docs reshuffle.

Usage:
    python scripts/check-migrations.py                 # compare against the newest v* tag
    python scripts/check-migrations.py --base v0.1.12  # compare against an explicit ref

The base is the newest release tag rather than every tag, because immutability
is transitive: a file untouched since v0.1.10 is byte-identical in v0.1.12, so
the tip covers the whole history. A migration that does not exist in the base is
new and always allowed, which is why only additions are a way forward.

Comparison is byte-exact, because sqlx hashes bytes: an EOL flip re-encodes the
file and breaks the checksum just like an edit does. `.gitattributes` pins
`*.sql` to `text eol=lf`, so a checkout on any platform materialises the same
bytes the runner will hash.

A line-endings-only difference is reported but does not fail: a runner that only
ever sees the blob cannot tell that apart from a real edit, and the pin above
keeps it from arising in CI. Real edits and deletions do fail.

Exit status is 1 when a released migration was modified or deleted, listing the
offending paths. When no release tag is reachable (shallow clone, or a branch
that predates the first release) the check is skipped with a notice and exit 0,
so it cannot fail for reasons unrelated to the change under review.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MIGRATION_DIRS = ("migrations", "migrations-mysql")
TAG_GLOB = "v[0-9]*"
EOL_ONLY = "line endings only"


def git(*args: str) -> bytes:
    result = subprocess.run(("git", *args), cwd=ROOT, capture_output=True)
    if result.returncode != 0:
        stderr = result.stderr.decode("utf-8", "replace").strip()
        sys.exit(f"error: git {' '.join(args)} failed: {stderr}")
    return result.stdout


def newest_release_tag() -> str | None:
    """The most recent `v*` tag reachable from HEAD, or None when there is none."""
    result = subprocess.run(
        ("git", "describe", "--tags", "--abbrev=0", "--match", TAG_GLOB),
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    tag = result.stdout.strip()
    return tag if result.returncode == 0 and tag else None


def released_migrations(base: str) -> list[str]:
    """The `.sql` paths the base revision ships, sorted.

    A migration directory that does not exist yet simply contributes nothing,
    so this stays correct for a base from before either directory was added.
    """
    listing = git("ls-tree", "-r", "--name-only", base, "--", *MIGRATION_DIRS).decode()
    return sorted(path for path in listing.splitlines() if path.endswith(".sql"))


def difference(path: str, shipped: bytes) -> str | None:
    """None when the working copy still matches the released bytes.

    Otherwise a short label, so the report can tell an EOL re-encoding apart
    from an actual edit.
    """
    local = ROOT / path
    if not local.is_file():
        return "deleted"
    current = local.read_bytes()
    if current == shipped:
        return None
    if current.replace(b"\r\n", b"\n") == shipped.replace(b"\r\n", b"\n"):
        return EOL_ONLY
    return "modified"


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Fail when a migration that already shipped has been edited."
    )
    parser.add_argument(
        "--base",
        help="release ref to compare against (default: the newest v* tag reachable from HEAD)",
    )
    args = parser.parse_args()

    base = args.base or newest_release_tag()
    if base is None:
        print("note: no v* release tag reachable from HEAD; skipping the check")
        return
    # Resolve the ref up front so a typo reports the ref, not each file.
    git("rev-parse", "--verify", f"{base}^{{commit}}")

    released = released_migrations(base)
    if not released:
        print(f"note: {base} ships no migrations; nothing to compare")
        return

    changes = [
        (path, kind)
        for path in released
        if (kind := difference(path, git("show", f"{base}:{path}")))
    ]
    edits = [(path, kind) for path, kind in changes if kind != EOL_ONLY]

    print(f"compared {len(released)} migration file(s) against {base}")
    # Reported for the developer's own benefit: the runner hashes bytes, so a
    # working copy that only re-encodes the file still breaks *this* machine
    # until it is checked out again. CI, which only sees the blob, cannot tell
    # this apart from an edit, so it is not a failure.
    for path, kind in changes:
        if kind == EOL_ONLY:
            print(f"  note: line endings only, re-checkout to realign   {path}")
    if not edits:
        print("no content changes" if changes else "all unchanged")
        return

    print()
    for path, kind in edits:
        print(f"  {kind:<28} {path}")
    print()
    sys.exit(
        f"error: {len(edits)} migration file(s) released in {base} were changed.\n"
        "       sqlx checksums these files in _sqlx_migrations, so every deployment\n"
        "       that already ran one now fails to start with\n"
        "         migration <version> was previously applied but has been modified\n"
        "       Restore the file to the released bytes and ship the change as a new\n"
        f"       dated migration instead (`git checkout {base} -- <path>`)."
    )


if __name__ == "__main__":
    main()
