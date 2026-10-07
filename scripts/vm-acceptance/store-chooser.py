#!/usr/bin/env python3
"""Review an already staged official package through the owner Store/portal actions.

No staging, trust changes, clicks, power/session operations, or install by default.
"""
import argparse
import importlib.util
import json
import re
import uuid
from pathlib import Path

from acceptance.action_jobs import begin, invoke, require
from acceptance.vm import TiloVm, VmError, first_json, wait_until

spec = importlib.util.spec_from_file_location("app_cohort", Path(__file__).with_name("app-cohort.py"))
app = importlib.util.module_from_spec(spec)
spec.loader.exec_module(app)


class OwnerVm(TiloVm):
    """Canonical CLI on the same authenticated owner console, via slot exec."""
    def control_json(self, *args, timeout=60):
        uid = self.slot_uid()
        command = ["env", "-u", "TILO_TEST_CONTROL", f"XDG_RUNTIME_DIR=/run/user/{uid}",
                   "/usr/bin/tilo-control", "--socket", f"/run/user/{uid}/tilo-session/admin-control.sock", *args]
        return first_json(self.run("slot", "exec", "--", *command, timeout=timeout))


def workspace(vm, expected_room):
    state = vm.state()
    tree = state.get("workspace_tree", {})
    current = [w for w in tree.get("workspaces", []) if w.get("id") == tree.get("active")]
    require(state.get("chrome", {}).get("session_locked") is False and len(current) == 1
            and current[0].get("room") == expected_room, "unlocked expected owner workspace unavailable")


def picker_rows(vm):
    reply = vm.control_json("actions", "list", "file-picker", "--programmatic", "--json")
    require(reply.get("query") == "file-picker" and isinstance(reply.get("actions"), list),
            "canonical chooser discovery unavailable")
    return [row for row in reply["actions"] if row.get("provider") == "file-picker"]


def request(rows):
    ids = set()
    for row in rows:
        operation, separator, identity = row.get("id", "").partition("@")
        require(separator and operation in ("inspect", "select", "navigate", "up", "confirm", "cancel",
                "filename", "replace", "keep-existing", "next-filter"), "invalid chooser action identity")
        try:
            require(str(uuid.UUID(identity)) == identity, "noncanonical chooser request identity")
        except ValueError as error:
            raise VmError("invalid chooser request identity") from error
        ids.add(identity)
    require(len(ids) == 1, "missing or competing chooser request")
    return ids.pop()


def component(name):
    require(isinstance(name, str) and name not in ("", ".", "..") and "/" not in name
            and "\\" not in name and len(name) <= 255 and "\x00" not in name,
            "entry must be one declared filename component")


class Chooser:
    def __init__(self, vm, identity, room, out, original):
        self.vm, self.identity, self.room, self.out, self.original = vm, identity, room, out, original
        self.sequence = 0

    def call(self, operation, args=None):
        self.original.pending()
        workspace(self.vm, self.room)
        rows = picker_rows(self.vm)
        require(request(rows) == self.identity, "chooser request replaced")
        found = [r for r in rows if r["id"] == f"{operation}@{self.identity}"]
        require(len(found) == 1, "declared chooser operation unavailable or ambiguous")
        self.sequence += 1
        value = invoke(self.vm, found[0], args or {}, self.out, f"picker-{self.sequence}-{operation}")
        require(isinstance(value, dict) and value.get("request_id") == self.identity
                and value.get("application") == app.STORE and isinstance(value.get("entries"), list)
                and re.fullmatch(r"[0-9a-f]{64}", value.get("state", "")), "chooser subject/state unobservable")
        return value

    def ascend(self, count):
        for _ in range(count):
            current = self.call("inspect", {"page": 0})
            require(current.get("completed") is False and current.get("can_up") is True,
                    "declared chooser parent unavailable; no path inferred")
            self.call("up", {"state": current["state"]})

    def find(self, name, directory):
        component(name)
        first = self.call("inspect", {"page": 0})
        require(first.get("completed") is False and isinstance(first.get("pages"), int)
                and 1 <= first["pages"] <= 1024, "active bounded chooser listing unavailable")
        matches = []
        for page in range(first["pages"]):
            value = first if page == 0 else self.call("inspect", {"page": page})
            require(value["state"] == first["state"] and value.get("pages") == first["pages"],
                    "chooser listing changed during inspection")
            matches += [entry for entry in value["entries"] if entry.get("name") == name
                        and entry.get("directory") is directory]
        require(len(matches) == 1 and re.fullmatch(r"[0-9a-f]{64}", matches[0].get("id", "")),
                "expected declared entry absent or ambiguous")
        return first["state"], matches[0]["id"]


