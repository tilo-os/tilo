#!/usr/bin/env python3
"""Semantic Store/Browser acceptance in an already activated owned SDK cohort.

Never stages packages, grants trust, closes windows, restarts sessions or reads Notes.
Run only after the deployment owner hands off the matched cohort.
"""
import argparse
import copy
import re
import json
import shlex
import time
from pathlib import Path
from urllib.parse import urlsplit

from acceptance.action_jobs import invoke, require
from acceptance.action_discovery import discover, semantic_identity
from acceptance.cohort import selected_layer, runtime_inventory
from acceptance.vm import TiloVm, first_json, wait_until

STORE = "so.tilo.store"
BROWSER = "so.tilo.browser"
CALCULATOR = "org.gnome.Calculator"


def binding(row):
    return tuple(row.get(k) for k in ("application", "room", "record", "resource"))


def catalog(vm, app, bound=None, family=None):
    return discover(lambda arguments: vm.control_json(*arguments), app, bound, family)


def choose(rows, app, kind, operation, args, bound=None, semantic=None):
    found = []
    for row in rows:
        if (row.get("provider") != "app-actions" or row.get("application") != app
                or not (identity := semantic_identity(row)) or identity[0] != kind
                or row.get("operation") != operation
                or (bound is not None and binding(row)[:3] != bound[:3])
                or (semantic is not None and identity != semantic)
                or (semantic is None and bound is not None and binding(row) != bound)):
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


def native_worker_identity(subject):
    """Retain every authenticated anchor; focus/layout can change input geometry."""
    if subject is None:
        return None
    return {key: value for key, value in subject.items() if key != "input_rect"}


class AppDoor:
    def __init__(self, vm, app, kind, bound, out, semantic=None, subject=None):
        self.vm, self.app, self.kind, self.bound, self.out = vm, app, kind, bound, out
        self.sequence = 0
        self.tab = self.worker = None
        self.semantic = semantic
        if semantic is not None:
            require(isinstance(semantic, tuple) and len(semantic) == 2
                    and semantic_identity({"semantic_identity": {"kind": semantic[0], "key": semantic[1]}}) == semantic
                    and semantic[0] == kind, "invalid original resource semantic identity")
        if subject is not None:
            require(isinstance(subject, dict), "invalid original worker subject")
            self.worker = copy.deepcopy(subject)
            self.tab = (subject.get("room"), str(subject.get("tab_id")), str(subject.get("surface_id")))
        if vm is not None:
            require(len(bound) == 4 and all(isinstance(part, str) and part for part in bound)
                    and bound[0] == app, "incomplete original application binding")
            require(wait_until(self._adopt, 60) is True,
                    "owned worker semantic resource never became observable")

    def _adopt(self):
        if self.worker is None:
            state = self.vm.state()
            tabs = [tab for tab in windows(state, self.app)
                    if tab[0] == self.bound[1] and mapped_record(state, tab, self.app) == self.bound[2]]
            require(len(tabs) <= 1, "original application record has ambiguous mapped windows")
            if not tabs:
                return None
            worker = native_subject(state, tabs[0], self.app)
            if worker is None:
                return None
            self.tab, self.worker = tabs[0], copy.deepcopy(worker)
        self._check_custody()
        rows = catalog(self.vm, self.app, self.bound, self.kind)
        identities = {identity for row in rows
                      if row.get("provider") == "app-actions" and binding(row)[:3] == self.bound[:3]
                      and (identity := semantic_identity(row)) and identity[0] == self.kind
                      and ((self.semantic is None and binding(row) == self.bound)
                           or identity == self.semantic)}
        require(len(identities) <= 1, "original worker has ambiguous semantic child resources")
        self._check_custody()
        if not identities:
            return None
        self.semantic = next(iter(identities))
        return True

    def _check_custody(self):
        require(self.worker is not None and self.tab is not None,
                "original authenticated worker custody unavailable")
        state = self.vm.state()
        require(state.get("chrome", {}).get("session_locked") is False,
                "original owner session is not observably unlocked")
        require(self.tab[0] == self.bound[1] and mapped_record(state, self.tab, self.app) == self.bound[2],
                "original worker differs from the bound room or record")
        require(native_worker_identity(native_subject(state, self.tab, self.app)) == native_worker_identity(self.worker),
                "original authenticated worker custody changed")

    def call(self, operation, args=None, timeout=120):
        args = dict(args or {})
        self._check_custody()
        row = choose(catalog(self.vm, self.app, self.bound, self.kind), self.app, self.kind, operation, args,
                     self.bound, self.semantic)
        self._check_custody()
        self.bound = binding(row)
        if row.get("contract", {}).get("confirmation", {}).get("mode") == "confirm":
            args["confirm"] = True
        self.sequence += 1
        (self.out / f"{self.app}-{self.sequence}-contract.json").write_text(json.dumps(row, indent=2))
        value = invoke(self.vm, row, args, self.out, f"{self.app}-{self.sequence}-{operation}", timeout)
        self._check_custody()
        return value

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
    shown = first_json(vm.run("--json", "slot", "show"))
    layer = selected_layer(receipt, shown, args.vm, args.slot)
    installed = runtime_inventory(shown, layer)
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
    return AppDoor(vm, STORE, "store.catalog", binding(row), out, semantic_identity(row))


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
    door.call("open", {"app": spec["app"]})
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
    invoke(vm, opens[0], {"app": BROWSER, "background": True}, out, "new-browser-tab", require_job=False)

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
            rows = [r for r in catalog(vm, BROWSER, (BROWSER, tab[0], record), "browser.page")
                    if (r.get("semantic_identity") or {}).get("kind") == "browser.page"
                    and r.get("room") == tab[0] and r.get("record") == record]
            bound = {binding(r) for r in rows}
            require(len(bound) <= 1, "new tab published ambiguous page bindings")
            if bound:
                identity = {semantic_identity(row) for row in rows}
                require(len(identity) == 1 and None not in identity,
                        "fresh Browser semantic identity unavailable or ambiguous")
                return next(iter(bound)), tab, next(iter(identity))
        return None

    result = wait_until(probe, 60)
    require(result, "queued Open did not create a new mapped tab and record; existing page left untouched")
    return AppDoor(vm, BROWSER, "browser.page", result[0], out, result[2]), result[1]


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


