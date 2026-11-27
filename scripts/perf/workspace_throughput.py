#!/usr/bin/env python3
"""How fast a running Tilo host takes records and files, as an agent sends them.

The in-process twin of this is the `a_workspace_takes_records_and_files_at_the_
same_speed_however_full` test of tilo-agent-session; this one drives a real
host over the network the way programs do: the Python SDK's actions and bulk
calls (`*_many`, `group` calls of up to 500), its uploads, and `tilo files put`
for a folder of files. Point it at a local stack:

    eval "$(os/containers/tilo-sh/stack.sh env)"
    uv run --project sdk/python scripts/perf/workspace_throughput.py --sizes 0,1000

For each size N (records of each kind already there) it times one action at a
time (p50, p99), a bulk call (records a second), and how long a change takes
to reach a second client watching the workspace. Then a folder of small files
through `tilo files put` (files a second) and one large upload (MB/s).
Prints a table, and writes it as JSON with --out.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable

from tilo import Tilo, rows, run


def issue(index: int) -> dict[str, Any]:
    return {"title": f"Issue {index}", "body": f"What issue {index} is about, in a line or two."}


def note(index: int) -> dict[str, Any]:
    return {"title": f"note-{index}", "markdown": f"# Note {index}\n\nA paragraph of what note {index} keeps.\n"}


# name, app, kind of the resource that makes one, action, its arguments
KINDS: list[tuple[str, str, str, str, Callable[[int], dict[str, Any]]]] = [
    ("tasks issue", "tasks", "tasks.issues", "create", issue),
    ("notes page", "notes", "notes", "create-page", note),
]


def quantile(samples: list[float], q: float) -> float:
    if not samples:
        return float("nan")
    ordered = sorted(samples)
    return ordered[round((len(ordered) - 1) * q)]


async def resource_of(ws: Any, app: str, kind: str, action: str) -> str:
    async for row in rows(ws, path=app, limit="200"):
        actions = row.get("actions") or []
        if row.get("kind") == kind or action in actions:
            return str(row["path"])
    raise SystemExit(f"no {kind} with {action} under {app}")


async def heard(watch: Any, since: float, timeout: float = 20.0) -> float | None:
    try:
        async with asyncio.timeout(timeout):
            async for event in watch:
                if event.get("type") == "change":
                    return time.perf_counter() - since
    except TimeoutError:
        return None
    return None


async def run(args: argparse.Namespace) -> list[dict[str, Any]]:
    table: list[dict[str, Any]] = []
    async with Tilo() as tilo, Tilo() as other:
        for size in args.sizes:
            made = (await run(tilo, {"path": "account/workspaces", "action": "create"}, {"name": f"Bench {size}"}))["result"]["workspace"]
            ws = tilo.workspace(made["id"])
            seen = other.workspace(made["id"])
            for _, app, _, _, _ in KINDS:
                await run(ws, {"path": app, "action": "open"})
            for name, app, kind, action, make in KINDS:
                path = await resource_of(ws, app, kind, action)
                record = ws.resource(path)
                began = time.perf_counter()
                await record.many(action, [make(index) for index in range(size)])
                seeded = time.perf_counter() - began
                took: list[float] = []
                arrived: list[float] = []
                for index in range(args.singles):
                    watch = seen.events(path=path)
                    began = time.perf_counter()
                    await run(ws, {"path": path, "action": action}, make(size + index))
                    took.append(time.perf_counter() - began)
                    late = await heard(watch, began)
                    if late is not None:
                        arrived.append(late)
                    await watch.aclose()
                items = [make(size + args.singles + index) for index in range(args.bulk)]
                began = time.perf_counter()
                await record.many(action, items)
                bulk = args.bulk / (time.perf_counter() - began)
                row = {
                    "kind": name,
                    "size": size,
                    "single_p50_ms": 1000 * quantile(took, 0.5),
                    "single_p99_ms": 1000 * quantile(took, 0.99),
                    "seed_per_s": size / seeded if size else 0.0,
                    "bulk_per_s": bulk,
                    "second_device_p50_ms": 1000 * quantile(arrived, 0.5) if arrived else None,
                }
                print(json.dumps(row), flush=True)
                table.append(row)

            # A folder of small files, as `tilo files put` sends it.
            if args.files and shutil.which("tilo"):
                with tempfile.TemporaryDirectory() as folder:
                    for index in range(args.files):
                        sub = os.path.join(folder, f"m{index // 50}")
                        os.makedirs(sub, exist_ok=True)
                        with open(os.path.join(sub, f"f{index}.rs"), "w") as out:
                            out.write(f"fn file_{index}() {{}}\n" * 40)
                    began = time.perf_counter()
                    subprocess.run(
                        ["tilo", "files", "put", folder, "-w", str(made["id"]), "--to", "src"],
                        check=True,
                        stdout=subprocess.DEVNULL,
                    )
                    rate = args.files / (time.perf_counter() - began)
                    row = {"kind": "files put", "size": size, "bulk_per_s": rate}
                    print(json.dumps(row), flush=True)
                    table.append(row)

        if args.large_mb:
            made = (await run(tilo, {"path": "account/workspaces", "action": "create"}, {"name": "Bench large"}))["result"]["workspace"]
            ws = tilo.workspace(made["id"])
            data = os.urandom(args.large_mb * 1024 * 1024)
            began = time.perf_counter()
            await ws.upload(data)
            rate = args.large_mb / (time.perf_counter() - began)
            row = {"kind": f"upload {args.large_mb} MB", "size": 0, "bulk_per_s": rate}
            print(json.dumps(row), flush=True)
            table.append(row)
    return table


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--sizes", type=lambda text: [int(part) for part in text.split(",")], default=[0, 1000])
    parser.add_argument("--singles", type=int, default=30)
    parser.add_argument("--bulk", type=int, default=1000)
    parser.add_argument("--files", type=int, default=1000)
    parser.add_argument("--large-mb", type=int, default=256)
    parser.add_argument("--out")
    args = parser.parse_args()
    table = asyncio.run(run(args))
    if args.out:
        with open(args.out, "w") as out:
            json.dump(table, out, indent=2)
    singles = [row["single_p50_ms"] for row in table if "single_p50_ms" in row]
    if singles:
        print(f"median single action p50: {statistics.median(singles):.1f} ms", file=sys.stderr)


if __name__ == "__main__":
    main()
