"""report.json (machine-readable) and report.md (for people) for one run."""

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
    return doc


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
