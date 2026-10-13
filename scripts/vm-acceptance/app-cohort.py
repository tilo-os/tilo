#!/usr/bin/env python3
"""Semantic Store/Browser acceptance in an already activated owned SDK cohort.

Never stages packages, grants trust, closes windows, restarts sessions or reads Notes.
Run only after the deployment owner hands off the matched cohort.
"""
import argparse
import json
import re
import shlex
import time
from pathlib import Path
from urllib.parse import urlsplit

from acceptance.action_jobs import invoke, require
from acceptance.vm import TiloVm, first_json, wait_until

STORE = "so.tilo.store"
BROWSER = "so.tilo.browser"
CALCULATOR = "org.gnome.Calculator"


def binding(row):
    return tuple(row.get(k) for k in ("application", "room", "record", "resource"))


def catalog(vm, app):
    result = vm.control_json("actions", "list", app, "--programmatic", "--json")
    require(result.get("query") == app and isinstance(result.get("actions"), list),
            "matched programmatic action discovery unavailable")
    return [r for r in result["actions"] if r.get("application") == app]


def choose(rows, app, kind, operation, args, bound=None):
    found = []
    for row in rows:
        if (row.get("provider") != "app-actions" or row.get("application") != app
                or (row.get("semantic_identity") or {}).get("kind") != kind
                or row.get("operation") != operation
                or (bound is not None and binding(row) != bound)):
            continue
        params = row.get("params")
        if not isinstance(params, list):
            continue
        names = {p["name"] for p in params}
        required = {p["name"] for p in params if p.get("required")}
        confirmation = row.get("contract", {}).get("confirmation", {}).get("mode")
        supplied = set(args) | ({"confirm"} if confirmation == "confirm" else set())
        if required <= supplied <= names:
            found.append(row)
    require(len(found) == 1, f"expected one exact {app}/{operation} binding, got {len(found)}")
    row = found[0]
    require(all(binding(row)) and bool(row.get("id")), "incomplete application binding")
    require(row.get("contract", {}).get("availability", {}).get("state") == "available",
            f"{app}/{operation} unavailable")
    return row


class AppDoor:
    def __init__(self, vm, app, kind, bound, out):
        self.vm, self.app, self.kind, self.bound, self.out = vm, app, kind, bound, out
        self.sequence = 0

    def call(self, operation, args=None, timeout=120):
        args = dict(args or {})
        row = choose(catalog(self.vm, self.app), self.app, self.kind, operation, args, self.bound)
        if row.get("contract", {}).get("confirmation", {}).get("mode") == "confirm":
            args["confirm"] = True
        self.sequence += 1
        (self.out / f"{self.app}-{self.sequence}-contract.json").write_text(json.dumps(row, indent=2))
        return invoke(self.vm, row, args, self.out, f"{self.app}-{self.sequence}-{operation}", timeout)

    def settled(self, app=None, timeout=120):
        deadline = time.monotonic() + timeout
        while True:
            value = self.call("inspect-catalogue", {"app": app} if app else {})
            require(isinstance(value, dict) and value.get("worker_attached") is True,
                    "Store worker/readback unobservable")
            require(not value.get("error") and not value.get("flatpak_error"), "Store worker reported failure")
            if value.get("worker_busy") is False and value.get("pending_jobs") == 0:
                return value
            require(time.monotonic() < deadline, "Store never settled")
            time.sleep(0.5)


def windows(state, app):
    return {(str(w["room"]), str(t["id"]), str(f["surface_id"]))
            for w in state["workspace_tree"]["workspaces"]
            for strip in w["strips"] for group in strip["window_groups"] for t in group["tabs"]
            if (f := (t.get("tile") or {}).get("Foreign")) and f.get("app_id") == app}


