#!/usr/bin/env python3
"""Compare the SQLite and MySQL schemas table by table.

This project ships two hand-maintained migration trees. Nothing generates one
from the other, so the only thing keeping them in step is a check that actually
applies both and compares the result. Drift is otherwise invisible: a column
added to `migrations/` and forgotten in `migrations-mysql/` compiles, passes the
SQLite test suite, and only fails on the first MySQL deployment that touches
that column.

The SQLite side is built by running the real migration files into a temporary
database, not by parsing them -- the `*_new` + RENAME + DROP pattern used by
several migrations means a grep of CREATE TABLE would report a schema that does
not exist.

The MySQL side needs a server. Pass its connection details; CI provides a
service container. Locally:

    python3 scripts/check-schema-parity.py \\
        --mysql-host 127.0.0.1 --mysql-user root --mysql-password secret

Exit status is 0 when the two agree and 1 when they do not (or when the MySQL
side could not be reached at all, which is a failure of the check, not a pass).
"""

from __future__ import annotations

import argparse
import glob
import os
import sqlite3
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Differences that are known and deliberate, as `table: {columns}`.
# Everything listed here is reported on stdout but does not fail the check, so
# an intentional asymmetry has to be written down rather than silently tolerated.
KNOWN_DIFFERENCES: dict[str, set[str]] = {}


def sqlite_schema() -> dict[str, set[str]]:
    """Apply every SQLite migration in filename order and read the result."""
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "parity.db")
        connection = sqlite3.connect(path)
        try:
            for filename in sorted(glob.glob(os.path.join(ROOT, "migrations", "*.sql"))):
                with open(filename, encoding="utf-8") as handle:
                    connection.executescript(handle.read())
            tables = [
                row[0]
                for row in connection.execute(
                    "SELECT name FROM sqlite_master WHERE type='table' "
                    "AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx%'"
                )
            ]
            return {
                table: {
                    row[1]
                    for row in connection.execute(f"PRAGMA table_info('{table}')")
                }
                for table in tables
            }
        finally:
            connection.close()


def mysql_cli(args: list[str], *, stdin: str | None = None) -> str:
    """Run the mysql client, returning stdout.

    The CLI rather than a driver: the GitHub runner already has it, and pulling a
    Python MySQL package into a check that CI runs once is a dependency to keep
    current for no benefit.
    """
    result = subprocess.run(
        ["mysql", *args],
        input=stdin,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "mysql client failed")
    return result.stdout


def mysql_schema(options: argparse.Namespace) -> dict[str, set[str]]:
    base = [
        f"--host={options.mysql_host}",
        f"--port={options.mysql_port}",
        f"--user={options.mysql_user}",
        "--protocol=TCP",
    ]
    if options.mysql_password:
        base.append(f"--password={options.mysql_password}")
    database = options.mysql_database

    # A fresh schema every run: leftovers from an earlier attempt would hide a
    # missing CREATE TABLE or add a column the migrations no longer produce.
    mysql_cli(base + ["--execute", f"DROP DATABASE IF EXISTS `{database}`"])
    mysql_cli(
        base
        + [
            "--execute",
            f"CREATE DATABASE `{database}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci",
        ]
    )
    for filename in sorted(glob.glob(os.path.join(ROOT, "migrations-mysql", "*.sql"))):
        with open(filename, encoding="utf-8") as handle:
            mysql_cli(base + ["--default-character-set=utf8mb4", database], stdin=handle.read())

    rows = mysql_cli(
        base
        + [
            "--batch",
            "--skip-column-names",
            "--execute",
            "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.COLUMNS "
            f"WHERE TABLE_SCHEMA='{database}'",
        ]
    )
    schema: dict[str, set[str]] = {}
    for line in rows.splitlines():
        if not line.strip():
            continue
        table, column = line.split("\t", 1)
        schema.setdefault(table, set()).add(column)
    return schema


def describe_differences(
    sqlite: dict[str, set[str]], mysql: dict[str, set[str]]
) -> list[str]:
    problems: list[str] = []
    for table in sorted(set(sqlite) - set(mysql)):
        problems.append(f"table only in SQLite migrations: {table}")
    for table in sorted(set(mysql) - set(sqlite)):
        problems.append(f"table only in MySQL migrations: {table}")
    for table in sorted(set(sqlite) & set(mysql)):
        allowed = KNOWN_DIFFERENCES.get(table, set())
        for column in sorted(sqlite[table] - mysql[table] - allowed):
            problems.append(f"column only in SQLite: {table}.{column}")
        for column in sorted(mysql[table] - sqlite[table] - allowed):
            problems.append(f"column only in MySQL: {table}.{column}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mysql-host", default=os.environ.get("MYSQL_HOST", "127.0.0.1"))
    parser.add_argument("--mysql-port", default=os.environ.get("MYSQL_PORT", "3306"))
    parser.add_argument("--mysql-user", default=os.environ.get("MYSQL_USER", "root"))
    parser.add_argument(
        "--mysql-password", default=os.environ.get("MYSQL_PASSWORD", "")
    )
    # A database of this check's own: it is dropped and recreated, so it must
    # never be a name a deployment would use.
    parser.add_argument(
        "--mysql-database", default=os.environ.get("MYSQL_SCHEMA_PARITY_DB", "qdrust_parity")
    )
    options = parser.parse_args()

    sqlite = sqlite_schema()
    try:
        mysql = mysql_schema(options)
    except (RuntimeError, FileNotFoundError) as error:
        print(f"error: cannot read the MySQL schema: {error}", file=sys.stderr)
        print("error: pass --mysql-host/--mysql-user/--mysql-password", file=sys.stderr)
        return 1

    problems = describe_differences(sqlite, mysql)
    if problems:
        print(
            f"schema drift: {len(problems)} difference(s) between "
            "migrations/ and migrations-mysql/",
            file=sys.stderr,
        )
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        print(
            "\nAdd the missing statement to the other tree, or record the "
            "difference in KNOWN_DIFFERENCES if it is deliberate.",
            file=sys.stderr,
        )
        return 1

    print(
        f"ok: {len(sqlite)} tables, "
        f"{sum(len(columns) for columns in sqlite.values())} columns agree"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