def review(door, fixture, filename, directories, out, initial_up=0):
    require(isinstance(initial_up, int) and not isinstance(initial_up, bool) and 0 <= initial_up <= 16,
            "initial parent count must be bounded")
    component(filename)
    for name in directories:
        component(name)
    vm = door.vm
    workspace(vm, door.bound[1])
    require(not picker_rows(vm), "another chooser already active; no Import submitted")
    row = app.choose(app.catalog(vm, app.STORE), app.STORE, "store.catalog", "import-package", {}, door.bound)
    original = begin(vm, row, {}, out, f"import-{filename}")
    def appeared():
        original.pending()
        workspace(vm, door.bound[1])
        rows = picker_rows(vm)
        return request(rows) if rows else None
    identity = wait_until(appeared, 30)
    require(identity is not None, "Import did not publish an observable chooser")
    chooser = Chooser(vm, identity, door.bound[1], out, original)
    chooser.ascend(initial_up)
    for name in directories:
        state, entry = chooser.find(name, True)
        chooser.call("navigate", {"state": state, "entry": entry})
    state, entry = chooser.find(filename, False)
    selected = chooser.call("select", {"state": state, "entry": entry})
    require(selected.get("can_confirm") is True, "selected file cannot be confirmed")
    completed = chooser.call("confirm", {"state": selected["state"], "confirm": True})
    require(completed.get("completed") is True, "chooser did not complete the portal request")
    original.finish(120)
    require(not picker_rows(vm), "chooser authority still visible after completion")
    value = door.settled(fixture["package_id"])
    expected = {"app": fixture["package_id"], "root": fixture["offer"]["tapp_root_hash_hex"],
                "version": fixture["offer"]["version"]}
    require(value.get("imported_package") == expected and value.get("detail", {}).get("trust") == "PublisherSigned",
            "Store did not independently review the exact official signed package")
    return value


def install_open(door, fixture, installed_before, out):
    vm = door.vm
    package, offer = fixture["package_id"], fixture["offer"]
    before = app.windows(vm.state(), package)
    door.call("install-imported-package", {"app": package, "root": offer["tapp_root_hash_hex"]}, 300)
    require(door.settled(package)["detail"]["installed"] is True, "Store install readback absent")
    audit = Path(__file__).with_name("acceptance") / "official_runtime.py"
    verified = first_json(vm.run("slot", "exec", "--", "python3", "-c", audit.read_text(),
        package, offer["tapp_root_hash_hex"], offer["version"], offer["target"]["runtime"],
        fixture["publisher"], timeout=300))
    require(verified.get("uid") == vm.slot_uid() and verified.get("files"),
            "canonical installed official runtime audit unavailable")
    (out / "official-runtime.json").write_text(json.dumps(verified, indent=2))
    door.call("open", {"app": package})
    def subject():
        state = vm.state()
        current = app.windows(state, package)
        candidates = [tab for tab in current if tab[0] == door.bound[1]]
        require(len(candidates) <= 1, "official app has ambiguous same-room windows; none closed")
        if candidates:
            tab = candidates[0]
            record = app.mapped_record(state, tab, package)
            if record is not None:
                return tab, record
        return None
    selected = wait_until(subject, 60)
    require(selected, "installed app has no observable public tab/record association")
    # Conservatively require every existing Calculator worker to carry the exact
    # official bundle. No PID/surface guess may excuse an old resident worker.
    app.mapped(vm, "calculator", verified["files"], vm.slot_uid(), out,
               executable=verified["executable_sha256"])
    state = vm.state()
    require(selected[0] in app.windows(state, package)
            and app.mapped_record(state, selected[0], package) == selected[1],
            "official app public association changed during mapped-backing audit")
    return {"installation": "update/reuse" if installed_before else "first-install",
            "window": "reuse" if selected[0] in before else "new", "tab": selected[0],
            "record": selected[1], "root": offer["tapp_root_hash_hex"], "version": offer["version"],
            "runtime": verified["runtime"], "mapped_backing": "calculator-mapped-sdk.json"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tilo-vm", default="tilo-vm")
    parser.add_argument("--vm", required=True)
    parser.add_argument("--slot", required=True)
    parser.add_argument("--cohort-receipt", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True, help="canonical verified-offer.json audit receipt")
    parser.add_argument("--initial-up", type=int, choices=range(17), default=0,
                        help="explicit declared parent steps before directories (normally 1 from Documents to HOME)")
    parser.add_argument("--directories", nargs="*", default=["Downloads"], help="declared components from initial chooser directory")
    parser.add_argument("--full", required=True, help="already staged filename, not a path")
    parser.add_argument("--thin", required=True, help="already staged filename, not a path")
    parser.add_argument("--install-open", action="store_true", help="after both reviews, install once through Store and observe Open")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    vm = OwnerVm(args.tilo_vm, args.vm, args.slot, args.out / "commands.log")
    _, _, installed_sdk = app.cohort(vm, args, args.out)
    fixture = json.loads(args.fixture.read_text())
    require(fixture.get("package_id") == "so.tilo.calculator" and fixture.get("independent_owner") ==
            "55d9a9c748bcfe34200832c18c5f0cf7932f17571612321eaa542474fa2abe06"
            and fixture.get("offer", {}).get("target", {}).get("architecture") == "aarch64",
            "expected independently verified official ARM fixture receipt required")
    door = app.store(vm, args.out)
    app.mapped(vm, "store", installed_sdk, vm.slot_uid(), args.out)
    first = review(door, fixture, args.thin, args.directories, args.out, args.initial_up)
    review(door, fixture, args.full, args.directories, args.out, args.initial_up)
    result = {"reviewed_full_and_thin": True, "installed_before": first["detail"]["installed"],
              "installation": "not requested", "runtime": fixture["offer"]["target"]["runtime"]}
    if args.install_open:
        result.update(install_open(door, fixture, first["detail"]["installed"], args.out))
    (args.out / "result.json").write_text(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