# Public package/process metadata only; no argv, environment or document content.
BASELINE = r'''
import json,os,pathlib,subprocess,tomllib
apps=subprocess.run(['flatpak','list','--app','--columns=application,installation'],capture_output=True,text=True,check=True).stdout.splitlines()
remotes=subprocess.run(['flatpak','remotes','--columns=name,url,options'],capture_output=True,text=True,check=True).stdout.splitlines()
trust=pathlib.Path(os.environ.get('XDG_CONFIG_HOME',str(pathlib.Path.home()/'.config')))/'tilo/trusted-attestors.toml'
entries=tomllib.loads(trust.read_text()).get('attestor',[]) if trust.exists() else []
print(json.dumps({'uid':os.getuid(),'flatpak_apps':[l.split()[0] for l in apps if l.split()],
 'flathub':[{'official':c[1].rstrip('/')=='https://dl.flathub.org/repo','disabled':'disabled' in ','.join(c[2:]).split(',')} for l in remotes if len(c:=l.split())>=2 and c[0]=='flathub'],
 'trusted_attestor_count':len(entries)}))
'''


def cohort(vm, args, out):
    receipt = json.loads(args.cohort_receipt.read_text())
    source = receipt.get("cohort_source", {})
    require(receipt.get("status") == "success" and receipt.get("component") == "apps+runtime"
            and receipt.get("vm") == args.vm and receipt.get("slot") == args.slot,
            "successful addressed canonical cohort activation receipt required")
    require(re.fullmatch(r"[0-9a-f]{40}", source.get("source_commit", ""))
            and re.fullmatch(r"[0-9a-f]{64}", source.get("dirty_tree_hash", "")),
            "full source inventory provenance required")
    shown = first_json(vm.run("--json", "slot", "show"))
    require(shown["slot"]["session"] == "active"
            and shown["slot"]["components"].get("apps+runtime") == receipt["layer"], "cohort inactive or changed")
    installed = {f["path"].replace("/usr/lib/tilo-runtime/", "/run/tilo-runtime/", 1): f["sha256"]
                 for f in shown["files"] if f["layer"] == receipt["layer"]
                 and f["path"].startswith("/usr/lib/tilo-runtime/")}
    require(any(p.endswith("/lib/libruntime_dylib.so") for p in installed), "closed SDK unobservable")
    (out / "cohort.json").write_text(json.dumps(shown, indent=2))
    return receipt, shown, installed


def mapped(vm, comm, installed, uid, out, *, executable=None, selected=None):
    source = Path(__file__).with_name("acceptance").joinpath("runtime_backing.py").read_text()
    selection = ""
    if selected is not None:
        pid, start = selected.get("peer_pid"), selected.get("peer_start_time_ticks")
        require(type(pid) is int and pid > 0 and isinstance(start, str) and start.isdecimal(),
                "authenticated native worker identity unavailable")
        selection = " " + str(pid) + " " + shlex.quote(start)
    value = first_json(vm.root_exec("python3 -c " + shlex.quote(source) + " " + shlex.quote(comm) + " " + str(uid) + selection))
    require(value.get("method") == "proc-map-files-fd" and value.get("reader_uid") == 0,
            "exact read-only mapped backing observation unavailable")
    require(value["uid"] == uid and value["processes"], "native worker unobservable")
    if selected is not None:
        require(len(value["processes"]) == 1 and value["processes"][0].get("pid") == pid
                and value["processes"][0].get("start_time_ticks") == start,
                "kernel backing observer returned a foreign worker")
    for process in value["processes"]:
        if executable is not None:
            require(process.get("executable", {}).get("sha256") == executable,
                    "resident app executable differs from verified installed package")
        rows = process["runtime"]
        require(any(r["path"].endswith("/lib/libruntime_dylib.so") for r in rows)
                and all(installed.get(r["path"]) == r["sha256"] for r in rows), "mapped SDK differs from verified cohort")
    (out / f"{comm}-mapped-sdk.json").write_text(json.dumps(value, indent=2))


