#!/usr/bin/env python3
"""One performance result format: a KPI's sample set becomes one JSON line.

A record names the KPI, its unit and its distribution (n, nearest-rank p50,
p95, p99 and max, the same percentiles `scripts/perf/runtime_benchmark.py`
reports), and the environment that produced it:

- `commit`: the source revision measured;
- `profile`: release, thin-app or dev;
- `renderer`: gpu, llvmpipe, lavapipe, virgl, swiftshader (Chrome's
  software WebGPU, for the browser runtime on a runner without a GPU), or none
  for a KPI that draws nothing (bytes, filesystem latency);
- `virtualization`: none, kvm or tcg;
- `runner`: the runner class (`runner`, a device slot) or none for a
  runner-independent counter;
- `hardware`: a description (CPU model, cores); not part of the comparison
  key, because one runner class spans several hosts;
- `started_at`: UTC, ISO 8601.

`tools/perf/compare.py` only compares records whose KPI and environment key
(profile, renderer, virtualization, runner) match, so a software-rendered or
TCG result never becomes a baseline for a hardware one.

Usage:

    python3 tools/perf/record.py env --profile dev --renderer none \
        --virtualization kvm --runner runner > env.json
    python3 tools/perf/record.py emit --kpi fs.metadata-cold --unit ms \
        --samples-file samples.txt --env-json env.json >> results.jsonl

A samples file holds one number per line or a JSON array. Records carry no
secrets: public numbers, commit SHAs and runner descriptions only.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime
import json
import math
import os
import platform
import re
import subprocess
import sys
from pathlib import Path
from typing import Any, Iterable

SCHEMA = 1
# `percent` is a share of one CPU (or of a resource) over a window; `per_s` a rate;
# `usd` a price derived from measurements and tools/perf/prices.toml.
UNITS = frozenset({"ms", "us", "s", "bytes", "MiB", "count", "W", "fps", "pixels", "percent", "per_s",
                   "usd"})
PROFILES = frozenset({"release", "thin-app", "dev"})
RENDERERS = frozenset({"gpu", "llvmpipe", "lavapipe", "virgl", "swiftshader", "none"})
VIRTUALIZATIONS = frozenset({"none", "kvm", "tcg"})
KPI = re.compile(r"[a-z0-9][a-z0-9_-]*(?:\.[A-Za-z0-9_-]+)+")
ENV_FIELDS = ("commit", "profile", "renderer", "virtualization", "runner", "hardware", "started_at")
KEY_FIELDS = ("profile", "renderer", "virtualization", "runner")


@dataclasses.dataclass
class Record:
    kpi: str
    unit: str
    n: int
    p50: float
    p95: float
    p99: float
    max: float
    commit: str
    profile: str
    renderer: str
    virtualization: str
    runner: str
    hardware: str
    started_at: str
    # Free-form provenance (registry sequence, repetition count, a synthetic
    # factor). Never a secret.
    meta: dict[str, Any] = dataclasses.field(default_factory=dict)
    # Written by compare.py when the record enters the store.
    verdict: dict[str, Any] | None = None
    schema: int = SCHEMA

    @property
    def group(self) -> str:
        return self.kpi.split(".", 1)[0]

    @property
    def env_key(self) -> tuple[str, ...]:
        return tuple(getattr(self, name) for name in KEY_FIELDS)

    def validate(self) -> None:
        if not KPI.fullmatch(self.kpi):
            raise ValueError(f"KPI id {self.kpi!r} must be dotted lowercase, e.g. render.frame_ms.4k")
        if self.unit not in UNITS:
            raise ValueError(f"{self.kpi}: unit {self.unit!r} is not one of {sorted(UNITS)}")
        if self.n < 1:
            raise ValueError(f"{self.kpi}: a record needs at least one sample")
        values = (self.p50, self.p95, self.p99, self.max)
        if any(not math.isfinite(value) or value < 0 for value in values):
            raise ValueError(f"{self.kpi}: percentiles must be finite and non-negative")
        if list(values) != sorted(values):
            raise ValueError(f"{self.kpi}: p50 <= p95 <= p99 <= max must hold")
        for name, allowed in (("profile", PROFILES), ("renderer", RENDERERS),
                              ("virtualization", VIRTUALIZATIONS)):
            if getattr(self, name) not in allowed:
                raise ValueError(f"{self.kpi}: {name} {getattr(self, name)!r} is not one of {sorted(allowed)}")
        for name in ENV_FIELDS:
            if not str(getattr(self, name)).strip():
                raise ValueError(f"{self.kpi}: environment field {name} is empty")

    def to_json(self) -> str:
        data = dataclasses.asdict(self)
        if data["verdict"] is None:
            del data["verdict"]
        return json.dumps(data, sort_keys=True, separators=(",", ":"))

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "Record":
        fields = {field.name for field in dataclasses.fields(cls)}
        unknown = set(data) - fields
        if unknown:
            raise ValueError(f"record has unknown fields {sorted(unknown)}")
        record = cls(**data)
        if record.schema != SCHEMA:
            raise ValueError(f"{record.kpi}: schema {record.schema} is not {SCHEMA}")
        record.validate()
        return record


def nearest_rank(ordered: list[float], percentile: float) -> float:
    """The nearest-rank percentile of sorted values (runtime_benchmark.py's rule)."""
    return ordered[math.ceil(len(ordered) * percentile / 100) - 1]


def from_samples(kpi: str, unit: str, samples: Iterable[float], **env: Any) -> Record:
    """One record from raw samples; `env` holds ENV_FIELDS and optionally meta."""
    ordered = sorted(float(value) for value in samples)
    if not ordered:
        raise ValueError(f"{kpi}: no samples were recorded")
    meta = dict(env.pop("meta", None) or {})
    missing = [name for name in ENV_FIELDS if name not in env]
    if missing:
        raise ValueError(f"{kpi}: environment is missing {missing}")
    extra = set(env) - set(ENV_FIELDS)
    if extra:
        raise ValueError(f"{kpi}: unknown environment fields {sorted(extra)}")
    record = Record(kpi=kpi, unit=unit, n=len(ordered), p50=nearest_rank(ordered, 50),
                    p95=nearest_rank(ordered, 95), p99=nearest_rank(ordered, 99), max=ordered[-1],
                    meta=meta, **{name: str(env[name]) for name in ENV_FIELDS})
    record.validate()
    return record


def write(path: Path, record: Record) -> None:
    """Append one record to a JSON-lines file."""
    record.validate()
    with path.open("a", encoding="utf-8") as stream:
        stream.write(record.to_json() + "\n")


def parse_lines(text: str, source: str = "<records>") -> list[Record]:
    records = []
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            records.append(Record.from_dict(json.loads(line)))
        except (ValueError, TypeError) as error:
            raise ValueError(f"{source}:{number}: {error}") from error
    return records


def read(path: Path) -> list[Record]:
    return parse_lines(path.read_text(encoding="utf-8"), str(path))


def utc_now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def detect_commit() -> str:
    if os.environ.get("GITHUB_SHA"):
        return os.environ["GITHUB_SHA"]
    result = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise ValueError("cannot name the measured commit: set GITHUB_SHA or run inside the checkout")
    return result.stdout.strip()


def detect_hardware() -> str:
    model = ""
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        match = re.search(r"^model name\s*:\s*(.+)$", cpuinfo.read_text(errors="replace"), re.M)
        model = match.group(1).strip() if match else ""
    if not model and platform.system() == "Darwin":
        result = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"],
                                capture_output=True, text=True, check=False)
        model = result.stdout.strip()
    model = model or platform.processor() or platform.machine()
    return f"{model}; {os.cpu_count()} cpus; {platform.system()} {platform.machine()}"


