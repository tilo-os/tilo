#!/usr/bin/env python3
"""Exercise packaged Tilo apps through the real AT-SPI D-Bus endpoint.

This is deliberately an *external* client: it asks the running Tilo session
to open an installed app the way a screen-reader user does (the compositor's
"Open an app" palette, over AT-SPI), finds the app on the accessibility bus,
walks exported objects, and invokes an action using the public AT-SPI
interfaces.  The app therefore starts exactly as a user's would: through the
launcher, inside its sandbox, attached to the session's runtime host.  The
client does not import the UI crate or inspect an in-process AccessKit tree.

``open APP_NAME`` performs only the session launch and prints the app's
AT-SPI bus name and process id, for gates with their own checks. Such a gate
then drives the opened app by that bus name: ``names BUS`` prints every
accessible name in its tree, ``type BUS TEXT`` sets its first editable text
field, and ``click BUS NAME`` invokes the click action of the node named NAME
once it appears.

``page`` opens Browser the same way and serves a page from this machine. Once
the address field holds keyboard focus it prints ``ready URL``; the caller
types that address on the real keyboard. The check passes when Browser's engine fetched
the page and Browser names the loaded document after its title.

``settings-network`` opens Settings' Network pane and requires what it reads
out to match ``nmcli`` on the same device: the connected Wi-Fi network, the
wired link, NetworkManager's connectivity verdict, and no read error.

``new-tab APP_NAME`` proves tabs are the desktop's: it opens the app, prints
``READY``, and waits for the person (the gate's real keyboard) to press
Ctrl+T. The app must answer with another instance of itself, in a new
desktop tab beside the first — one window group grows by exactly one tab
and a second process of the app joins the bus. Apps have no tab strips of
their own.

``osk-search FIFO`` proves the on-screen keyboard on a touchscreen: it opens
the app palette, taps its search field through the root touchscreen helper
serving FIFO (``input-hotplug-check.sh touchscreen``), requires the keyboard
service to start and stay running, taps three of its letter keys and reads the
typed letters back from the field.

The small ``[accessibility.atspi_acceptance]`` section in a signed app
manifest is the certification data.  Adding an app is data, not another
one-off screen-reader script.  Scenarios are intentionally limited to a safe
navigation/action and a visible post-action name; hardware, credentials, and
network journeys remain product-specific acceptance work.
"""

from __future__ import annotations

import argparse
import html
import http.server
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
import tomllib
from dataclasses import dataclass


ROOT_PATH = "/org/a11y/atspi/accessible/root"
# AccessKit exports /org/a11y/...; GTK namespaces UUID keys under its app id.
# Parse the quoted D-Bus object paths, without assuming a provider's prefix.
PATH_RE = re.compile(r"'(/[A-Za-z0-9_/]+)'")
POLL_SECONDS = 0.05
START_TIMEOUT_SECONDS = 20
TREE_NODE_LIMIT = 128
# A catalogue window (the App Store) exposes a page of cards and their
# controls; walking it whole needs more than one app scenario's limit.
APP_TREE_NODE_LIMIT = 2048
SHELL_TREE_NODE_LIMIT = 1024
# The session's compositor owns this backend name; it identifies the shell's
# AT-SPI connection without reading another process's /proc entry.
SHELL_BUS_NAME = "org.freedesktop.impl.portal.desktop.tilo"
OPEN_APP_ACTION = "Open an app"
# AT-SPI role numbers (AtspiRole) the gate tells apart.
ROLE_PAGE_TAB = 37
ROLE_PAGE_TAB_LIST = 38
# Document frame, spreadsheet, presentation, text, web and email.
DOCUMENT_ROLES = frozenset({82, 92, 93, 94, 95, 96})
LAUNCH_TIMEOUT_SECONDS = 120
PAGE_TITLE = "Tilo browser engine check"
# Typing the address and starting the engine, under emulation on some lanes.
PAGE_TIMEOUT_SECONDS = 300
BROWSER_TREE_NODE_LIMIT = 512


class GateError(RuntimeError):
    """A release-gate failure with enough context for a maintainer to act."""


@dataclass(frozen=True)
class Scenario:
    app_id: str
    app_name: str
    binary: str
    root: str
    required: tuple[str, ...]
    action: str
    after_action: tuple[str, ...]


def command(*args: str, check: bool = True) -> str:
    completed = subprocess.run(args, text=True, capture_output=True, check=False)
    if check and completed.returncode:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise GateError(f"{' '.join(args[:5])} failed: {detail}")
    return completed.stdout


def atspi_address() -> str:
    output = command(
        "gdbus", "call", "--session", "--dest", "org.a11y.Bus",
        "--object-path", "/org/a11y/bus", "--method", "org.a11y.Bus.GetAddress",
    )
    match = re.search(r"'([^']+)'", output)
    if not match:
        raise GateError(f"org.a11y.Bus returned no AT-SPI address: {output.strip()}")
    return match.group(1)


def scenario_from_manifest(path: Path) -> Scenario | None:
    with path.open("rb") as source:
        document = tomllib.load(source)
    accessibility = document.get("accessibility", {})
    spec = accessibility.get("atspi_acceptance") if isinstance(accessibility, dict) else None
    if not isinstance(spec, dict):
        return None
    package = document.get("package", {})
    entrypoint = document.get("entrypoint", {})
    values = {
        "app_id": package.get("id"),
        "app_name": package.get("name"),
        "binary": entrypoint.get("bin"),
        "root": spec.get("root"),
        "action": spec.get("action"),
    }
    required = spec.get("required", [])
    after_action = spec.get("after_action", [])
    if not all(isinstance(value, str) and value for value in values.values()):
        raise GateError(f"{path}: incomplete [accessibility.atspi_acceptance] data")
    if not isinstance(required, list) or not isinstance(after_action, list):
        raise GateError(f"{path}: required and after_action must be string arrays")
    if not all(isinstance(value, str) and value for value in required + after_action):
        raise GateError(f"{path}: required and after_action must be string arrays")
    if not required or not after_action:
        raise GateError(f"{path}: require non-empty required and after_action evidence")
    return Scenario(
        app_id=values["app_id"], app_name=values["app_name"], binary=values["binary"],
        root=values["root"], required=tuple(required), action=values["action"],
        after_action=tuple(after_action),
    )


def installed_scenarios(payload_root: Path) -> list[tuple[Scenario, Path]]:
    scenarios: list[tuple[Scenario, Path]] = []
    for manifest in sorted(payload_root.glob("*/manifest.toml")):
        scenario = scenario_from_manifest(manifest)
        if scenario:
            # package-store keeps signed metadata in <hash>.pkg and the
            # materialised executable tree beside it in <hash>.
            payload = manifest.parent
            if payload.name.endswith(".pkg"):
                if not re.fullmatch(r"[0-9a-f]{64}\.pkg", payload.name):
                    raise GateError(f"invalid installed package metadata directory: {payload}")
                payload = payload.with_name(payload.name[:-4])
            binary = payload / scenario.binary
            if not binary.is_file() or not os.access(binary, os.X_OK):
                raise GateError(f"{scenario.app_id}: certified binary is absent: {binary}")
            scenarios.append((scenario, binary))
    if not scenarios:
        raise GateError(
            f"no [accessibility.atspi_acceptance] scenarios under {payload_root}; "
            "the installed image lost its core-app certification data"
        )
    return scenarios