def store(vm, out):
    rows = catalog(vm, STORE)
    if not rows:
        opens = [r for r in vm.actions(STORE) if r.get("provider") == "applications" and r.get("id") == STORE]
        require(len(opens) == 1, "normal Store Open unavailable")
        invoke(vm, opens[0], {}, out, "open-store", require_job=False)
        rows = wait_until(lambda: catalog(vm, STORE), 60)
        require(rows, "Store resource never appeared")
    require(not any(r.get("operation") == "use-flathub-for-account" for r in rows), "obsolete Flathub opt-in remains")
    row = choose(rows, STORE, "store.catalog", "inspect-catalogue", {})
    return AppDoor(vm, STORE, "store.catalog", binding(row), out)


def flatpak_flow(door, baseline):
    require(CALCULATOR not in baseline["flatpak_apps"], "Calculator already installed; no first-install claim")
    require(baseline["flathub"] and all(r["official"] and not r["disabled"] for r in baseline["flathub"]),
            "official enabled Flathub prerequisite not observed")
    initial = door.settled()
    require(initial.get("flatpak_available") is True and initial.get("remotes_truncated") is False,
            "default Flatpak/remotes unavailable")
    remotes = [r for r in initial["remotes"] if r["name"] == "flathub" and r["disabled"] is False]
    require(remotes, "Flathub not exposed by Store")
    remote = next((r for r in remotes if r["scope"] == "account"), remotes[0])
    door.call("browse-remote", {"remote": remote["resource"]}, timeout=300)
    browsed = door.settled()
    require(browsed["matching"] > 0, "Flathub browse returned no observable apps")
    door.call("set-search", {"query": "Calculator"})
    searched = door.settled()
    require(searched["query"] == "Calculator" and any(i["id"] == CALCULATOR for i in searched["items"]),
            "real Flatpak Calculator search result missing")
    door.call("show-details", {"app": CALCULATOR})
    detail = door.settled(CALCULATOR)
    item = detail["detail"]
    require(detail["selected_app"] == CALCULATOR and item["source"] == "Flatpak"
            and item["installed"] is False and all(item.get(k) for k in ("name", "description", "latest_version", "repository", "trust")),
            "readable first-install details missing")
    door.call("flatpak-install", {"app": CALCULATOR}, timeout=600)
    item = door.settled(CALCULATOR)["detail"]
    require(item["installed"] is True, "Store did not observe installation")
    door.vm.run("slot", "exec", "--", "flatpak", "--user", "info", CALCULATOR)
    before = windows(door.vm.state(), CALCULATOR)
    require(not before, "Calculator window already open before first launch")
    door.call("flatpak-open", {"app": CALCULATOR})
    require(wait_until(lambda: windows(door.vm.state(), CALCULATOR), 60), "installed Calculator never mapped")
    return {"app": CALCULATOR, "installed_before": False, "account_install_observed": True, "mapped": True}


def package_flow(door, spec, local):
    require(set(spec) == {"app", "root", "version", "path"} and re.fullmatch(r"[0-9a-f]{64}", spec["root"]),
            "fixture must name exact app/root/version and already sandbox-readable path")
    require(spec["app"] not in ("so.tilo.notes", STORE, BROWSER),
            "fixture must not touch Notes or replace the acceptance workers")
    require(not windows(door.vm.state(), spec["app"]), "fixture app already has an owned window")
    door.call("import-package", {"path": spec["path"]})
    review = door.settled(spec["app"])
    require(review.get("imported_package") == {k: spec[k] for k in ("app", "root", "version")}, "reviewed identity differs")
    item = review["detail"]
    require(item["trust"] == ("TrustedAttestor" if local else "PublisherSigned"), "wrong independent trust route")
    baseline = item["installed"]
    door.call("install-imported-package", {"app": spec["app"], "root": spec["root"]}, timeout=300)
    result = door.settled(spec["app"])
    require(result["detail"]["installed"] is True, "installed local package unobservable")
    inventory = first_json(door.vm.run("slot", "exec", "--", "app", "list", "--installed", "--json"))
    installed = [row for row in inventory if row.get("package_id") == spec["app"]]
    require(len(installed) == 1 and installed[0]["version"] == spec["version"],
            "canonical installed inventory differs from reviewed version")
    door.call("open-app", {"app": spec["app"]})
    require(wait_until(lambda: windows(door.vm.state(), spec["app"]), 60),
            "imported app never mapped")
    return {"route": "trusted-local" if local else "catalogue", "app": spec["app"], "root": spec["root"],
            "installed_before": baseline, "claim": "update/reuse" if baseline else "first-install"}