def environment(profile: str, renderer: str, virtualization: str, runner: str,
                hardware: str | None = None) -> dict[str, str]:
    env = {"commit": detect_commit(), "profile": profile, "renderer": renderer,
           "virtualization": virtualization, "runner": runner,
           "hardware": hardware or detect_hardware(), "started_at": utc_now()}
    # Validate through a throwaway record so a bad label fails here, not later.
    from_samples("env.check", "count", [0], **env)
    return env


def load_samples(path: Path) -> list[float]:
    text = path.read_text(encoding="utf-8").strip()
    if text.startswith("["):
        values = json.loads(text)
    else:
        values = [line for line in text.splitlines() if line.strip()]
    return [float(value) for value in values]


def load_env(path: Path) -> dict[str, Any]:
    env = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(env, dict):
        raise ValueError(f"{path}: the environment must be a JSON object")
    return env


def parse_meta(pairs: list[str]) -> dict[str, str]:
    meta = {}
    for pair in pairs:
        name, separator, value = pair.partition("=")
        if not separator or not name:
            raise ValueError(f"--meta {pair!r} must be NAME=VALUE")
        meta[name] = value
    return meta


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    env_parser = commands.add_parser("env", help="print the environment JSON of this run")
    env_parser.add_argument("--profile", required=True, choices=sorted(PROFILES))
    env_parser.add_argument("--renderer", required=True, choices=sorted(RENDERERS))
    env_parser.add_argument("--virtualization", required=True, choices=sorted(VIRTUALIZATIONS))
    env_parser.add_argument("--runner", required=True)
    env_parser.add_argument("--hardware", help="override the detected hardware description")
    emit = commands.add_parser("emit", help="print one record for a samples file")
    emit.add_argument("--kpi", required=True)
    emit.add_argument("--unit", required=True, choices=sorted(UNITS))
    emit.add_argument("--samples-file", required=True, type=Path)
    emit.add_argument("--env-json", required=True, type=Path)
    emit.add_argument("--meta", action="append", default=[], metavar="NAME=VALUE")
    args = parser.parse_args(argv)
    try:
        if args.command == "env":
            print(json.dumps(environment(args.profile, args.renderer, args.virtualization,
                                         args.runner, args.hardware), sort_keys=True))
        else:
            env = load_env(args.env_json)
            env["meta"] = {**env.get("meta", {}), **parse_meta(args.meta)}
            print(from_samples(args.kpi, args.unit, load_samples(args.samples_file), **env).to_json())
    except ValueError as error:
        print(f"record.py: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
