#!/usr/bin/env python3
"""Measure a running device through tilo-vm, using an isolated Notes page.

Uses the normal semantic app actions and kernel input path. Missing state or
presentation evidence is a failed measurement, never a passing latency result.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import subprocess
import time

from input_latency import summarize_physical_notes_latency
from device_notes_trace import collect_trace


def require_scratch(state, page, count):
    """Refuse input unless the real editor has adopted this exact scratch body."""
    if any(state.get(key) != page for key in
           ("selected_page", "main_page", "editor_binding_page")):
        raise RuntimeError("Notes is not bound to the requested scratch page")
    if (state.get("app") != "so.tilo.notes"
            or state.get("editor_focused") is not True
            or state.get("adopted") is not True
            or state.get("pending_page_events") != 0
            or state.get("body_chars") != count
            or state.get("body_bytes") != count
            or state.get("rendered_body_chars") != count
            or not isinstance(state.get("body_hash"), str)
            or len(state["body_hash"]) != 64
            or any(c not in "0123456789abcdef" for c in state["body_hash"])
            or state.get("body_hash") != state.get("rendered_body_hash")
            or state.get("hash_algorithm") != "blake3"
            or state.get("caret", {}).get("byte_offset") != count):
        raise RuntimeError("Scratch editor focus, body, caret or adoption is unverified")


def require_keyboard_notes(state):
    focus = state["chrome"]["seat_focus"]
    keyboard = focus.get("keyboard") or {}
    surfaces = [tab["tile"]["Foreign"]["surface_id"]
                for workspace in state["workspace_tree"]["workspaces"]
                for strip in workspace["strips"]
                for group in strip["window_groups"] for tab in group["tabs"]
                if tab.get("tile", {}).get("Foreign", {}).get("app_id") == "so.tilo.notes"]
    if (len(surfaces) != 1 or keyboard.get("app_id") != "so.tilo.notes"
            or keyboard.get("root_surface") != surfaces[0]
            or focus.get("routed_keyboard_surface") != surfaces[0]):
        raise RuntimeError("The compositor keyboard is not routed to the unique Notes surface")


class Device:
    def __init__(self, options):
        self.options = options
        self.env = dict(os.environ, TILO_VM=options.device)

    def run(self, arguments):
        result = subprocess.run(
            [self.options.tool, *arguments], env=self.env,
            capture_output=True, text=True, timeout=60)
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or result.stdout.strip())
        return result.stdout

    def control(self, arguments):
        return json.loads(self.run(
            ["control", "--slot", self.options.slot, *arguments]))

    def execute(self, arguments):
        return self.run(["slot", "exec", "--slot", self.options.slot,
                         "--timeout", "30", "--", *arguments])

    def action(self, operation, payload, resource=None):
        deadline = time.monotonic() + 15
        while True:
            catalogue = self.control(["actions", "list", "so.tilo.notes", "--json"])
            rows = [row for row in catalogue["actions"]
                    if row.get("application") == "so.tilo.notes"
                    and row.get("operation") == operation
                    and (resource is None or row.get("resource") == resource
                         or str(row.get("resource", "")).endswith(
                             ":" + resource.encode().hex()))]
            if len(rows) == 1:
                break
            if len(rows) > 1 or time.monotonic() >= deadline:
                raise RuntimeError("No unique live Notes operation: " + operation)
            time.sleep(.25)
        row = rows[0]
        # The fixed benchmark operations carry only a generated scratch title
        # or no parameters. They never accept secrets.
        result = self.control(["actions", "invoke", row["provider"], row["id"],
                               "--args-json", json.dumps(payload)])
        job = result.get("receipt", {}).get("broker_invocation_id")
        if job is None:
            raise RuntimeError("Notes action did not return a trackable job")
        deadline = time.monotonic() + 20
        while True:
            result = self.control(["actions", "status", str(job)])
            update = result["receipt"]["update"]
            if update["state"] == "succeeded":
                return update.get("output", {}).get("value")
            if update["state"] not in ("accepted", "running"):
                raise RuntimeError(
                    f"Notes {operation} job {job} failed: " + json.dumps(update))
            if time.monotonic() >= deadline:
                raise RuntimeError(f"Notes job {job} still pending; no retry was dispatched")
            time.sleep(.1)


def private_write(path, text):
    path.write_text(text)
    path.chmod(0o600)


def capture(device, options):
    if device.control(["state-dump"])["chrome"]["session_locked"]:
        raise RuntimeError("Device session is locked")
    # Use the ordinary New note operation, then independently verify the new
    # identity and title. Never choose an existing page because it looks empty.
    existing = {row["item"] for row in device.control(
        ["read", "so.tilo.notes"])["pages"]}
    title = "Device latency scratch " + datetime.datetime.now(
        datetime.timezone.utc).isoformat()
    device.action("new-page", {"value": title})
    deadline = time.monotonic() + 10
    while True:
        pages = device.control(["read", "so.tilo.notes"])["pages"]
        fresh = [row for row in pages if row["item"] not in existing and row["title"] == title]
        if len(fresh) == 1:
            break
        if time.monotonic() >= deadline:
            raise RuntimeError("Notes did not publish a unique new scratch page")
        time.sleep(.1)
    device.action("open", {}, resource=fresh[0]["item"])
    state = device.action("inspect-editor", {})
    page = state.get("selected_page")
    if not isinstance(page, str) or "notes.page:" + page in existing:
        raise RuntimeError("Notes did not adopt a new scratch page")
    created = device.control(["read", "so.tilo.notes", "notes.page:" + page])
    if created.get("title") != title or created.get("text") != "":
        raise RuntimeError("New scratch title and empty body were not verified")
    require_scratch(state, page, 0)
    log_path = device.execute(["/usr/bin/python3", "-c",
        "import os; print(os.path.join(os.environ['HOME'], "
        "'.local/state/tilo/session-logs/latest.log'))"]).strip()
    states = []
    records = {}
    try:
        for index in range(options.samples):
            require_scratch(device.action("inspect-editor", {}), page, index)
            require_keyboard_notes(device.control(["state-dump"]))
            device.run(["input", "--slot", options.slot, "type", "a", "--json"])
            time.sleep(.5)
            state = device.action("inspect-editor", {})
            require_scratch(state, page, index + 1)
            states.append(state)
            lines = collect_trace(device, log_path, page, records)
            report = summarize_physical_notes_latency(lines, page)
            if len(report["samples"]) != index + 1:
                raise ValueError("Physical trace did not preserve every scratch input")
            private_write(options.out / "timeline.log", "\n".join(lines))
            print(f"Scratch key {index + 1}/{options.samples} adopted", flush=True)
        time.sleep(.5)
    finally:
        private_write(options.out / "states.json", json.dumps(states, indent=2))
        private_write(options.out / "capture.json", json.dumps({
            "device": options.device, "slot": options.slot,
            "scratch_page": page, "samples_requested": options.samples,
            "samples_adopted": len(states), "log_path": log_path,
        }, indent=2))
        private_write(options.out / "timeline.log", "\n".join(records))
    return summarize_physical_notes_latency(list(records), page)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", required=True)
    parser.add_argument("--slot", default="device")
    parser.add_argument("--tool", default="tilo-vm")
    parser.add_argument("--samples", type=int, default=20)
    parser.add_argument("--out", type=Path, required=True)
    options = parser.parse_args()
    if not 1 <= options.samples <= 1000:
        parser.error("samples must be between 1 and 1000")
    options.out.mkdir(parents=True, exist_ok=False, mode=0o700)
    try:
        report = capture(Device(options), options)
    except (RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
        private_write(options.out / "report.json", json.dumps({
            "latency_complete": False, "error": str(error)}, indent=2))
        parser.exit(1, f"Measurement failed: {error}\nEvidence: {options.out}\n")
    private_write(options.out / "report.json", json.dumps(report, indent=2))
    print(json.dumps(report["latency"], indent=2))


if __name__ == "__main__":
    main()