def mapped_record(state, tab, application):
    """Use the current kernel-bound certified checkout, never continuity aliases."""
    worker = native_subject(state, tab, application)
    return None if worker is None else worker["certified_checkout"]["record"]


def fresh_browser(vm, out):
    before_tabs = windows(vm.state(), BROWSER)
    before_rows = catalog(vm, BROWSER)
    old = {binding(r) for r in before_rows}
    opens = [r for r in vm.actions("tab.open-beside") if r.get("provider") == "actions" and r.get("id") == "tab.open-beside"]
    require(len(opens) == 1, "declared new Browser tab action unavailable")
    invoke(vm, opens[0], {"app": BROWSER}, out, "new-browser-tab", require_job=False)

    def probe():
        state = vm.state()
        tabs = windows(state, BROWSER)
        require(before_tabs <= tabs, "an existing Browser tab changed; acceptance stopped")
        fresh = tabs - before_tabs
        require(len(fresh) <= 1, "fresh Browser window ambiguous")
        if fresh:
            tab = next(iter(fresh))
            record = mapped_record(state, tab, BROWSER)
            if record is None:
                return None
            require(record not in {b[2] for b in old}, "new tab reused an existing Browser record")
            rows = [r for r in catalog(vm, BROWSER)
                    if (r.get("semantic_identity") or {}).get("kind") == "browser.page"
                    and r.get("room") == tab[0] and r.get("record") == record]
            bound = {binding(r) for r in rows}
            require(len(bound) <= 1, "new tab published ambiguous page bindings")
            if bound:
                return next(iter(bound)), tab
        return None

    result = wait_until(probe, 60)
    require(result, "queued Open did not create a new mapped tab and record; existing page left untouched")
    return AppDoor(vm, BROWSER, "browser.page", result[0], out), result[1]


def browser_subject(state, tab):
    return native_subject(state, tab, BROWSER)


def native_subject(state, tab, application):
    """Bind an exact public tab/record to its admitted native process lifetime."""
    association = state.get("workspace_tree", {}).get("local_app_records", {}).get(tab[1])
    if association is None:
        return None
    require(isinstance(association, dict) and association.get("room") == tab[0]
            and association.get("app_id") == application
            and type(association.get("record_instance")) is int
            and 0 < association["record_instance"] < 2**64,
            "mapped tab has a foreign or invalid launch association")
    proof = state.get("chrome", {}).get("native_window_provenance")
    require(isinstance(proof, dict) and proof.get("method") == "authenticated-wayland-peer"
            and proof.get("omitted") == 0 and isinstance(proof.get("windows"), list),
            "authenticated native worker provenance unavailable")
    rows = [row for row in proof["windows"] if row.get("room") == tab[0]
            and str(row.get("tab_id")) == tab[1] and str(row.get("surface_id")) == tab[2]]
    require(len(rows) <= 1, "ambiguous authenticated native worker")
    if not rows:
        return None
    worker = rows[0]
    association = state["workspace_tree"]["local_app_records"][tab[1]]
    require(worker.get("package_id") == application
            and type(worker.get("record_instance")) is int
            and worker["record_instance"] == association["record_instance"],
            "native worker differs from the public launch record")
    require(type(worker.get("peer_pid")) is int and worker["peer_pid"] > 0
            and isinstance(worker.get("peer_start_time_ticks"), str)
            and worker["peer_start_time_ticks"].isdecimal()
            and int(worker["peer_start_time_ticks"]) > 0
            and isinstance(worker.get("adoption_id"), str)
            and re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", worker["adoption_id"])
            and worker["adoption_id"] != "00000000-0000-0000-0000-000000000000",
            "native process lifetime or adoption unavailable")
    checkout = worker.get("certified_checkout")
    if checkout is None:
        return None  # The admitted worker has not registered a certified directory yet.
    require(isinstance(checkout, dict) and set(checkout) == {"application", "room", "record"}
            and checkout["application"] == application and checkout["room"] == tab[0]
            and isinstance(checkout["record"], str) and 0 < len(checkout["record"].encode()) <= 512
            and not any(ord(char) < 32 for char in checkout["record"]),
            "current certified checkout differs from the native worker")
    return worker


