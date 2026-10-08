#!/usr/bin/env python3
"""Strict Terminal proof in an already activated, owner-controlled cohort slot."""
import argparse
import hashlib
import importlib.util
import json
import re
import time
import uuid
from pathlib import Path

from acceptance.vm import TiloVm, first_json, wait_until
from acceptance.action_jobs import require, succeeded, invoke
from acceptance.cohort import selected_layer, runtime_inventory

APP = "so.tilo.terminal"


def identity(row):
    return tuple(row.get(k) for k in ("application", "room", "record", "resource"))


def terminal_rows(rows):
    return [r for r in rows if r.get("application") == APP
            and r.get("semantic_identity") == {"kind": "terminal.session", "key": "terminal.session"}]


# Reuse the maintained scoped action, authenticated native custody and kernel-FD
# backing proof. No second control client or app-private discovery protocol.
_spec = importlib.util.spec_from_file_location("terminal_app_cohort", Path(__file__).with_name("app-cohort.py"))
apps = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(apps)
FAMILY = "terminal.session"


def topology(state):
    return {(str(workspace["room"]), str(tab["id"]))
            for workspace in state["workspace_tree"]["workspaces"]
            for strip in workspace["strips"] for group in strip["window_groups"] for tab in group["tabs"]}


def executable(shown, layer):
    rows = [row for row in shown.get("files", []) if row.get("component") == "apps+runtime"
            and row.get("layer") == layer and isinstance(row.get("path"), str)
            and row["path"].startswith("/usr/share/tilo-apps/") and row["path"].endswith("/bin/terminal")]
    require(len(rows) == 1 and "link" not in rows[0]
            and isinstance(rows[0].get("sha256"), str) and re.fullmatch(r"[0-9a-f]{64}", rows[0]["sha256"]),
            "selected signed Terminal executable unavailable or ambiguous")
    return rows[0]["sha256"]


class OwnedTerminal:
    """One new background tab; an existing Terminal is never an acceptance target."""
    def __init__(self, vm, out):
        self.vm, self.out = vm, out
        state = vm.state()
        require(state.get("chrome", {}).get("session_locked") is False, "unlocked owner state unavailable")
        self.before = topology(state)
        self.old = apps.windows(state, APP)
        self.tab = self.worker = self.door = None

    def adopt(self):
        state = self.vm.state()
        require(state.get("chrome", {}).get("session_locked") is False, "owner session locked")
        require(self.before <= topology(state), "pre-existing window set changed")
        current = apps.windows(state, APP)
        require(self.old <= current, "pre-existing Terminal changed")
        fresh = current - self.old
        require(len(fresh) <= 1, "fresh Terminal association ambiguous")
        if not fresh:
            return None
        tab = next(iter(fresh))
        require(self.tab is None or self.tab == tab, "fresh Terminal replaced")
        self.tab = tab
        worker = apps.native_subject(state, tab, APP)
        if worker is None:
            return None
        require(self.worker is None or self.worker == worker, "native Terminal lifetime replaced")
        self.worker = dict(worker)
        record = apps.mapped_record(state, tab, APP)
        rows = apps.catalog(self.vm, APP, (APP, tab[0], record), FAMILY)
        candidates = [row for row in terminal_rows(rows) if row.get("operation") == "inspect-session"]
        if not candidates:
            return None
        require(len(candidates) == 1, "fresh Terminal Inspector ambiguous")
        self.door = apps.AppDoor(self.vm, APP, FAMILY, identity(candidates[0]), self.out,
                                 (FAMILY, FAMILY), subject=self.worker)
        return self.door

    def open(self):
        rows = [row for row in self.vm.actions("tab.open-beside")
                if row.get("provider") == "actions" and row.get("id") == "tab.open-beside"]
        require(len(rows) == 1, "normal background tab Open unavailable")
        state = self.vm.state()
        require(state.get("chrome", {}).get("session_locked") is False
                and topology(state) == self.before and apps.windows(state, APP) == self.old,
                "owner/window state changed before launch")
        # Single dispatch, never retry an accepted launch.
        invoke(self.vm, rows[0], {"app": APP, "background": True}, self.out, "open-terminal", require_job=False)
        require(wait_until(self.adopt, 60), "new Terminal did not publish its exact scoped Inspector")
        return self.door

    def guard(self):
        state = self.vm.state()
        require(state.get("chrome", {}).get("session_locked") is False, "owner session locked")
        require(self.before <= topology(state), "pre-existing window set changed")
        require(self.tab is not None and self.worker is not None
                and apps.native_subject(state, self.tab, APP) == self.worker,
                "owned Terminal worker association changed")
        return state

    def close(self):
        if self.tab is None:
            return {"closed": False, "reason": "no observed owned tab"}
        state = self.vm.state()
        if (self.tab[0], self.tab[1]) not in topology(state):
            require(self.before <= topology(state), "pre-existing window set changed")
            return {"closed": True, "already_absent": True}
        state = self.guard()
        groups = [str(group["id"]) for workspace in state["workspace_tree"]["workspaces"]
                  if str(workspace["room"]) == self.tab[0] for strip in workspace["strips"]
                  for group in strip["window_groups"] if any(str(tab["id"]) == self.tab[1] for tab in group["tabs"])]
        require(len(groups) == 1, "owned Terminal group ambiguous")
        expected = "tab.close:" + groups[0] + ":" + self.tab[1]
        rows = [row for row in self.vm.actions(expected)
                if row.get("provider") == "desktop-windows" and row.get("id") == expected]
        require(len(rows) == 1, "exact owned tab close unavailable")
        self.guard()  # Fresh authorization/lifetime check immediately before effect.
        invoke(self.vm, rows[0], {}, self.out, "close-owned-terminal", require_job=False)
        def absent():
            current = self.vm.state()
            require(self.before <= topology(current), "pre-existing window set changed during cleanup")
            return True if (self.tab[0], self.tab[1]) not in topology(current) else None
        require(wait_until(absent, 30), "owned Terminal close did not complete")
        return {"closed": True, "tab": self.tab, "worker": self.worker}


