#!/usr/bin/env python3
"""The performance results store: append-only JSON-lines files.

The store is the Forgejo generic package `tilo-perf-results`, read
anonymously and written only by t's perf-scheduled check (tools/ci/perf-compare.sh)
with the package token. Each package version is a UTC month (`2026-10`); each
scheduled run adds one immutable file, `<started>-<run>-<attempt>.jsonl`, so
runs never contend for a file and no record is ever rewritten. compare.py
reads the newest months it needs and writes the next file.

A local directory with the same `<month>/<file>.jsonl` layout is a store too,
which is what tests and local comparisons use:

    python3 tools/perf/store.py list https://code.tilo.so tilo
    python3 tools/perf/store.py fetch https://code.tilo.so tilo > history.jsonl

`fetch` prints measurements only. A gate test's records (`compare.py
--synthetic`, tagged `meta.synthetic_series`) hold values multiplied on
purpose, so `fetch` leaves them out and says how many it left out on stderr;
`--synthetic` prints them too. compare.py reads `history()`, which keeps them:
a synthetic series reads its own previous verdict.
"""

from __future__ import annotations

import argparse
import datetime
import json
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import record  # noqa: E402

PACKAGE = "tilo-perf-results"
MONTH = re.compile(r"\d{4}-\d{2}")
FILE = re.compile(r"[0-9TZ]{16}-[A-Za-z0-9_.-]+\.jsonl")
# Enough history for five weekly baselines; nightly KPIs need far less.
LOOKBACK_MONTHS = 3
TIMEOUT_SECONDS = 30


def months_back(now: datetime.datetime, count: int = LOOKBACK_MONTHS) -> list[str]:
    """The current UTC month and the `count - 1` before it, newest first."""
    year, month = now.year, now.month
    result = []
    for _ in range(count):
        result.append(f"{year:04d}-{month:02d}")
        year, month = (year - 1, 12) if month == 1 else (year, month - 1)
    return result


def file_name(started_at: str, run: str) -> str:
    """The store file name of one run: sortable by start time, unique per run."""
    stamp = started_at.replace("-", "").replace(":", "")
    name = f"{stamp}-{run}.jsonl"
    if not FILE.fullmatch(name):
        raise ValueError(f"store file name {name!r} must look like 20261009T011500Z-<run>.jsonl")
    return name


class LocalStore:
    def __init__(self, root: Path) -> None:
        self.root = root

    def files(self, months: list[str]) -> list[tuple[str, str]]:
        found = []
        for month in months:
            directory = self.root / month
            if directory.is_dir():
                found.extend((month, path.name) for path in directory.glob("*.jsonl"))
        return sorted(found)

    def read(self, month: str, name: str) -> str:
        return (self.root / month / name).read_text(encoding="utf-8")


class PackageStore:
    """A Forgejo generic package, read without credentials."""

    def __init__(self, server: str, owner: str, package: str = PACKAGE) -> None:
        self.server = server.rstrip("/")
        self.owner = owner
        self.package = package

    def _get(self, url: str) -> bytes | None:
        try:
            with urllib.request.urlopen(url, timeout=TIMEOUT_SECONDS) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            error.close()
            if error.code == 404:
                return None
            raise

    def version_url(self, month: str) -> str:
        quote = urllib.parse.quote
        return f"{self.server}/api/packages/{quote(self.owner)}/generic/{quote(self.package)}/{month}"

    def files(self, months: list[str]) -> list[tuple[str, str]]:
        quote = urllib.parse.quote
        found = []
        for month in months:
            body = self._get(f"{self.server}/api/v1/packages/{quote(self.owner)}/generic/"
                             f"{quote(self.package)}/{month}/files")
            if body is None:
                continue
            found.extend((month, item["name"]) for item in json.loads(body)
                         if FILE.fullmatch(item.get("name", "")))
        return sorted(found)

    def read(self, month: str, name: str) -> str:
        body = self._get(f"{self.version_url(month)}/{urllib.parse.quote(name)}")
        if body is None:
            raise ValueError(f"store file {month}/{name} was listed but cannot be read")
        return body.decode("utf-8")


def open_store(location: str, owner: str = "tilo") -> LocalStore | PackageStore:
    if location.startswith(("http://", "https://")):
        return PackageStore(location, owner)
    return LocalStore(Path(location))


def history(store: LocalStore | PackageStore, months: list[str]) -> list[record.Record]:
    """Every stored record of the given months, oldest file first."""
    records = []
    for month, name in store.files(months):
        records.extend(record.parse_lines(store.read(month, name), f"{month}/{name}"))
    return records


def synthetic(item: record.Record) -> bool:
    """A gate test's record (`compare.py --synthetic`), not a measurement."""
    return item.meta.get("synthetic_series") is not None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("list", "fetch"):
        command = commands.add_parser(name)
        command.add_argument("store", help="a forge URL (https://code.tilo.so) or a local directory")
        command.add_argument("owner", nargs="?", default="tilo")
        command.add_argument("--months", type=int, default=LOOKBACK_MONTHS)
        if name == "fetch":
            command.add_argument("--synthetic", action="store_true",
                                 help="also print gate-test records (meta.synthetic_series), whose values are multiplied")
    args = parser.parse_args(argv)
    store = open_store(args.store, args.owner)
    months = months_back(datetime.datetime.now(datetime.timezone.utc), args.months)
    if args.command == "list":
        for month, name in store.files(months):
            print(f"{month}/{name}")
    else:
        records = history(store, months)
        shown = [item for item in records if args.synthetic or not synthetic(item)]
        for item in shown:
            print(item.to_json())
        if len(shown) < len(records):
            print(f"store.py: omitted {len(records) - len(shown)} synthetic gate-test records "
                  "(meta.synthetic_series); --synthetic includes them", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
