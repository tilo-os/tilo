"""Which GPU the session's windows render on, judged against the GPU the machine has.

Every app window prints one line per GPU device it opens; the session log keeps it as
`[app <pkg> stderr] tilo-gpu: window adapter "<name>" type=<type> backend=<backend>
driver="<driver>"`, and a window that ends up on a CPU rasterizer although a hardware
adapter exists also prints `tilo-gpu: window renders on a CPU adapter although a GPU is
present: ...`. `tilo system health --gpu --json` says whether the machine has a hardware
render node with a GPU behind it at all ("no-gpu"; a virtio GPU without 3D acceleration has a
render node but no GPU) and whether the session uses it ("ok" or "degraded").

Plain functions over text, so the VM check (R05) and the installed-image gate in CI
(gpu_check.py) judge alike.
"""

from __future__ import annotations

import json
import re

LOG = "$HOME/.local/state/tilo/session-logs/latest.log"
LOG_SEPARATOR = "--- tilo-gpu lines"

# Run in the guest as the session's user: the health report, then every app's adapter lines.
# The health command exits non-zero on "degraded"; its JSON is what is judged.
PROBE = (f'tilo system health --gpu --json 2>&1; echo; echo "{LOG_SEPARATOR}"; '
         f'grep -a "stderr\\] tilo-gpu: " "{LOG}" 2>/dev/null; true')

ADAPTER = re.compile(r'\[app (?P<app>\S+) stderr\] tilo-gpu: window adapter "(?P<name>[^"]*)" '
                     r'type=(?P<type>\w+) backend=(?P<backend>\w+) driver="(?P<driver>[^"]*)"')
CPU_FALLBACK = re.compile(r"\[app (?P<app>\S+) stderr\] tilo-gpu: "
                          r"(?P<message>window renders on a CPU adapter although a GPU is present.*)")
STATUSES = ("ok", "degraded", "no-gpu")


def parse_adapters(log: str) -> list[dict]:
    """Every `window adapter` line in `log`, in order."""
    return [m.groupdict() for m in ADAPTER.finditer(log)]


def parse_fallbacks(log: str) -> list[dict]:
    """Every line where a window reported landing on a CPU adapter beside a GPU."""
    return [{"app": m["app"], "message": m["message"].strip()} for m in CPU_FALLBACK.finditer(log)]


def parse_health(text: str) -> dict | None:
    """The JSON object `tilo system health --gpu --json` printed, else None. Lines before
    it (a warning on stderr) are skipped; a pretty-printed object is read whole."""
    start = text.find("{")
    while start != -1:
        try:
            doc, _ = json.JSONDecoder().raw_decode(text[start:])
        except ValueError:
            start = text.find("{", start + 1)
            continue
        if isinstance(doc, dict) and "status" in doc:
            return doc
        start = text.find("{", start + 1)
    return None


def split_probe(out: str) -> tuple[str, str]:
    """`PROBE` output -> (health text, log lines)."""
    health, _, log = out.partition(LOG_SEPARATOR)
    return health, log


def judge(health: dict | None, adapters: list[dict], fallbacks: list[dict],
          health_text: str = "") -> tuple[str, str]:
    """("pass" | "fail" | "unobservable", why).

    A machine without a GPU behind its render nodes has nothing to fall back from, so it cannot
    show the fault: unobservable. Otherwise the health report must be "ok", no window may
    report a CPU adapter, and at least one window must have reported its adapter at all."""
    if health is None:
        return "unobservable", ("`tilo system health --gpu --json` printed no health report: "
                                f"{health_text.strip()[:300]!r}")
    status = health.get("status")
    problems = [str(p) for p in health.get("problems") or []]
    if status == "no-gpu":
        return "unobservable", ("no render node with a GPU behind it in this machine (a "
                                "software-only VM; boot it with --gl for virgl), so a CPU "
                                "fallback cannot be told apart")
    if status not in STATUSES:
        return "unobservable", f"unknown GPU health status {status!r}"
    faults = []
    if status == "degraded":
        faults.append("GPU health is degraded: " + ("; ".join(problems) or "no problems listed"))
    cpu = [a for a in adapters if a["type"] == "Cpu"]
    if cpu:
        faults.append("windows render on a CPU adapter although the machine has a GPU: " +
                      ", ".join(sorted({f"{a['app']} ({a['name']}, {a['backend']})" for a in cpu})))
    if fallbacks:
        faults.append("apps reported a CPU fallback: " +
                      "; ".join(sorted({f"{f['app']}: {f['message'][:160]}" for f in fallbacks})))
    if faults:
        return "fail", "; ".join(faults)
    if not adapters:
        return "unobservable", "no app window reported its GPU adapter (no `tilo-gpu: window adapter` line)"
    return "pass", "every window renders on " + ", ".join(
        sorted({f"{a['name']} ({a['type']}, {a['backend']})" for a in adapters}))


def summary(adapters: list[dict]) -> dict:
    """Adapters per app, for the report."""
    out: dict = {}
    for a in adapters:
        line = f"{a['name']} type={a['type']} backend={a['backend']} driver={a['driver']}"
        if line not in out.setdefault(a["app"], []):
            out[a["app"]].append(line)
    return out
