"""Per-run state every check shares: the VM handle, evidence, and helpers."""

from __future__ import annotations

import time
from pathlib import Path

from . import core, records
from .vm import TiloVm, VmError, wait_until


class Context:
    def __init__(self, vm: TiloVm, out: Path, config: dict):
        self.vm = vm
        self.out = out
        self.config = config
        self.facts: dict = {}
        self.metrics: dict = {}
        self.current: core.Result | None = None
        # Performance samples for run.py --records (acceptance/records.py).
        self.samples: list[dict] = []
        (out / "shots").mkdir(parents=True, exist_ok=True)

    # -- evidence ------------------------------------------------------------

    def record(self, kpi: str, unit: str, values, **meta) -> None:
        """Keep `values` as samples of the performance KPI `kpi`, measured by the
        running check; a check records before it judges, so a miss is recorded too."""
        item = records.sample(kpi, unit, values, self.current.id if self.current else None, meta)
        if item is not None:
            self.samples.append(item)

    def shot(self, name: str) -> Path:
        path = self.out / "shots" / f"{name}.png"
        self.vm.shot(path)
        self.evidence(path)
        return path

    def evidence(self, path: Path) -> None:
        if self.current is not None:
            self.current.evidence.append(str(path.relative_to(self.out)))

    def write(self, name: str, text: str) -> Path:
        path = self.out / "data" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        self.evidence(path)
        return path

    # -- state ----------------------------------------------------------------

    def state(self) -> dict:
        return self.vm.state()

    def wait_state(self, predicate, timeout: float = 8.0, what: str = "state"):
        """Poll the state dump until `predicate(state)` holds; return that state."""
        last = {}

        def probe():
            last["s"] = self.vm.state()
            return last["s"] if predicate(last["s"]) else None

        got = wait_until(probe, timeout, interval=0.4)
        if got is None:
            raise core.CheckFailed(f"{what} not reached within {timeout:.0f}s")
        return got

    def settle(self, seconds: float = 0.8) -> None:
        time.sleep(seconds)

    # -- a known desktop between checks ---------------------------------------

    def close_overlays(self) -> None:
        for _ in range(3):
            st = self.vm.state()
            open_ = [k for k, v in core.overlays(st).items() if v]
            palette = (st["chrome"].get("palette") or {}).get("visible")
            if not open_ and not palette and not st["chrome"]["window_switcher"].get("open"):
                return
            self.vm.key("Escape")
            time.sleep(0.6)

    def close_all_windows(self, timeout: float = 20) -> None:
        """Close every window group through its own close action.

        A group whose close action fails outright is recorded in facts["unclosable"] and
        left alone: the check that owns it reports it, the others go on."""
        deadline = time.monotonic() + timeout
        stuck = self.facts.setdefault("unclosable", {})
        while time.monotonic() < deadline:
            closers = [a for a in self.vm.actions()
                       if a["provider"] == "desktop-windows" and a["id"].startswith("group.close:")
                       and a["id"] not in stuck]
            if not closers:
                return
            for a in closers:
                try:
                    self.vm.invoke(a["provider"], a["id"], {"confirm": True}
                                   if a.get("params") else None)
                except VmError as exc:
                    if "action_failed" in str(exc):
                        stuck[a["id"]] = a.get("title")
            time.sleep(1.0)
        raise core.CheckFailed("windows still open after closing every group")

    def reset(self) -> None:
        self.close_overlays()
        self.close_all_windows()
        self.close_overlays()

    # -- opening things ---------------------------------------------------------

    def open_app(self, app_id: str, provider: str = "applications", timeout: float = 25):
        """Open an app through its search action; return (group, seconds to its group)."""
        before = {g["id"] for g in core.all_groups(self.vm.state())}
        started = time.monotonic()
        # A fresh session publishes its app catalogue a few seconds after it starts.
        while True:
            try:
                self.vm.invoke(provider, app_id)
                break
            except VmError as exc:
                if "unknown_action" not in str(exc):
                    raise
                if time.monotonic() - started > 45:
                    raise core.CheckFailed(f"{app_id} is not in the {provider} catalogue "
                                           "45 s after asking for it") from exc
                time.sleep(2)
        st = self.wait_state(
            lambda s: [g for g in core.all_groups(s) if g["id"] not in before],
            timeout, f"a new window group for {app_id}")
        new = [g for g in core.all_groups(st) if g["id"] not in before][0]
        return new, round(time.monotonic() - started, 2)

    def new_terminal_window(self):
        before = {g["id"] for g in core.all_groups(self.vm.state())}
        self.vm.invoke("actions", "window.new.terminal")
        st = self.wait_state(lambda s: [g for g in core.all_groups(s) if g["id"] not in before],
                             10, "a new Terminal window")
        return [g for g in core.all_groups(st) if g["id"] not in before][0]
