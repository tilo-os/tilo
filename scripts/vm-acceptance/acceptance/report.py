"""report.json (machine-readable) and report.md (for people) for one run, and
failure-evidence.txt: the text evidence of the failed checks, bounded, for the log."""

from __future__ import annotations

import json
from dataclasses import asdict
from pathlib import Path

from .core import AREAS, Result

ESSENTIALS = [
    ("provenance", "0. Commit, image and session"),
    ("window", "1. Window, workspace and strip management, gestures, animations"),
    ("ui", "2. UI consistency (themes, languages, layout)"),
    ("apps", "3. Open/close of apps"),
    ("search", "4. Search and history/activity"),
    ("resources", "5. Resources"),
    ("basics", "6. Basics (hardware controls, keyboard layout, displays)"),
    ("agent", "7. Agent door (in-session MCP from a terminal)"),
    ("input", "8. Input fidelity (real device descriptors, layouts, touchpad)"),
]

# A failed run's report directory may not outlive it, so the text its failures left is
# printed into the log too: up to EVIDENCE_FILE_BYTES of each file (the end of a long log,
# where the failure is) and EVIDENCE_TOTAL_BYTES for the run.
EVIDENCE_FILE_BYTES = 4096
EVIDENCE_TOTAL_BYTES = 96 * 1024
TEXT_SUFFIXES = {".txt", ".log", ".json", ".jsonl", ".csv", ".md"}

# `xfail` is a named, expected failure (core.ExpectedFailure): it does not fail the run.
PASSING = ("pass", "xfail")
ICONS = {"pass": "✅ pass", "xfail": "⚠️ xfail"}


def _compact(v, limit=220):
    s = json.dumps(v, default=str, ensure_ascii=False)
    return s if len(s) <= limit else s[:limit] + "…"


def write(out: Path, meta: dict, results: list[Result], owners: dict) -> dict:
    summary = {}
    for area, _ in ESSENTIALS:
        rs = [r for r in results if r.area == area]
        summary[area] = {"pass": sum(r.status == "pass" for r in rs),
                         "xfail": sum(r.status == "xfail" for r in rs),
                         "fail": sum(r.status not in PASSING for r in rs),
                         "unobservable": sum(r.unobservable for r in rs),
                         "owner": owners.get(area)}
    overall = "pass" if results and all(r.status in PASSING for r in results) else "fail"
    doc = {**meta, "overall": overall, "summary": summary, "results": [asdict(r) for r in results]}
    (out / "report.json").write_text(json.dumps(doc, indent=1, default=str, ensure_ascii=False))
    (out / "report.md").write_text(_markdown(meta, summary, results, owners, overall))
    (out / "failure-evidence.txt").write_text(failure_evidence(out, results))
    return doc


def failure_evidence(out: Path, results: list[Result]) -> str:
    """The text evidence files of every failed check, each cut to EVIDENCE_FILE_BYTES and
    the whole to EVIDENCE_TOTAL_BYTES; images are left out. JSON is printed compact from
    its start, where its summary fields are; other text from its end, where the failure is."""
    L, total = [], 0
    for r in results:
        if r.status in PASSING:
            continue
        for rel in r.evidence:
            path = out / rel
            if path.suffix not in TEXT_SUFFIXES or not path.is_file():
                continue
            header = f"==== {r.id}: {rel}"
            if total >= EVIDENCE_TOTAL_BYTES:
                L.append(f"{header}: left out, the run's printed evidence reached {EVIDENCE_TOTAL_BYTES} bytes")
                continue
            data = path.read_bytes()
            if path.suffix == ".json":
                try:
                    data = json.dumps(json.loads(data), separators=(",", ":"), ensure_ascii=False).encode()
                except ValueError:
                    pass
            cut = data[:EVIDENCE_FILE_BYTES] if path.suffix == ".json" else data[-EVIDENCE_FILE_BYTES:]
            if len(cut) < len(data):
                header += f" ({'first' if path.suffix == '.json' else 'last'} {len(cut)} of {len(data)} bytes)"
            L += [header, cut.decode("utf-8", "replace").rstrip("\n")]
            total += len(cut)
    return "\n".join(L) + "\n" if L else ""


def _markdown(meta, summary, results, owners, overall) -> str:
    L = []
    L.append(f"# Tilo VM acceptance — {meta.get('commit_short', '?')} — **{overall.upper()}**\n")
    p = meta.get("provenance") or {}
    rows = [
        ("Commit", f"`{meta.get('commit')}`" + (f" ({meta.get('commit_subject')})" if meta.get("commit_subject") else "")),
        ("Disk", f"`{p.get('disk')}`"),
        ("Built from", _compact(p.get("record"), 400)),
        ("Booted image digest", f"`{p.get('booted_digest')}`"),
        ("Swapped in", _swapped(p.get("swap"))),
        ("VM", f"{meta.get('vm')} ({meta.get('vm_memory_mb')} MiB, {meta.get('vm_cpus')} vCPU, gl={meta.get('gl')})"),
        ("Slot", f"{meta.get('slot')} headful, outputs {', '.join(meta.get('outputs', []))}"),
        ("Renderer", str(meta.get("renderer"))),
        ("Started / finished", f"{meta.get('started')} / {meta.get('finished')}"),
        ("Suite", f"`{meta.get('suite_commit')}` — {meta.get('command')}"),
    ]
    L.append("| | |\n|---|---|")
    L += [f"| {k} | {v} |" for k, v in rows]
    L.append("\n## Per essential\n")
    L.append("| Essential | Pass | Expected fail | Fail | of which unobservable | Owner |\n|---|---|---|---|---|---|")
    for area, title in ESSENTIALS:
        s = summary[area]
        L.append(f"| {title} | {s['pass']} | {s['xfail']} | {s['fail']} | {s['unobservable']} | {owners.get(area, '')} |")
    L.append("\n## Failures by owning area\n")
    for area, title in ESSENTIALS:
        fails = [r for r in results if r.area == area and r.status != "pass"]  # xfail too: named, not hidden
        if not fails:
            continue
        L.append(f"### {title} — {owners.get(area, '')}\n")
        for r in fails:
            tag = " (unobservable)" if r.unobservable else " (expected failure)" if r.status == "xfail" else ""
            L.append(f"- **{r.id} {r.title}**{tag}: {r.detail}")
    L.append("\n## All checks\n")
    for area, title in ESSENTIALS:
        rs = [r for r in results if r.area == area]
        if not rs:
            continue
        L.append(f"### {title}\n")
        L.append("| Check | Result | Detail | Numbers | Evidence |\n|---|---|---|---|---|")
        for r in rs:
            ev = " ".join(f"[{Path(e).stem}]({e})" for e in r.evidence)
            det = r.detail.replace("|", "\\|").replace("\n", " ")
            num = _compact(r.metrics).replace("|", "\\|") if r.metrics else ""
            L.append(f"| {r.id} {r.title} | {ICONS.get(r.status, '❌ fail')} | {det} | {num} | {ev} |")
        L.append("")
    return "\n".join(L) + "\n"


def _swapped(swap) -> str:
    if not swap:
        return "nothing: the disk's image as built"
    parts = [f"{c['component']} → {c['slot']}" + (f" (failed: {c['error']})" if c.get("error") else "")
             for c in swap["components"]]
    return f"from `{swap['ref']}` (`{swap['commit'][:12]}`): " + ", ".join(parts)


def owners_from(arg: str | None) -> dict:
    owners = dict(AREAS)
    if arg:
        for part in arg.split(","):
            k, _, v = part.partition("=")
            owners[k.strip()] = v.strip()
    return owners
