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

    def wait_still(self, timeout: float = 3.0) -> dict:
        """The state once nothing asks for animation frames (the dump's `animating` is
        empty: an overlay's fade or a resize has ended), or the latest one after
        `timeout`."""
        last = {}

        def still():
            last["s"] = self.vm.state()
            return last["s"] if not (last["s"].get("chrome") or {}).get("animating") else None

        return wait_until(still, timeout, interval=0.2) or last["s"]

    def wait_presented(self, app_id: str, timeout: float = 30.0, after_seq: int = 0):
        """`app_id`'s foreign frame once it has presented a frame past `after_seq`, else None."""
        def presented():
            for f in self.vm.foreign_frames():
                if f.get("app_id") == app_id and f.get("presented") and f.get("frame_seq", 0) > after_seq:
                    return f
            return None

        return wait_until(presented, timeout, interval=0.3)

    def frame_seq(self, app_id: str) -> int | None:
        """The newest frame `app_id` presented, or None while it shows none."""
        seqs = [f.get("frame_seq", 0) for f in self.vm.foreign_frames()
                if f.get("app_id") == app_id and f.get("presented")]
        return max(seqs) if seqs else None

    # -- a known desktop between checks ---------------------------------------

    def close_overlays(self) -> None:
        """Escape until no modal, overlay, prompt or dialog is open (`core.open_surfaces`):
        overlays and search, and also the Invite composer, portal sheets and the rest the
        state dump reports outside `overlays`."""
        for _ in range(3):
            before = core.open_surfaces(self.vm.state())
            if not before:
                return
            self.vm.key("Escape")
            # Escape closes the top surface; the next press is for what is still open.
            wait_until(lambda: core.open_surfaces(self.vm.state()) != before, 3, 0.2)

    def close_all_windows(self, timeout: float = 20) -> None:
        """Close every window group through its own close action.

        A group whose close action fails outright is recorded in facts["unclosable"] and
        left alone: the check that owns it reports it, the others go on."""
        deadline = time.monotonic() + timeout
        stuck = self.facts.setdefault("unclosable", {})
        while True:
            closers = [a for a in self.vm.actions()
                       if a["provider"] == "desktop-windows" and a["id"].startswith("group.close:")
                       and a["id"] not in stuck]
            if not closers:
                return
            if time.monotonic() >= deadline:
                raise core.CheckFailed("windows still open after closing every group")
            closing = set()
            for a in closers:
                try:
                    self.vm.invoke(a["provider"], a["id"], {"confirm": True}
                                   if a.get("params") else None)
                    closing.add(a["id"].removeprefix("group.close:"))
                except VmError as exc:
                    if "action_failed" in str(exc):
                        stuck[a["id"]] = a.get("title")

            def closed():
                live = {str(g["id"]) for g in core.all_groups(self.vm.state())}
                return not closing & live

            # A close a client did not act on is asked for again.
            wait_until(closed, max(0.0, min(5.0, deadline - time.monotonic())), 0.25)

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
                # The invocation is the probe: it succeeds once the catalogue lists the app.
                time.sleep(0.5)
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
