"""Actual isolated CPU compositor journey; reuses the existing control client."""
import json
import pathlib
import runpy
import sys
import time
sys.path.insert(0, str(pathlib.Path.cwd() / "scripts/headless-examples"))
from tilo_client import TiloClient

out = pathlib.Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
runtime = out / "runtime"
socket = runtime / "editor.sock"
assert len(str(socket).encode()) < 108, "use a shorter private capture directory for the Unix socket"
runtime.mkdir(mode=0o700, exist_ok=True)
png_pixels = runpy.run_path("scripts/headless-examples/full-loop.py")["png_pixels"]


def wait(description, predicate):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(description)


def target(client, suffix):
    return next((t for t in client.list_targets() if t.name.endswith(suffix) and t.rect[2] > 0 and t.rect[3] > 0), None)


def rows(value):
    if isinstance(value, dict):
        if value.get("id") == "topbar.editor.open":
            yield value
        for child in value.values():
            yield from rows(child)
    elif isinstance(value, list):
        for child in value:
            yield from rows(child)


def command(client, payload):
    value = client.raw(payload)
    assert "err" not in value, value
    return value


def capture(client, name):
    path = out / (name + ".png")
    client.snapshot(path)
    width, height, channels, pixels = png_pixels(path)
    assert channels == 4 and len(set(pixels)) > 4
    return path, (width, height, channels, pixels)


spawned = TiloClient.spawn_headless(socket_path=socket, wait_seconds=30)
with spawned as client:
    catalogue = command(client, {"op": "actions_enumerate", "query": "Edit top bar"})
    candidates = list(rows(catalogue))
    assert candidates, catalogue
    action = candidates[0]
    assert action["title"] == "Edit top bar", action
    assert action["subtitle"] == "Reorder, add or remove top-bar widgets", action
    provider = action.get("provider") or action.get("provider_id")
    assert provider
    receipt = {"catalogue": action, "steps": []}

    def open_editor():
        value = command(client, {"op": "actions_invoke", "provider": provider, "id": action["id"]})
        wait("semantic open must paint editor", lambda: target(client, "topbar_editor_remove_system_monitor"))
        receipt["steps"].append({"semantic_open": value})

    open_editor()
    opened, open_pixels = capture(client, "live-open")
    remove = target(client, "topbar_editor_remove_system_monitor")
    x, y = remove.center
    command(client, {"op": "pointer_move", "x": x, "y": y})
    command(client, {"op": "pointer_button", "pressed": True})
    assert target(client, "topbar_editor_remove_system_monitor"), "a press cannot remove"
    capture(client, "live-pressed")
    command(client, {"op": "pointer_move", "x": 2, "y": 450})
    command(client, {"op": "pointer_button", "pressed": False})
    assert target(client, "topbar_editor_remove_system_monitor"), "outside release cancels"

    client.click(remove.name)
    wait("release removes Monitor", lambda: target(client, "topbar_editor_add_system_monitor"))
    assert not target(client, "topbar_editor_remove_system_monitor")
    capture(client, "live-add-focus")
    client.key("Enter")
    wait("fresh Enter adds Monitor", lambda: target(client, "topbar_editor_remove_system_monitor"))
    client.key("Escape")
    wait("Esc closes editor", lambda: not target(client, "topbar_editor_remove_clock"))
    closed, closed_pixels = capture(client, "live-closed")
    assert open_pixels[:3] == closed_pixels[:3]
    assert sum(a != b for a, b in zip(open_pixels[3], closed_pixels[3])) > 10000, "actual editor pixels must disappear"

    open_editor()
    remove = target(client, "topbar_editor_remove_system_monitor")
    client.click(remove.name)
    wait("Monitor removed for held-key test", lambda: target(client, "topbar_editor_add_system_monitor"))
    command(client, {"op": "key_down", "key": "Enter"})
    wait("held Enter adds once", lambda: target(client, "topbar_editor_remove_system_monitor"))
    for _ in range(4):
        command(client, {"op": "key_down", "key": "Enter"})
    assert target(client, "topbar_editor_remove_system_monitor"), "held input cannot remove again"
    client.key("Escape")
    command(client, {"op": "key_up", "key": "Enter"})
    assert not target(client, "topbar_editor_remove_clock"), "late release cannot reopen editor"

    open_editor()
    before = target(client, "topbar_editor_remove_clock").rect[1]
    clock = target(client, "topbar_editor_remove_clock")
    calendar = target(client, "topbar_editor_remove_calendar")
    command(client, {"op": "pointer_move", "x": clock.rect[0] - 60, "y": clock.center[1]})
    command(client, {"op": "pointer_button", "pressed": True})
    command(client, {"op": "pointer_move", "x": clock.rect[0] - 60, "y": calendar.center[1]})
    client.key("Escape")
    command(client, {"op": "pointer_button", "pressed": False})
    open_editor()
    assert target(client, "topbar_editor_remove_clock").rect[1] == before, "Esc must cancel drag before release"
    client.key("Escape")
    assert not client.state_dump()["chrome"]["overlays"]["overview"]
    receipt["steps"].append("pointer cancel/remove; keyboard re-add; held submit/Esc/release; drag/Esc/reopen")
    (out / "journey.json").write_text(json.dumps(receipt, indent=2))

assert spawned._proc.poll() is not None, "owned compositor must terminate"
assert not socket.exists(), "owned control socket must disappear"
assert spawned._profile is not None and not spawned._profile.exists(), "owned scratch profile must disappear"
runtime.rmdir()
print("PASS semantic/pointer/keyboard/cancel/repeat/drag journey; actual open/closed pixels; cleanup")