def browser_executable(shown, layer):
    rows = [row for row in shown.get("files", []) if row.get("component") == "apps+runtime"
            and row.get("layer") == layer and isinstance(row.get("path"), str)
            and row["path"].startswith("/usr/share/tilo-apps/") and row["path"].endswith("/bin/browser")]
    require(len(rows) == 1 and "link" not in rows[0]
            and isinstance(rows[0].get("sha256"), str) and re.fullmatch(r"[0-9a-f]{64}", rows[0]["sha256"]),
            "selected signed cohort Browser executable unavailable or ambiguous")
    return rows[0]["sha256"]


HTTPS_FIXTURE = json.loads(Path(__file__).resolve().parents[1].joinpath("browser/fixtures/https-functional.json").read_text())


def fixture_url(value):
    if not isinstance(value, str):
        return False
    try:
        observed, expected = urlsplit(value), urlsplit(HTTPS_FIXTURE["url"])
        return (observed.scheme == "https" and observed.hostname == expected.hostname
                and observed.username is None and observed.password is None
                and observed.port in (None, 443) and observed.path == expected.path
                and not observed.query and not observed.fragment)
    except ValueError:
        return False


def verify_https_page(page):
    require(isinstance(page, dict) and page.get("provenance") == "web-content"
            and page.get("title") == HTTPS_FIXTURE["title"] and fixture_url(page.get("url")),
            "readable HTTPS page identity missing")
    outline = page.get("outline")
    require(isinstance(outline, str), "HTTPS web-content outline missing")
    lines = [line.lstrip() for line in outline.splitlines()]
    heading = '- heading ' + json.dumps(HTTPS_FIXTURE["heading"]) + ' level=1'
    require(heading in lines, "HTTPS fixture H1 not read from the live document")
    for link in HTTPS_FIXTURE["links"]:
        pattern = r'^- listitem: link ' + re.escape(json.dumps(link["name"])) + r' \[ref=d[0-9]+e[0-9]+\] -> ' + re.escape(link["url"]) + r'$'
        require(any(re.fullmatch(pattern, line) for line in lines), "HTTPS fixture live link/reference/target missing")


def browser_flow(door):
    final_url = door.call("navigate", {"url": HTTPS_FIXTURE["url"]})
    require(fixture_url(final_url), "HTTPS navigation completion missing")
    settled = door.call("wait-page-settled", {"quiet_ms": 500, "timeout_ms": 10000})
    require(settled.get("data", {}).get("settled") is True, "successful job did not prove a settled page")
    page = door.call("read-page")
    verify_https_page(page)
    return {"binding": door.bound, "url": page["url"], "title": page["title"], "settled": True,
            "content_verified": "H1 and two live links with exact targets", "latency_claim": False}


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
    proof = {"flows": flows, "source": receipt["cohort_source"], "layer": shown["slot"]["components"]["apps+runtime"], "results": {}}
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
        worker = wait_until(lambda: browser_subject(vm.state(), tab), 60)
        require(worker, "new Browser has no authenticated observable native worker")
        mapped(vm, "browser", installed, args.uid, out,
               executable=browser_executable(shown, shown["slot"]["components"]["apps+runtime"]), selected=worker)
        require(browser_subject(vm.state(), tab) == worker, "Browser association changed during backing audit")
        proof["results"]["browser"] = dict(browser_flow(door), tab=tab, worker=worker)
        require(browser_subject(vm.state(), tab) == worker, "Browser association changed during HTTPS acceptance")
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