class AtspiClient:
    def __init__(self, address: str, bus_name: str):
        self.address = address
        self.bus_name = bus_name

    def call(self, path: str, interface_method: str, *args: str, check: bool = True) -> str:
        return command(
            "gdbus", "call", "--address", self.address, "--dest", self.bus_name,
            "--object-path", path, "--method", interface_method, *args, check=check,
        )

    def role(self, path: str) -> int | None:
        """The node's AT-SPI role number (AtspiRole). AccessKit answers
        Accessible.GetRole but has no role-name method, so roles are compared
        by number."""
        reply = self.call(path, "org.a11y.atspi.Accessible.GetRole", check=False)
        match = re.search(r"uint32\s+(\d+)", reply)
        return int(match.group(1)) if match else None

    def property(self, path: str, name: str) -> str:
        return self.call(
            path, "org.freedesktop.DBus.Properties.Get", "org.a11y.atspi.Accessible", name,
        )

    def tree(self, limit: int = TREE_NODE_LIMIT) -> dict[str, str]:
        pending = [ROOT_PATH]
        seen: set[str] = set()
        result: dict[str, str] = {}
        while pending and len(seen) < limit:
            path = pending.pop(0)
            if path in seen:
                continue
            seen.add(path)
            try:
                result[path] = self.property(path, "Name")
                children = self.call(path, "org.a11y.atspi.Accessible.GetChildren")
            except GateError:
                # AccessKit can replace its initial placeholder transaction.
                # The caller retries a complete bounded walk instead of treating
                # one stale object as an empty successful tree.
                raise
            for child in PATH_RE.findall(children):
                if child not in seen:
                    pending.append(child)
        return result

    @staticmethod
    def named(tree: dict[str, str], expected: str) -> list[str]:
        needle = f"'{expected}'"
        return [path for path, name in tree.items() if needle in name]

    def interfaces(self, path: str) -> str:
        return self.call(path, "org.a11y.atspi.Accessible.GetInterfaces", check=False)

    def set_text(self, path: str, value: str) -> bool:
        result = self.call(path, "org.a11y.atspi.EditableText.SetTextContents", value, check=False)
        return "true" in result

    def clickable(self, tree: dict[str, str], expected: str) -> list[str]:
        """Paths of the nodes named EXPECTED that offer AT-SPI's click action."""
        return [
            path for path in self.named(tree, expected)
            if "'click'" in self.call(path, "org.a11y.atspi.Action.GetActions", check=False)
        ]

    def click(self, tree: dict[str, str], expected: str) -> None:
        for path in self.clickable(tree, expected):
            result = self.call(path, "org.a11y.atspi.Action.DoAction", "0")
            if "true" not in result:
                raise GateError(f"AT-SPI action {expected!r} was rejected: {result.strip()}")
            return
        raise GateError(f"AT-SPI tree has no click-capable node named {expected!r}")


# Below its breakpoint an app built on the shared shell shows one screen at a
# time: it opens on the selected pane, with a Back arrow to its navigation list
# (crates/sdk/ui-core/src/widget/app_shell.rs). A tiled desktop window is often
# that narrow. Someone who cannot see an entry there goes back to the list once.
NARROW_BACK = "Back"


def wait_for_reachable(
    client: AtspiClient,
    names: tuple[str, ...],
    subject: str,
    limit: int = TREE_NODE_LIMIT,
    clickable: bool = False,
) -> dict[str, str]:
    """The app's tree once it exposes every name in NAMES (as something that
    can be clicked, with CLICKABLE), going back from a narrow window's pane to
    its list once if that is where they are."""
    find = client.clickable if clickable else client.named
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    latest: dict[str, str] = {}
    went_back = False
    while True:
        try:
            latest = client.tree(limit)
            if all(find(latest, name) for name in names):
                return latest
            if not went_back and client.clickable(latest, NARROW_BACK):
                client.click(latest, NARROW_BACK)
                went_back = True
                # The list replaces the pane; give the deadline back for it.
                deadline = time.monotonic() + START_TIMEOUT_SECONDS
        except GateError:
            # AccessKit can replace a subtree while it is walked; walk again.
            pass
        if time.monotonic() >= deadline:
            break
        time.sleep(POLL_SECONDS * 4)
    observed = "\n".join(sorted({name.strip() for name in latest.values() if name.strip()}))
    raise GateError(
        f"{subject}: tree never exposed {'click-capable ' if clickable else ''}"
        f"{', '.join(names)}{' (also not after going Back)' if went_back else ''}; "
        f"observed AT-SPI names:\n{observed}"
    )


def bus_pids(address: str) -> dict[str, int]:
    """Unique AT-SPI connections and the process behind each."""
    result: dict[str, int] = {}
    for line in command("busctl", f"--address={address}", "list", "--no-pager", check=False).splitlines():
        fields = line.split()
        if len(fields) > 1 and fields[0].startswith(":") and fields[1].isdigit():
            result[fields[0]] = int(fields[1])
    return result


def shell_client(address: str) -> AtspiClient:
    output = command(
        "busctl", "--user", "call", "org.freedesktop.DBus", "/org/freedesktop/DBus",
        "org.freedesktop.DBus", "GetConnectionUnixProcessID", "s", SHELL_BUS_NAME,
    )
    pid = int(output.split()[-1])
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        for bus, owner in bus_pids(address).items():
            if owner == pid:
                return AtspiClient(address, bus)
        time.sleep(POLL_SECONDS)
    raise GateError(f"the Tilo compositor (pid {pid}) is not on the AT-SPI bus")


def shell_tree(shell: AtspiClient) -> dict[str, str]:
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while True:
        try:
            return shell.tree(SHELL_TREE_NODE_LIMIT)
        except GateError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(POLL_SECONDS)


def wait_for_shell_node(shell: AtspiClient, name: str, *, editable: bool = False) -> tuple[dict[str, str], str]:
    deadline = time.monotonic() + LAUNCH_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        tree = shell_tree(shell)
        for path in AtspiClient.named(tree, name):
            if editable:
                if "org.a11y.atspi.EditableText" in shell.interfaces(path):
                    return tree, path
            elif "'click'" in shell.call(path, "org.a11y.atspi.Action.GetActions", check=False):
                return tree, path
        time.sleep(POLL_SECONDS * 4)
    kind = "editable" if editable else "actionable"
    names = sorted({value.strip() for value in shell_tree(shell).values() if value.strip()})
    raise GateError(
        f"the session exposed no {kind} {name!r} over AT-SPI; it exposed: {', '.join(names)}"
    )


