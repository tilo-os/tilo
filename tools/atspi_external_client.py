#!/usr/bin/env python3
"""Exercise packaged Tilo apps through the real AT-SPI D-Bus endpoint.

This is deliberately an *external* client: it starts an installed package
payload, finds that process on the accessibility bus, walks exported objects,
and invokes an action using the public AT-SPI interfaces.  It does not import
the UI crate or inspect an in-process AccessKit tree.

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
import tempfile
import time
import tomllib
from dataclasses import dataclass


ROOT_PATH = "/org/a11y/atspi/accessible/root"
PATH_RE = re.compile(r"/org/a11y/atspi/accessible/[0-9/]+")
POLL_SECONDS = 0.05
START_TIMEOUT_SECONDS = 20
TREE_NODE_LIMIT = 128


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

    def tree(self) -> dict[str, str]:
        pending = [ROOT_PATH]
        seen: set[str] = set()
        result: dict[str, str] = {}
        while pending and len(seen) < TREE_NODE_LIMIT:
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


def find_bus_for_pid(address: str, pid: int, process: subprocess.Popen[bytes]) -> str:
    deadline = time.monotonic() + START_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise GateError(f"app process exited before AT-SPI registration (status {process.returncode})")
        output = command("busctl", f"--address={address}", "list", "--no-pager", check=False)
        for line in output.splitlines():
            fields = line.split()
            if len(fields) > 1 and fields[1] == str(pid):
                return fields[0]
        time.sleep(POLL_SECONDS)
    raise GateError(f"pid {pid} did not register on the AT-SPI bus within {START_TIMEOUT_SECONDS}s")


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


def terminate(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=3)


def exercise(scenario: Scenario, binary: Path, address: str) -> None:
    state_dir = Path(tempfile.mkdtemp(prefix=f"tilo-atspi-{scenario.app_id.rsplit('.', 1)[-1]}-"))
    log = state_dir / "app.log"
    environment = os.environ | {
        "TILO_APP_NAME": scenario.app_name,
        "TILO_USER_HOME": str(state_dir),
        "XDG_CONFIG_HOME": str(state_dir / "config"),
        "XDG_CACHE_HOME": str(state_dir / "cache"),
        "XDG_DATA_HOME": str(state_dir / "data"),
        "XDG_STATE_HOME": str(state_dir / "state"),
    }
    with log.open("wb") as output:
        process = subprocess.Popen(
            [str(binary)], env=environment, stdout=output, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            bus_name = find_bus_for_pid(address, process.pid, process)
            client = AtspiClient(address, bus_name)
            initial = wait_for_tree(client, (scenario.root, *scenario.required), scenario)
            client.click(initial, scenario.action)
            wait_for_tree(client, scenario.after_action, scenario)
        except Exception as error:
            raise GateError(f"{scenario.app_id}: {error}; app log retained at {log}") from error
        finally:
            terminate(process)
    shutil.rmtree(state_dir)
    print(f"AT-SPI external client passed: {scenario.app_id} ({scenario.action} action round trip)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--payload-root", type=Path, default=Path("/usr/share/tilo-apps/pkgs"),
        help="installed app package directory containing <content-hash>.pkg/manifest.toml",
    )
    args = parser.parse_args()
    for executable in ("busctl", "gdbus"):
        if not shutil.which(executable):
            print(f"AT-SPI external-client failed: required executable {executable} is unavailable", file=sys.stderr)
            return 2
    try:
        address = atspi_address()
        for scenario, binary in installed_scenarios(args.payload_root):
            exercise(scenario, binary, address)
    except GateError as error:
        print(f"AT-SPI external-client failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
