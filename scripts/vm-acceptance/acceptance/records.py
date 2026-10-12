"""The run's measurements as performance records (tools/perf/record.py).

Checks hand their samples to `Context.record` whether they pass or not: a number over
its budget is the record that matters most. run.py `--records` turns them into one
JSON line per KPI with the run's environment, and the workflow compares them with the
store (DEVELOPMENT.md, "Performance results"). The environment says what produced
them: a KVM guest on a CI runner class, rendering on the VM's renderer. A software VM's
numbers are never compared with, or quoted as, hardware figures.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools/perf"))

import record  # noqa: E402

# The dev image's components are release builds (`tilo-vm swap --builder host` too).
PROFILE = "release"


def renderer(label: str | None, display: dict | None = None) -> str:
    """The record's renderer for run.py's renderer line: a CPU rasterizer (Mesa's
    Vulkan CPU driver, lavapipe, which wgpu reports as llvmpipe), virgl, or a GPU."""
    text = (label or "").lower()
    if text.startswith("software"):
        return "lavapipe"
    if "virgl" in text:
        return "virgl"
    if (display or {}).get("hardware"):
        return "gpu"
    raise ValueError(f"cannot name the renderer of {label!r}")


def environment(commit: str, renderer_name: str, runner: str, guest: str) -> dict:
    """The environment every record of the run carries; `guest` describes the VM."""
    return {"commit": commit, "profile": PROFILE, "renderer": renderer_name,
            "virtualization": "kvm", "runner": runner,
            "hardware": f"{record.detect_hardware()}; guest {guest}", "started_at": record.utc_now()}


def sample(kpi: str, unit: str, values, check: str | None, meta: dict) -> dict | None:
    """One check's samples for `kpi`, or None when it measured nothing."""
    numbers = [float(v) for v in values if v is not None]
    if not numbers:
        return None
    if unit not in record.UNITS or not record.KPI.fullmatch(kpi):
        raise ValueError(f"{kpi} [{unit}] is not a valid KPI and unit")
    return {"kpi": kpi, "unit": unit, "samples": numbers, "check": check,
            "meta": {name: str(value) for name, value in meta.items()}}


def build(samples: list[dict], env: dict, meta: dict | None = None) -> list[record.Record]:
    """One record per KPI; samples of one KPI from several calls are pooled."""
    pooled: dict[str, dict] = {}
    for item in samples:
        entry = pooled.setdefault(item["kpi"], {"unit": item["unit"], "samples": [],
                                                "meta": {}, "checks": []})
        if entry["unit"] != item["unit"]:
            raise ValueError(f"{item['kpi']} was recorded in {entry['unit']} and {item['unit']}")
        entry["samples"] += item["samples"]
        entry["meta"].update(item["meta"])
        if item["check"] and item["check"] not in entry["checks"]:
            entry["checks"].append(item["check"])
    out = []
    for kpi, entry in pooled.items():
        item_meta = {**(meta or {}), **entry["meta"], "check": ",".join(entry["checks"])}
        out.append(record.from_samples(kpi, entry["unit"], entry["samples"], meta=item_meta, **env))
    return out


def write(path: Path, records: list[record.Record]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(item.to_json() + "\n" for item in records), encoding="utf-8")