def app_process(pid: int) -> int:
    """The app behind an AT-SPI connection. A sandboxed app reaches the bus
    through the launcher's filtered proxy, so the bus reports the proxy's pid;
    the app is the launcher's descendant that is neither a proxy nor bwrap."""
    def comm(process: int) -> str:
        try:
            return Path(f"/proc/{process}/comm").read_text().strip()
        except OSError:
            return ""

    if comm(pid) != "xdg-dbus-proxy":
        return pid
    try:
        launcher = int(Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[1])
    except (OSError, IndexError, ValueError):
        return pid
    children: dict[int, list[int]] = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            parent = int((entry / "stat").read_text().rsplit(")", 1)[1].split()[1])
        except (OSError, IndexError, ValueError):
            continue
        children.setdefault(parent, []).append(int(entry.name))
    pending = list(children.get(launcher, []))
    while pending:
        process = pending.pop(0)
        if comm(process) not in ("xdg-dbus-proxy", "bwrap"):
            return process
        pending.extend(children.get(process, []))
    return pid


def process_command(pid: int) -> str:
    try:
        return Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace").strip()[:160]
    except OSError:
        return "(exited)"


def app_log(app_name: str) -> str:
    """Why a launch failed, for a failed open. The compositor records each
    launch and the app's own output in its session log, not the journal."""
    session_log = Path.home() / ".local/state/tilo/session-logs/latest.log"
    try:
        lines = session_log.read_text(errors="replace").splitlines()[-2000:]
    except OSError as error:
        return f"(no session log: {error})"
    needles = ("tilo::app_launch", "[app so.tilo.", app_name.lower())
    relevant = [line for line in lines if any(n in line.lower() for n in needles)]
    return " | ".join(line[:300] for line in relevant[-12:]) or "(nothing)"


def is_app_connection(client: AtspiClient) -> bool:
    """Whether a connection that appeared after the launch is an application:
    it exports an application node. Clients such as gdbus or busctl do not.
    The application node is named after the executable, not the display
    name, so the launch itself is what identifies the app."""
    try:
        name = client.property(ROOT_PATH, "Name")
    except GateError:
        return False
    return re.search(r"'[^']+'", name) is not None


def open_in_session(address: str, app_name: str) -> tuple[str, int]:
    """Open APP_NAME through the compositor's app palette; return its bus and pid."""
    shell = shell_client(address)
    before = set(bus_pids(address))
    _, opener = wait_for_shell_node(shell, OPEN_APP_ACTION)
    result = shell.call(opener, "org.a11y.atspi.Action.DoAction", "0")
    if "true" not in result:
        raise GateError(f"AT-SPI {OPEN_APP_ACTION!r} was rejected: {result.strip()}")
    # The palette's query is an ordinary editable text field for AT.
    deadline = time.monotonic() + LAUNCH_TIMEOUT_SECONDS
    while True:
        tree = shell_tree(shell)
        fields = [
            path for path in tree
            if "org.a11y.atspi.EditableText" in shell.interfaces(path)
            and "Command palette" not in tree[path]
        ]
        if any(shell.set_text(path, app_name) for path in fields):
            break
        if time.monotonic() >= deadline:
            raise GateError("the app palette exposed no editable query over AT-SPI")
        time.sleep(POLL_SECONDS * 4)
    # The palette names an app's launch row "Open <app>".
    row_name = f"Open {app_name}"
    tree, _ = wait_for_shell_node(shell, row_name)
    rows = [
        path for path in AtspiClient.named(tree, row_name)
        if "'click'" in shell.call(path, "org.a11y.atspi.Action.GetActions", check=False)
    ]
    for row in rows:
        result = shell.call(row, "org.a11y.atspi.Action.DoAction", "0", check=False)
        if "true" not in result:
            continue
        # One accepted activation launches the app; wait for it rather than
        # launching a second copy from another row.
        return wait_for_new_app(address, before, app_name)
    raise GateError(f"no {row_name!r} row accepted its activation; session log: {app_log(app_name)}")


def wait_for_new_app(address: str, before: set[str], app_name: str) -> tuple[str, int]:
    """The app connection that joined the bus after BEFORE was taken."""
    deadline = time.monotonic() + LAUNCH_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        for bus, pid in bus_pids(address).items():
            if bus in before:
                continue
            if is_app_connection(AtspiClient(address, bus)):
                return bus, app_process(pid)
        time.sleep(POLL_SECONDS * 4)
    seen = []
    for bus, pid in sorted(bus_pids(address).items()):
        if bus in before:
            continue
        process = app_process(pid)
        try:
            name = AtspiClient(address, bus).property(ROOT_PATH, "Name").strip()
        except GateError as error:
            name = f"no application node: {error}"
        seen.append(f"{bus} (pid {pid}, process {process} {process_command(process)}): {name}")
    raise GateError(
        f"{app_name!r} was opened but never appeared on the AT-SPI bus; "
        f"new connections: {'; '.join(seen) or 'none'}; "
        f"session log: {app_log(app_name)}"
    )


def open_from_launchpad_card(
    address: str, card: str, app_name: str, expected: tuple[str, ...] = (),
) -> tuple[str, int]:
    """Activate the empty workspace's CARD and require that it opens APP_NAME
    itself. A card that only pre-fills the app launcher starts nothing, so
    no app joins the bus and this fails. EXPECTED names nodes the app must
    then expose: what the card's action did in it, such as the screen it
    opened on or the call it started."""
    shell = shell_client(address)
    before = set(bus_pids(address))
    _, node = wait_for_shell_node(shell, card)
    result = shell.call(node, "org.a11y.atspi.Action.DoAction", "0")
    if "true" not in result:
        raise GateError(f"AT-SPI launchpad card {card!r} was rejected: {result.strip()}")
    bus, pid = wait_for_new_app(address, before, app_name)
    # The application node is named after the executable; a sandboxed one is
    # behind its proxy, so its command line names it too.
    root_name = AtspiClient(address, bus).property(ROOT_PATH, "Name")
    if app_name.lower() not in f"{root_name} {process_command(pid)}".lower():
        close_app(pid)
        raise GateError(
            f"launchpad card {card!r} opened {root_name.strip()} "
            f"({process_command(pid)}), not {app_name}"
        )
    if expected:
        try:
            wait_for_tree(AtspiClient(address, bus), expected, app_name)
        except GateError as error:
            close_app(pid)
            raise GateError(f"launchpad card {card!r} opened {app_name} but not what it names: {error}") from error
    return bus, pid


def app_names(address: str, bus: str) -> list[str]:
    """Every accessible name in an opened app's tree, in walk order."""
    client = AtspiClient(address, bus)
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while True:
        try:
            tree = client.tree(APP_TREE_NODE_LIMIT)
            break
        except GateError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(POLL_SECONDS)
    names = []
    for value in tree.values():
        match = re.search(r"'((?:[^'\\]|\\.)*)'", value)
        if match and match.group(1).strip():
            names.append(match.group(1))
    return names


def click_in_app(address: str, bus: str, name: str, timeout: float) -> None:
    """Click the node named NAME in an opened app once it is exposed."""
    client = AtspiClient(address, bus)
    deadline = time.monotonic() + timeout
    last_error = GateError(f"no node named {name!r} appeared")
    while time.monotonic() < deadline:
        try:
            client.click(client.tree(APP_TREE_NODE_LIMIT), name)
            return
        except GateError as error:
            last_error = error
        time.sleep(POLL_SECONDS * 4)
    raise last_error