def example_url(value):
    if not isinstance(value, str):
        return False
    try:
        parsed = urlsplit(value)
        return (parsed.scheme == "https" and parsed.hostname == "example.com"
                and parsed.username is None and parsed.password is None and parsed.port in (None, 443))
    except ValueError:
        return False


def browser_flow(door):
    final_url = door.call("navigate", {"url": "https://example.com/"})
    require(example_url(final_url), "HTTPS navigation completion missing")
    settled = door.call("wait-page-settled", {"quiet_ms": 500, "timeout_ms": 10000})
    require(settled.get("data", {}).get("settled") is True, "successful job did not prove a settled page")
    page = door.call("read-page")
    require(page.get("provenance") == "web-content" and page.get("title") == "Example Domain"
            and example_url(page.get("url")) and "Example Domain" in page.get("outline", ""),
            "readable HTTPS page evidence missing")
    return {"binding": door.bound, "url": page["url"], "title": page["title"], "settled": True}


def run(args):
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    vm = TiloVm(str(args.binary.resolve()), args.vm, args.slot, out / "commands.log")
    receipt, shown, installed = cohort(vm, args, out)
    baseline = first_json(vm.run("slot", "exec", "--", "python3", "-c", BASELINE))
    require(baseline["uid"] == args.uid, "unexpected account")
    (out / "baseline.json").write_text(json.dumps(baseline, indent=2))
    flows = args.flow or ["flatpak", "catalogue", "local", "browser"]
    require(len(set(flows)) == len(flows), "duplicate acceptance flow")
    specs = json.loads(args.package_fixtures.read_text()) if args.package_fixtures else {}
    for route in ("catalogue", "local"):
        if route in flows:
            require(route in specs, f"missing genuine {route} package fixture; never grants trust or stages files")
    if "local" in flows:
        require(baseline["trusted_attestor_count"] > 0, "no existing trusted local attestor; positive generic import blocked")
    proof = {"flows": flows, "source": receipt["cohort_source"], "layer": receipt["layer"], "results": {}}
    if any(flow != "browser" for flow in flows):
        door = store(vm, out)
        mapped(vm, "store", installed, args.uid, out)
        if "flatpak" in flows:
            proof["results"]["flatpak"] = flatpak_flow(door, baseline)
        for route in ("catalogue", "local"):
            if route in flows:
                proof["results"][route] = package_flow(door, specs[route], route == "local")
    if "browser" in flows:
        door, tab = fresh_browser(vm, out)
        mapped(vm, "browser", installed, args.uid, out)
        proof["results"]["browser"] = dict(browser_flow(door), tab=tab)
    require(first_json(vm.run("--json", "slot", "show"))["slot"] == shown["slot"], "cohort changed during acceptance")
    (out / "proof.json").write_text(json.dumps(proof, indent=2))
    print(json.dumps(proof))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "cohort-receipt", "out"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("vm", "slot"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--uid", required=True, type=int)
    parser.add_argument("--flow", action="append", choices=("flatpak", "catalogue", "local", "browser"))
    parser.add_argument("--package-fixtures", type=Path)
    run(parser.parse_args())