def marker_returned(content, session, marker):
    require(isinstance(content, dict) and content.get("session") == session
            and content.get("target") == {"kind": "environment"}, "Read changed session/target")
    require(content.get("output_truncated") is False, "output incomplete")
    require(isinstance(content.get("output"), str), "output unavailable")
    lines = content["output"].splitlines()
    hits = [i for i, line in enumerate(lines) if line == marker]
    require(len(hits) <= 1, "duplicate standalone command marker")
    return len(hits) == 1 and any(re.fullmatch(r"\[Default [^\]]*\]\$\s*", line) for line in lines[hits[0]+1:])


def run(args, vm=None):
    """Root may supply its existing authorized OwnerVm; never relabel an Agent."""
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    vm = vm or TiloVm(str(args.binary.resolve()), args.vm, args.slot, out / "commands.log")
    receipt, shown, installed = apps.cohort(vm, args, out)
    layer = shown["slot"]["components"]["apps+runtime"]
    expected_executable = executable(shown, layer)
    owned = OwnedTerminal(vm, out)
    proof = None
    try:
        door = owned.open()
        owned.guard()
        apps.mapped(vm, "terminal", installed, args.uid, out,
                    executable=expected_executable, selected=owned.worker)
        backing = json.loads((out / "terminal-mapped-sdk.json").read_text())
        require(any("/libstd-" in row["path"] for row in backing["processes"][0]["runtime"]),
                "native SDK libstd mapping missing")
        owned.guard()
        info = door.call("inspect-session")
        require(isinstance(info, dict) and info.get("lifecycle") == "open" and info.get("input_available") is True
                and info.get("transport_disconnected") is False and info.get("target") == {"kind": "environment"},
                "fresh Default session not usable")
        session = info["session"]
        marker = "TILO_TERMINAL_COHORT_" + uuid.uuid4().hex.upper()
        door.call("run", {"command": "printf '" + marker + "\\n'", "confirm": True})
        deadline = time.monotonic() + 30
        while not marker_returned(door.call("read"), session, marker):
            require(time.monotonic() < deadline, "standalone marker plus returned prompt absent")
            time.sleep(0.5)
        owned.guard()
        require(first_json(vm.run("--json", "slot", "show"))["slot"] == shown["slot"], "cohort changed during proof")
        proof = {"source": receipt["cohort_source"], "layer": layer,
                 "cohort_receipt_sha256": hashlib.sha256(args.cohort_receipt.read_bytes()).hexdigest(),
                 "vm": args.vm, "slot": args.slot, "session": session, "binding": door.bound,
                 "tab": owned.tab, "worker": owned.worker, "process": backing["processes"][0], "marker": marker,
                 "standalone_output_and_returned_prompt": True,
                 "host": {"available": False, "reason": "no public exact worker-to-device-shell connection binding"},
                 "latency_claim": False}
    finally:
        try:
            cleanup = owned.close()
        except Exception as error:
            (out / "cleanup.json").write_text(json.dumps({"closed": False, "error": str(error)}, indent=2))
            raise
        (out / "cleanup.json").write_text(json.dumps(cleanup, indent=2))
    proof["cleanup"] = cleanup
    (out / "proof.json").write_text(json.dumps(proof, indent=2))
    print(json.dumps(proof))
    return proof


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "cohort-receipt", "out"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("vm", "slot"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--uid", required=True, type=int)
    run(parser.parse_args())