def type_in_app(address: str, bus: str, text: str) -> None:
    """Set the first editable text field of an opened app to TEXT."""
    client = AtspiClient(address, bus)
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        tree = client.tree(APP_TREE_NODE_LIMIT)
        for path in tree:
            if "org.a11y.atspi.EditableText" in client.interfaces(path) and client.set_text(path, text):
                return
        time.sleep(POLL_SECONDS * 4)
    raise GateError("the app exposed no editable text field over AT-SPI")


def tab_lists(shell: AtspiClient) -> list[list[str]]:
    """The desktop's tab strips, in tree order: each strip's tab names. A
    window group's strip is an AT-SPI page tab list."""
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while True:
        try:
            tree = shell.tree(SHELL_TREE_NODE_LIMIT)
            lists: list[list[str]] = []
            for path in tree:
                if shell.role(path) != ROLE_PAGE_TAB_LIST:
                    continue
                children = shell.call(path, "org.a11y.atspi.Accessible.GetChildren")
                tabs = []
                for child in PATH_RE.findall(children):
                    if shell.role(child) == ROLE_PAGE_TAB:
                        tabs.append(tree.get(child, shell.property(child, "Name")).strip())
                lists.append(tabs)
            return lists
        except GateError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(POLL_SECONDS)


def new_tab(address: str, app_name: str) -> None:
    """Open APP_NAME, then require that the Ctrl+T the gate presses opens a
    second instance of it as a tab beside the first, in the same group."""
    _bus, first = open_in_session(address, app_name)
    pids = [first]
    try:
        shell = shell_client(address)
        before_tabs = tab_lists(shell)
        before_bus = set(bus_pids(address))
        print("READY", flush=True)
        _second_bus, second = wait_for_new_app(address, before_bus, app_name)
        pids.append(second)
        if app_name.lower() not in process_command(second).lower():
            raise GateError(
                f"Ctrl+T in {app_name} started {process_command(second)}, not another {app_name}"
            )
        deadline = time.monotonic() + LAUNCH_TIMEOUT_SECONDS
        after_tabs = tab_lists(shell)
        while time.monotonic() < deadline:
            grown = [
                (old, new) for old, new in zip(before_tabs, after_tabs)
                if len(new) == len(old) + 1
            ]
            if len(after_tabs) == len(before_tabs) and len(grown) == 1:
                old, new = grown[0]
                print(
                    f"AT-SPI external client passed: Ctrl+T in {app_name} opened a desktop tab "
                    f"beside it ({old} -> {new})"
                )
                return
            time.sleep(POLL_SECONDS * 4)
            after_tabs = tab_lists(shell)
        raise GateError(
            f"Ctrl+T in {app_name} started a second instance, but no window group gained "
            f"exactly one tab: before {before_tabs}, after {after_tabs}"
        )
    finally:
        for pid in pids:
            close_app(pid)


def close_app(pid: int) -> None:
    """End the app the session opened, so the next scenario starts clean."""
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(POLL_SECONDS)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def wait_for_tree(client: AtspiClient, names: tuple[str, ...], subject: str) -> dict[str, str]:
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    latest: dict[str, str] = {}
    while time.monotonic() < deadline:
        try:
            latest = client.tree()
        except GateError:
            time.sleep(POLL_SECONDS)
            continue
        if all(client.named(latest, expected) for expected in names):
            return latest
        time.sleep(POLL_SECONDS)
    observed = "\n".join(sorted(set(latest.values())))
    raise GateError(
        f"{subject}: tree never exposed {', '.join(names)}; "
        f"observed AT-SPI names:\n{observed}"
    )


def exercise(scenario: Scenario, address: str) -> None:
    bus_name, pid = open_in_session(address, scenario.app_name)
    try:
        client = AtspiClient(address, bus_name)
        initial = wait_for_reachable(client, (scenario.root, *scenario.required), scenario.app_id)
        client.click(initial, scenario.action)
        wait_for_tree(client, scenario.after_action, scenario.app_id)
    except GateError as error:
        raise GateError(f"{scenario.app_id}: {error}") from error
    finally:
        close_app(pid)
    print(f"AT-SPI external client passed: {scenario.app_id} ({scenario.action} action round trip)")


def serve_page(title: str) -> tuple[http.server.ThreadingHTTPServer, str, list[str]]:
    """Serve one page on loopback; return the server, its URL and the paths requested."""
    escaped = html.escape(title)
    body = (
        f"<!doctype html><html><head><meta charset=utf-8><title>{escaped}</title></head>"
        f"<body><h1>{escaped}</h1><p>Served from this machine.</p></body></html>"
    ).encode()
    requests: list[str] = []

    class Page(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802 - the stdlib's handler name
            requests.append(self.path)
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format: str, *args: object) -> None:  # noqa: A002
            return

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Page)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_address[1]}/", requests


def wait_for_names(client: AtspiClient, names: tuple[str, ...], timeout: float) -> dict[str, str]:
    deadline = time.monotonic() + timeout
    latest: dict[str, str] = {}
    while time.monotonic() < deadline:
        try:
            latest = client.tree(BROWSER_TREE_NODE_LIMIT)
        except GateError:
            time.sleep(POLL_SECONDS)
            continue
        if all(client.named(latest, name) for name in names):
            return latest
        time.sleep(POLL_SECONDS * 4)
    observed = ", ".join(sorted({value.strip() for value in latest.values() if value.strip()}))
    raise GateError(f"Browser never exposed {', '.join(names)}; it exposed: {observed}")


ADDRESS_FIELD = "Address and search"


def wait_for_keyboard_focus(client: AtspiClient, field: str, timeout: float) -> None:
    """Give the editable FIELD keyboard focus and wait until it has it.

    An app publishes its accessibility tree from its first layout, but the
    compositor maps its window, and hands it the keyboard, only once its first
    frame is presented. On a software-rendered VM that is seconds later, and
    keys pressed in between land on whatever had focus before: an address
    typed then reaches Browser with its first characters missing, and
    ``tp://…`` is no web address. AccessKit reports a node focused only while
    its window holds keyboard focus, so the focused state is the signal that
    real key presses will arrive."""
    deadline = time.monotonic() + timeout
    target = None
    while True:
        if target is None:
            try:
                tree = client.tree(BROWSER_TREE_NODE_LIMIT)
            except GateError:
                tree = {}
            target = next(
                (path for path, name in tree.items()
                 if is_typing_target(client, path, name, field)),
                None,
            )
            if target is not None:
                client.call(target, "org.a11y.atspi.Component.GrabFocus", check=False)
        if target is not None:
            reply = client.call(target, "org.a11y.atspi.Accessible.GetState", check=False)
            word = re.search(r"uint32\s+(\d+)", reply)
            if word and int(word.group(1)) & FOCUSED_STATE_BIT:
                return
        if time.monotonic() >= deadline:
            what = "never appeared" if target is None else "never took keyboard focus"
            raise GateError(f"Browser's editable {field!r} {what}; "
                            f"session log: {app_log('Browser')}")
        time.sleep(POLL_SECONDS * 4)


