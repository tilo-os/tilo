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
AT-SPI bus name and process id, for gates with their own checks.

The small ``[accessibility.atspi_acceptance]`` section in a signed app
manifest is the certification data.  Adding an app is data, not another
one-off screen-reader script.  Scenarios are intentionally limited to a safe
navigation/action and a visible post-action name; hardware, credentials, and
network journeys remain product-specific acceptance work.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import time
import tomllib
from dataclasses import dataclass


ROOT_PATH = "/org/a11y/atspi/accessible/root"
PATH_RE = re.compile(r"/org/a11y/atspi/accessible/[0-9/]+")
POLL_SECONDS = 0.05
START_TIMEOUT_SECONDS = 20
TREE_NODE_LIMIT = 128
SHELL_TREE_NODE_LIMIT = 1024
# The session's compositor owns this backend name; it identifies the shell's
# AT-SPI connection without reading another process's /proc entry.
SHELL_BUS_NAME = "org.freedesktop.impl.portal.desktop.tilo"
OPEN_APP_ACTION = "Open an app"
LAUNCH_TIMEOUT_SECONDS = 60


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

    def click(self, tree: dict[str, str], expected: str) -> None:
        for path in self.named(tree, expected):
            actions = self.call(path, "org.a11y.atspi.Action.GetActions", check=False)
            if "'click'" not in actions:
                continue
            result = self.call(path, "org.a11y.atspi.Action.DoAction", "0")
            if "true" not in result:
                raise GateError(f"AT-SPI action {expected!r} was rejected: {result.strip()}")
            return
        raise GateError(f"AT-SPI tree has no click-capable node named {expected!r}")


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
    raise GateError(f"the session exposed no {kind} {name!r} over AT-SPI")


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
    _, row = wait_for_shell_node(shell, app_name)
    result = shell.call(row, "org.a11y.atspi.Action.DoAction", "0")
    if "true" not in result:
        raise GateError(f"AT-SPI palette row {app_name!r} was rejected: {result.strip()}")
    deadline = time.monotonic() + LAUNCH_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        for bus, pid in bus_pids(address).items():
            if bus in before:
                continue
            client = AtspiClient(address, bus)
            try:
                name = client.property(ROOT_PATH, "Name")
            except GateError:
                continue
            if f"'{app_name}'" in name:
                return bus, pid
        time.sleep(POLL_SECONDS * 4)
    raise GateError(f"{app_name!r} was opened but never appeared on the AT-SPI bus")


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


def wait_for_tree(client: AtspiClient, names: tuple[str, ...], scenario: Scenario) -> dict[str, str]:
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
        f"{scenario.app_id}: tree never exposed {', '.join(names)}; "
        f"observed AT-SPI names:\n{observed}"
    )


def exercise(scenario: Scenario, address: str) -> None:
    bus_name, pid = open_in_session(address, scenario.app_name)
    try:
        client = AtspiClient(address, bus_name)
        initial = wait_for_tree(client, (scenario.root, *scenario.required), scenario)
        client.click(initial, scenario.action)
        wait_for_tree(client, scenario.after_action, scenario)
    except GateError as error:
        raise GateError(f"{scenario.app_id}: {error}") from error
    finally:
        close_app(pid)
    print(f"AT-SPI external client passed: {scenario.app_id} ({scenario.action} action round trip)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--payload-root", type=Path, default=Path("/usr/share/tilo-apps/pkgs"),
        help="installed app package directory containing <content-hash>.pkg/manifest.toml",
    )
    sub = parser.add_subparsers(dest="command")
    opener = sub.add_parser("open", help="open an installed app in the session; print its AT-SPI bus and pid")
    opener.add_argument("app_name")
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
        for scenario, _binary in installed_scenarios(args.payload_root):
            exercise(scenario, address)
    except GateError as error:
        print(f"AT-SPI external-client failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