def load_page(address: str, title: str) -> str:
    """Open Browser, hand the address to the caller's keyboard, and require the
    engine to load the page this process serves."""
    server, url, requests = serve_page(title)
    try:
        bus_name, pid = open_in_session(address, "Browser")
        try:
            client = AtspiClient(address, bus_name)
            wait_for_names(client, (ADDRESS_FIELD,), START_TIMEOUT_SECONDS * 3)
            wait_for_keyboard_focus(client, ADDRESS_FIELD, START_TIMEOUT_SECONDS * 3)
            print(f"ready {url}", flush=True)
            try:
                wait_for_names(client, (title,), PAGE_TIMEOUT_SECONDS)
            except GateError as error:
                raise GateError(
                    f"{error}; the page server saw {requests or 'no request'}; "
                    f"session log: {app_log('Browser')}"
                ) from error
            if not requests:
                raise GateError(f"Browser showed {title!r} without fetching {url}")
            return url
        finally:
            close_app(pid)
    finally:
        server.shutdown()


# Settings' English labels for NetworkManager's connectivity verdicts.
CONNECTIVITY_LABELS = {
    "full": "Connected to the internet",
    "limited": "Limited connectivity",
    "portal": "Sign-in required",
    "none": "Offline",
    "unknown": "Connectivity not checked",
}
SETTINGS_APP_NAME = "Personal Settings"
NETWORK_PANE = "Network"
NETWORK_TIMEOUT_SECONDS = 45
# Settings words a failed host read "Can't …"; none may appear when nmcli
# itself answers on the same device.
READ_ERROR_MARKERS = ("Can't", "Can\u2019t")


def split_terse(line: str) -> list[str]:
    """Split one ``nmcli -t`` row on unescaped colons."""
    fields, current, escaped = [], [], False
    for char in line:
        if escaped:
            current.append(char)
            escaped = False
        elif char == "\\":
            escaped = True
        elif char == ":":
            fields.append("".join(current))
            current = []
        else:
            current.append(char)
    fields.append("".join(current))
    return fields


@dataclass(frozen=True)
class NetworkExpectation:
    connectivity: str
    wifi: str | None
    wired: bool


def network_expectation(devices: str, connectivity: str) -> NetworkExpectation:
    """What Settings must show, from ``nmcli -t -f TYPE,STATE,CONNECTION
    device status`` and ``nmcli -t networking connectivity``."""
    wifi = None
    wired = False
    for line in devices.splitlines():
        fields = split_terse(line)
        if len(fields) < 3 or fields[1] != "connected":
            continue
        if fields[0] == "wifi" and wifi is None and fields[2] not in ("", "--"):
            wifi = f"Connected to {fields[2]}"
        elif fields[0] == "ethernet":
            wired = True
    verdict = connectivity.strip().lower()
    if verdict not in CONNECTIVITY_LABELS:
        raise GateError(f"nmcli reported an unknown connectivity verdict {verdict!r}")
    if wifi is None and not wired:
        raise GateError(
            "nmcli reports no connected Wi-Fi or wired link; "
            f"the network check needs one:\n{devices}"
        )
    return NetworkExpectation(CONNECTIVITY_LABELS[verdict], wifi, wired)


def network_mismatches(names: list[str], expected: NetworkExpectation) -> list[str]:
    """What the Network pane's AT-SPI names lack or wrongly contain."""
    # gdbus quotes a name with single quotes, or double quotes when the
    # name itself holds an apostrophe.
    texts = [
        single or double
        for name in names
        for single, double in re.findall(r"'([^']*)'|\"([^\"]*)\"", name)
    ]
    problems = []
    if expected.connectivity not in texts:
        problems.append(f"connectivity {expected.connectivity!r}")
    if expected.wifi and expected.wifi not in texts:
        problems.append(f"Wi-Fi status {expected.wifi!r}")
    if expected.wired and not any(
        text == "Connected" or text.startswith("Connected · ") for text in texts
    ):
        problems.append("a connected wired status")
    errors = [text for text in texts if text.startswith(READ_ERROR_MARKERS)]
    if errors:
        problems.append(f"no read error, but it shows {errors!r}")
    return problems


def settings_network(address: str) -> None:
    devices = command("nmcli", "-t", "-f", "TYPE,STATE,CONNECTION", "device", "status")
    connectivity = command("nmcli", "-t", "networking", "connectivity")
    expected = network_expectation(devices, connectivity)
    bus_name, pid = open_in_session(address, SETTINGS_APP_NAME)
    try:
        client = AtspiClient(address, bus_name)
        # The Network pane's heading is also named "Network"; the category
        # entry is the one that can be clicked.
        tree = wait_for_reachable(
            client, (NETWORK_PANE,), SETTINGS_APP_NAME, SHELL_TREE_NODE_LIMIT, clickable=True,
        )
        client.click(tree, NETWORK_PANE)
        deadline = time.monotonic() + NETWORK_TIMEOUT_SECONDS
        problems: list[str] = ["the pane was never read"]
        names: list[str] = []
        while time.monotonic() < deadline:
            try:
                names = list(client.tree(SHELL_TREE_NODE_LIMIT).values())
            except GateError:
                time.sleep(POLL_SECONDS * 4)
                continue
            problems = network_mismatches(names, expected)
            if not problems:
                break
            time.sleep(POLL_SECONDS * 10)
        if problems:
            observed = "\n".join(sorted({name.strip() for name in names if name.strip()}))
            raise GateError(
                f"Settings' Network pane disagrees with nmcli ({expected}): it lacks "
                f"{'; '.join(problems)}; observed AT-SPI names:\n{observed}\n"
                f"session log: {app_log(SETTINGS_APP_NAME)}"
            )
    finally:
        close_app(pid)
    print(f"Settings network status matches nmcli: {expected}")


FOCUSED_STATE_BIT = 1 << 12
TYPING_TIMEOUT_SECONDS = 30
TYPING_TREE_NODE_LIMIT = 512


def text_reply(reply: str) -> str | None:
    """The string in a gdbus `Text.GetText` reply such as `('hi',)`."""
    match = re.match(r"\('(.*)',\)\s*$", reply.strip(), re.S)
    if not match:
        return None
    return match.group(1).replace("\\'", "'").replace("\\\\", "\\")


def text_nodes(client: AtspiClient) -> dict[str, str]:
    """Every node exposing AT-SPI Text, with its whole current text."""
    texts: dict[str, str] = {}
    for path in client.tree(TYPING_TREE_NODE_LIMIT):
        if "org.a11y.atspi.Text" not in client.interfaces(path):
            continue
        reply = client.call(path, "org.a11y.atspi.Text.GetText", "--", "0", "-1", check=False)
        text = text_reply(reply)
        if text is not None:
            texts[path] = text
    return texts


def is_typing_target(client: AtspiClient, path: str, name: str, field: str | None) -> bool:
    """Whether `path` is where a new document takes typing: the editable text
    field named FIELD when the app names one, else its document."""
    if field is not None:
        return (
            f"'{field}'" in name
            and "org.a11y.atspi.EditableText" in client.interfaces(path)
        )
    return client.role(path) in DOCUMENT_ROLES and "Component" in client.interfaces(path)


def prepare_typing(
    address: str, app_name: str, create: str, state: Path, field: str | None = None
) -> None:
    """Open APP_NAME, activate CREATE (its new-document action) and give the
    document — or its editable FIELD, for an app whose document is a list of
    blocks — keyboard focus, so the next real key presses type into it. The
    app's bus and pid are written to STATE for `typed`."""
    bus_name, pid = open_in_session(address, app_name)
    client = AtspiClient(address, bus_name)
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while True:
        try:
            tree = client.tree(TYPING_TREE_NODE_LIMIT)
            client.click(tree, create)
            break
        except GateError:
            if time.monotonic() >= deadline:
                close_app(pid)
                raise
            time.sleep(POLL_SECONDS * 4)
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    document = None
    while document is None:
        seen: list[str] = []
        try:
            tree = client.tree(TYPING_TREE_NODE_LIMIT)
        except GateError:
            # The app replaces its tree while the new document opens.
            tree = {}
        for path, name in tree.items():
            if is_typing_target(client, path, name, field):
                document = path
                break
            seen.append(f"role {client.role(path)} {name.strip()}")
        else:
            if time.monotonic() >= deadline:
                close_app(pid)
                # What the app shows instead says why: the empty state still
                # up means the action created nothing.
                raise GateError(
                    f"{app_name}: {create!r} opened no document to type into; "
                    f"the app exposes: {'; '.join(seen) or 'nothing'}; "
                    f"session log: {app_log(app_name)}"
                )
            time.sleep(POLL_SECONDS * 4)
    client.call(document, "org.a11y.atspi.Component.GrabFocus")
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while True:
        reply = client.call(document, "org.a11y.atspi.Accessible.GetState", check=False)
        word = re.search(r"uint32\s+(\d+)", reply)
        if word and int(word.group(1)) & FOCUSED_STATE_BIT:
            break
        if time.monotonic() >= deadline:
            close_app(pid)
            raise GateError(f"{app_name}: the document never took keyboard focus")
        time.sleep(POLL_SECONDS)
    state.write_text(f"{bus_name} {pid}\n")
    print(f"{app_name}: a new document has keyboard focus; type into it now")


MEDIA_UNREACHABLE = "document media service is"


def check_present(address: str, state: Path, app_name: str, name: str) -> None:
    """APP_NAME's document opened by `prepare_typing` gained a node named NAME (an
    image block is named after its alt text), and the app's launch reached
    the document media service that keeps such content with the document."""
    bus_name, pid_text = state.read_text().split()
    pid = int(pid_text)
    client = AtspiClient(address, bus_name)
    deadline = time.monotonic() + TYPING_TIMEOUT_SECONDS
    try:
        while time.monotonic() < deadline:
            try:
                if client.named(client.tree(TYPING_TREE_NODE_LIMIT), name):
                    break
            except GateError:
                pass
            time.sleep(POLL_SECONDS * 4)
        else:
            raise GateError(f"the document never showed a node named {name!r}")
        log = app_log(app_name)
        if MEDIA_UNREACHABLE in log:
            raise GateError(f"the app could not reach the document media service: {log}")
        print(f"the document shows {name!r}, kept by the document media service")
    finally:
        close_app(pid)
        state.unlink(missing_ok=True)


def check_typed(
    address: str, state: Path, expected: str, status: str | None = None, min_chars: int | None = None,
) -> None:
    """The document opened by `prepare_typing` holds exactly EXPECTED and,
    with STATUS, the app names its save state STATUS and no other. With
    MIN_CHARS, a text node holding at least that many of the typed characters
    is enough: long typing runs are judged by how much arrived, not by every
    key."""
    bus_name, pid_text = state.read_text().split()
    pid = int(pid_text)
    client = AtspiClient(address, bus_name)
    deadline = time.monotonic() + TYPING_TIMEOUT_SECONDS
    texts: dict[str, str] = {}
    try:
        while time.monotonic() < deadline:
            try:
                texts = text_nodes(client)
            except GateError:
                texts = {}
            if min_chars is not None and any(len(text) >= min_chars for text in texts.values()):
                longest = max(len(text) for text in texts.values())
                print(f"typed text reads back: {longest} of {len(expected)} characters")
                if status is not None:
                    check_save_status(client, status, deadline)
                return
            if expected in texts.values():
                print(f"typed text reads back exactly: {expected!r}")
                if status is not None:
                    # Saving starts once the typing ends: the save state gets
                    # its own window rather than what reading the text left.
                    check_save_status(
                        client, status, time.monotonic() + SAVE_TIMEOUT_SECONDS
                    )
                return
            time.sleep(POLL_SECONDS * 4)
        seen = sorted({text[:200] for text in texts.values() if text})
        raise GateError(f"typed {expected[:200]!r}, but the document's text nodes hold {seen!r}")
    finally:
        close_app(pid)
        state.unlink(missing_ok=True)


SESSION_LOG = Path.home() / ".local/state/tilo/session-logs/latest.log"
HISTORY_BUTTON = "History"


def session_log_lines() -> list[str]:
    try:
        return SESSION_LOG.read_text(errors="replace").splitlines()
    except OSError:
        return []


def wait_for_log_line(start: int, needle: str, subject: str, timeout: float) -> str:
    """The first session log line after line START that holds NEEDLE."""
    deadline = time.monotonic() + timeout
    while True:
        for line in session_log_lines()[start:]:
            if needle in line:
                return re.sub(r"\x1b\[[0-9;]*m", "", line).strip()
        if time.monotonic() >= deadline:
            raise GateError(f"{subject}: the session log never showed {needle!r}")
        time.sleep(POLL_SECONDS * 4)


def reopen_history(address: str, app_name: str, package: str, expect_text: str) -> None:
    """Open APP_NAME again on the document typed into before, require that it shows
    EXPECT_TEXT, then open its History. Prints the reopened app's APP-STARTUP line and
    History's HISTORY-OPEN line, which app_startup_check.py judges."""
    start = len(session_log_lines())
    bus_name, pid = open_in_session(address, app_name)
    client = AtspiClient(address, bus_name)
    try:
        startup = wait_for_log_line(
            start, f"stderr] APP-STARTUP app={package} ", app_name, LAUNCH_TIMEOUT_SECONDS)
        print(startup)
        deadline = time.monotonic() + START_TIMEOUT_SECONDS
        texts: dict[str, str] = {}
        while not any(expect_text in text for text in texts.values()):
            if time.monotonic() >= deadline:
                seen = sorted({text[:80] for text in texts.values() if text})
                raise GateError(f"{app_name} reopened without the typed note; it shows {seen!r}")
            time.sleep(POLL_SECONDS * 4)
            try:
                texts = text_nodes(client)
            except GateError:
                texts = {}
        deadline = time.monotonic() + START_TIMEOUT_SECONDS
        while True:
            try:
                client.click(client.tree(TYPING_TREE_NODE_LIMIT), HISTORY_BUTTON)
                break
            except GateError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(POLL_SECONDS * 4)
        print(wait_for_log_line(
            start, f"[app {package} stderr] HISTORY-OPEN ", f"{app_name} History",
            START_TIMEOUT_SECONDS))
    finally:
        close_app(pid)


SAVE_STATES = ("Saved", "Saving…", "Save failed", "Not saved")
SAVE_TIMEOUT_SECONDS = 30


def check_save_status(client: AtspiClient, expected: str, deadline: float) -> None:
    """The app's save-state node reads EXPECTED, and none reads another state."""
    shown: list[str] = []
    while time.monotonic() < deadline:
        try:
            tree = client.tree(TYPING_TREE_NODE_LIMIT)
        except GateError:
            time.sleep(POLL_SECONDS * 4)
            continue
        shown = [state for state in SAVE_STATES if client.named(tree, state)]
        if shown == [expected]:
            print(f"save state reads {expected!r}")
            return
        time.sleep(POLL_SECONDS * 4)
    raise GateError(f"save state should read {expected!r}, but the app shows {shown!r}")


def property_text(reply: str) -> str | None:
    """The string in a gdbus `Properties.Get` reply such as `(<'Level 2'>,)`."""
    match = re.match(r"\(<\s*'(.*)'\s*>,\)\s*$", reply.strip(), re.S)
    if not match:
        return None
    return match.group(1).replace("\\'", "'").replace("\\\\", "\\")


def check_described(address: str, state: Path, name: str, expected: str) -> None:
    """The document opened by `prepare_typing` has a node named NAME whose
    accessible description is EXPECTED: how a screen reader learns structure
    the text alone does not carry, such as how deep a block is nested."""
    bus_name, pid_text = state.read_text().split()
    pid = int(pid_text)
    client = AtspiClient(address, bus_name)
    deadline = time.monotonic() + TYPING_TIMEOUT_SECONDS
    seen: set[str] = set()
    try:
        while time.monotonic() < deadline:
            try:
                for path in client.named(client.tree(TYPING_TREE_NODE_LIMIT), name):
                    description = property_text(client.property(path, "Description"))
                    if description == expected:
                        print(f"{name!r} reads {expected!r}")
                        return
                    seen.add(repr(description))
            except GateError:
                pass
            time.sleep(POLL_SECONDS * 4)
        raise GateError(f"no node named {name!r} reads {expected!r}; saw {sorted(seen)}")
    finally:
        close_app(pid)
        state.unlink(missing_ok=True)


OSK_SERVICE_TIMEOUT_SECONDS = 60
OSK_TYPING_TIMEOUT_SECONDS = 15
# Three letter keys across the keyboard's middle letter row.
OSK_KEY_FRACTIONS = (0.2, 0.45, 0.7)
OSK_ROW_FRACTION = 0.55
SCREEN_COORDS = "0"


def extents_reply(reply: str) -> tuple[int, int, int, int] | None:
    """The rectangle in a gdbus `Component.GetExtents` reply such as `((0, 0, 1280, 800),)`."""
    match = re.match(r"\(\(\s*(-?\d+),\s*(-?\d+),\s*(-?\d+),\s*(-?\d+)\s*\),\)\s*$", reply.strip())
    if not match:
        return None
    x, y, w, h = (int(value) for value in match.groups())
    return x, y, w, h


def keyboard_rect(width: float, height: float) -> tuple[float, float, float, float]:
    """Where the compositor docks the on-screen keyboard on a WIDTH x HEIGHT
    output: bottom-anchored and centred, at most 960 wide, 40% of the height
    within 220..380, with a 20 px margin."""
    margin = min(20.0, width / 20.0)
    w = min(max(width - margin * 2.0, 1.0), 960.0)
    h = min(min(max(height * 0.40, 220.0), 380.0), max(height - margin * 2.0, 1.0))
    return (width - w) / 2.0, max(height - h - margin, 0.0), w, h


def osk_processes() -> list[int]:
    """This user's running on-screen keyboard processes."""
    uid = os.getuid()
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            if entry.stat().st_uid != uid:
                continue
            argv0 = (entry / "cmdline").read_bytes().split(b"\0", 1)[0]
        except OSError:
            continue
        if argv0.endswith(b"/bin/osk"):
            found.append(int(entry.name))
    return found


def shell_viewport(shell: AtspiClient) -> tuple[int, int]:
    """The compositor's output size, as its own AT-SPI window reports it."""
    largest = None
    for path in shell_tree(shell):
        rect = extents_reply(
            shell.call(path, "org.a11y.atspi.Component.GetExtents", SCREEN_COORDS, check=False)
        )
        if not rect or rect[2] <= 0 or rect[3] <= 0:
            continue
        role = shell.call(path, "org.a11y.atspi.Accessible.GetRoleName", check=False)
        if "'frame'" in role or "'window'" in role:
            return rect[0] + rect[2], rect[1] + rect[3]
        if largest is None or rect[2] * rect[3] > largest[2] * largest[3]:
            largest = rect
    if largest is None:
        raise GateError("the compositor's AT-SPI tree has no node with on-screen extents")
    # Without a window node, the desktop surface spans the output.
    return largest[0] + largest[2], largest[1] + largest[3]


def open_search_field(shell: AtspiClient) -> str:
    """Open the app palette and return its editable query field."""
    _, opener = wait_for_shell_node(shell, OPEN_APP_ACTION)
    result = shell.call(opener, "org.a11y.atspi.Action.DoAction", "0")
    if "true" not in result:
        raise GateError(f"AT-SPI {OPEN_APP_ACTION!r} was rejected: {result.strip()}")
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        tree = shell_tree(shell)
        for path in tree:
            if "Command palette" in tree[path]:
                continue
            if "org.a11y.atspi.EditableText" in shell.interfaces(path):
                return path
        time.sleep(POLL_SECONDS * 4)
    raise GateError("the app palette exposed no editable query over AT-SPI")


def field_text(shell: AtspiClient, path: str) -> str:
    reply = shell.call(path, "org.a11y.atspi.Text.GetText", "--", "0", "-1", check=False)
    return text_reply(reply) or ""


def osk_search(address: str, fifo: Path) -> None:
    """Tap the search field on the touchscreen FIFO drives; the on-screen
    keyboard must start and show, and tapping three of its letter keys must
    type into the field, read back over AT-SPI."""
    address = atspi_address()
    shell = shell_client(address)
    width, height = shell_viewport(shell)

    def tap(x: float, y: float) -> None:
        with fifo.open("w") as touch:
            touch.write(f"tap {x / width:.5f} {y / height:.5f}\n")

    try:
        field = open_search_field(shell)
        rect = extents_reply(
            shell.call(field, "org.a11y.atspi.Component.GetExtents", SCREEN_COORDS)
        )
        if not rect or rect[2] <= 0 or rect[3] <= 0:
            raise GateError(f"the search field has no on-screen extents: {rect}")
        centre = (rect[0] + rect[2] / 2, rect[1] + rect[3] / 2)
        before = field_text(shell, field)
        # A touchscreen in use is what starts the keyboard service.
        tap(*centre)
        deadline = time.monotonic() + OSK_SERVICE_TIMEOUT_SECONDS
        stable_since = None
        while True:
            if osk_processes():
                stable_since = stable_since or time.monotonic()
                # Alive past one restart period: it did not exit at start.
                if time.monotonic() - stable_since >= 6:
                    break
            else:
                stable_since = None
            if time.monotonic() >= deadline:
                raise GateError(
                    "the on-screen keyboard never kept running after a touch on the search field; "
                    f"session log: {app_log('osk')}"
                )
            time.sleep(POLL_SECONDS * 4)
        kx, ky, kw, kh = keyboard_rect(width, height)
        deadline = time.monotonic() + OSK_TYPING_TIMEOUT_SECONDS * 2
        while True:
            # The tap on the focused field shows the keyboard for it.
            tap(*centre)
            time.sleep(1.5)
            for fx in OSK_KEY_FRACTIONS:
                tap(kx + kw * fx, ky + kh * OSK_ROW_FRACTION)
                time.sleep(0.4)
            typed_deadline = time.monotonic() + OSK_TYPING_TIMEOUT_SECONDS
            text = before
            while time.monotonic() < typed_deadline:
                text = field_text(shell, field)
                if text != before and text.strip():
                    print(f"on-screen keyboard typed {text!r} into the search field")
                    return
                time.sleep(POLL_SECONDS * 4)
            if time.monotonic() >= deadline:
                raise GateError(
                    f"tapping the on-screen keyboard's keys left the search field at {text!r}; "
                    f"keyboard processes {osk_processes()}; session log: {app_log('osk')}"
                )
    finally:
        with fifo.open("w") as touch:
            touch.write("done\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--payload-root", type=Path, default=Path("/usr/share/tilo-apps/pkgs"),
        help="installed app package directory containing <content-hash>.pkg/manifest.toml",
    )
    sub = parser.add_subparsers(dest="command")
    opener = sub.add_parser("open", help="open an installed app in the session; print its AT-SPI bus and pid")
    opener.add_argument("app_name")
    names = sub.add_parser("names", help="print every accessible name in an opened app's tree")
    names.add_argument("bus")
    clicker = sub.add_parser("click", help="click the node with this accessible name in an opened app")
    clicker.add_argument("bus")
    clicker.add_argument("name")
    clicker.add_argument("--timeout", type=float, default=LAUNCH_TIMEOUT_SECONDS)
    typer = sub.add_parser("type", help="set the first editable text field of an opened app")
    typer.add_argument("bus")
    typer.add_argument("text")
    sub.add_parser(
        "settings-network",
        help="require Settings' Network pane to show what nmcli reports on this device",
    )
    typing = sub.add_parser(
        "type-into",
        help="open an installed app, start a new document and focus it for real keyboard input",
    )
    typing.add_argument("app_name")
    typing.add_argument("create", help="the accessible name of the app's new-document action")
    typing.add_argument("--state", type=Path, required=True)
    typing.add_argument(
        "--field",
        help="the accessible name of the editable field a new document types into, "
        "for apps whose document is a list of blocks",
    )
    typed = sub.add_parser("typed", help="check the document `type-into` opened holds exactly TEXT")
    typed.add_argument("text")
    typed.add_argument("--state", type=Path, required=True)
    typed.add_argument("--status", help="the save state the app must name once the text reads back")
    typed.add_argument(
        "--min-chars", type=int,
        help="accept a text node holding at least this many characters instead of exactly TEXT",
    )
    history = sub.add_parser(
        "reopen-history",
        help="open APP_NAME again, require the typed note, open History; print the app's "
        "APP-STARTUP and HISTORY-OPEN lines",
    )
    history.add_argument("app_name")
    history.add_argument("package")
    history.add_argument("--expect-text", required=True)
    described = sub.add_parser(
        "described",
        help="check the document `type-into` opened has a node NAME described as DESCRIPTION",
    )
    described.add_argument("name")
    described.add_argument("description")
    described.add_argument("--state", type=Path, required=True)
    present = sub.add_parser(
        "present",
        help="check APP_NAME's document `type-into` opened shows a node named NAME, its media service reached",
    )
    present.add_argument("app_name")
    present.add_argument("name")
    present.add_argument("--state", type=Path, required=True)
    card = sub.add_parser(
        "card", help="activate a launchpad card on the empty workspace; require that it opens its app",
    )
    card.add_argument("card_name")
    card.add_argument("app_name")
    card.add_argument(
        "--expect", action="append", default=[], metavar="NAME",
        help="an accessible name the app must expose after the card's action (repeatable)",
    )
    page = sub.add_parser(
        "page", help="open Browser, wait for the caller to type the printed address, require the page to load",
    )
    page.add_argument("--title", default=PAGE_TITLE)
    tab = sub.add_parser(
        "new-tab",
        help="open an app, print READY, and require that the Ctrl+T pressed next opens it "
        "again as a desktop tab beside the first",
    )
    tab.add_argument("app_name")
    osk = sub.add_parser(
        "osk-search",
        help="tap the search field on a touchscreen; the on-screen keyboard must show and type into it",
    )
    osk.add_argument("fifo", type=Path, help="the touchscreen command FIFO input-hotplug-check.sh serves")
    args = parser.parse_args()
    for executable in ("busctl", "gdbus"):
        if not shutil.which(executable):
            print(f"AT-SPI external-client failed: required executable {executable} is unavailable", file=sys.stderr)
            return 2
    try:
        address = atspi_address()
        if args.command == "open":
            bus_name, pid = open_in_session(address, args.app_name)
            print(f"{bus_name} {pid}")
            return 0
        if args.command == "page":
            url = load_page(address, args.title)
            print(f"AT-SPI external client passed: Browser loaded {url} ({args.title!r})")
            return 0
        if args.command == "names":
            print("\n".join(app_names(address, args.bus)))
            return 0
        if args.command == "click":
            click_in_app(address, args.bus, args.name, args.timeout)
            return 0
        if args.command == "type":
            type_in_app(address, args.bus, args.text)
            return 0
        if args.command == "settings-network":
            settings_network(address)
            return 0
        if args.command == "type-into":
            prepare_typing(address, args.app_name, args.create, args.state, args.field)
            return 0
        if args.command == "typed":
            check_typed(address, args.state, args.text, args.status, args.min_chars)
            return 0
        if args.command == "reopen-history":
            reopen_history(address, args.app_name, args.package, args.expect_text)
            return 0
        if args.command == "described":
            check_described(address, args.state, args.name, args.description)
            return 0
        if args.command == "present":
            check_present(address, args.state, args.app_name, args.name)
            return 0
        if args.command == "osk-search":
            osk_search(address, args.fifo)
            return 0
        if args.command == "new-tab":
            new_tab(address, args.app_name)
            return 0
        if args.command == "card":
            _bus_name, pid = open_from_launchpad_card(
                address, args.card_name, args.app_name, tuple(args.expect),
            )
            close_app(pid)
            print(f"AT-SPI external client passed: launchpad card {args.card_name!r} opened {args.app_name}")
            return 0
        for scenario, _binary in installed_scenarios(args.payload_root):
            exercise(scenario, address)
    except GateError as error:
        print(f"AT-SPI external-client failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
