#!/usr/bin/env python3
"""Strict Terminal proof in an already activated, owner-controlled cohort slot."""
import argparse
import hashlib
import json
import re
import shlex
import time
import uuid
from pathlib import Path

from acceptance.vm import TiloVm, first_json, wait_until
from acceptance.action_jobs import require, succeeded, invoke

APP = "so.tilo.terminal"


def identity(row):
    return tuple(row.get(k) for k in ("application", "room", "record", "resource"))


def terminal_actions(vm):
    # App action contracts are machine discovery, not human palette suggestions.
    return vm.actions(APP, programmatic=True)


def terminal_rows(rows):
    return [r for r in rows if r.get("application") == APP
            and r.get("semantic_identity") == {"kind": "terminal.session", "key": "terminal.session"}]


def select(rows, operation, bound=None):
    found = [r for r in terminal_rows(rows) if r.get("operation") == operation
             and (bound is None or identity(r) == bound)]
    require(len(found) == 1, f"expected one bound Terminal {operation}, got {len(found)}")
    row = found[0]
    require(row["provider"] == "app-actions" and all(identity(row)), "incomplete public binding")
    require(row.get("contract", {}).get("availability", {}).get("state") == "available",
            f"Terminal {operation} unavailable")
    return row


# Only process identities, mapped runtime ELF and hashes; no argv, environment or documents.
PROCESS_PROBE = r'''
import json,os,pathlib
rows=[]
for p in pathlib.Path('/proc').iterdir():
 if not p.name.isdigit(): continue
 try:
  if p.stat().st_uid!=os.getuid() or (p/'comm').read_text().strip()!='terminal': continue
  rows.append({'pid':int(p.name),'uid':os.getuid()})
 except (FileNotFoundError,ProcessLookupError): continue
print(json.dumps({'uid':os.getuid(),'processes':rows}))
'''


def processes(vm):
    return first_json(vm.run("slot", "exec", "--", "python3", "-c", PROCESS_PROBE))


def run(args):
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    vm = TiloVm(str(args.binary.resolve()), args.vm, args.slot, out / "commands.log")
    receipt = json.loads(args.cohort_receipt.read_text())
    source = receipt.get("cohort_source", {})
    require(receipt.get("status") == "success" and receipt.get("component") == "apps+runtime",
            "successful canonical cohort activation receipt required")
    require(receipt.get("vm") == args.vm and receipt.get("slot") == args.slot,
            "cohort receipt does not address this VM/slot")
    require(re.fullmatch(r"[0-9a-f]{40}", source.get("source_commit", "")) is not None
            and re.fullmatch(r"[0-9a-f]{64}", source.get("dirty_tree_hash", "")) is not None,
            "full source inventory provenance required")
    shown = first_json(vm.run("--json", "slot", "show"))
    require(shown["slot"]["session"] == "active", "slot session inactive")
    layer = receipt["layer"]
    require(shown["slot"]["components"].get("apps+runtime") == layer, "installed cohort differs")
    runtime_files = [f for f in shown["files"] if f["layer"] == layer
                     and f["path"].endswith("/lib/libruntime_dylib.so")]
    require(len(runtime_files) == 1, "closed SDK runtime unobservable or ambiguous")
    expected = runtime_files[0]
    (out / "cohort-files.json").write_text(json.dumps(shown, indent=2))
    before = processes(vm)
    require(before["uid"] == args.uid, "unexpected slot account")
    require(not before["processes"], "Terminal already open; close its own declared tab before fresh proof")
    rows = terminal_actions(vm)
    require(not terminal_rows(rows), "stale Terminal binding present before fresh launch")
    opens = [r for r in vm.actions(APP) if r["provider"] == "applications" and r["id"] == APP]
    require(len(opens) == 1, "normal Terminal Open unavailable")
    vm.invoke("applications", APP)
    rows = wait_until(lambda: (r if terminal_rows(r := terminal_actions(vm)) else None), 45, interval=0.5)
    require(rows is not None, "Terminal resource not published")
    inspect = select(rows, "inspect-session")
    bound = identity(inspect)
    (out / "terminal-contracts.json").write_text(json.dumps(terminal_rows(rows), indent=2))
    info = invoke(vm, inspect, None, out, "inspect")
    require(info and info.get("lifecycle") == "open" and info.get("input_available") is True
            and info.get("transport_disconnected") is False and info.get("target") == {"kind": "environment"},
            "fresh Default session not usable")
    session = info["session"]
    marker = "TILO_TERMINAL_COHORT_" + uuid.uuid4().hex.upper()
    command = "printf '" + marker + "\\n'"
    run_row = select(terminal_actions(vm), "run", bound)
    require(run_row["contract"]["confirmation"]["mode"] == "confirm", "Run confirmation contract changed")
    invoke(vm, run_row, {"command": command, "confirm": True}, out, "run")
    deadline = time.monotonic() + 30
    while True:
        content = invoke(vm, select(terminal_actions(vm), "read", bound), None, out, "read")
        require(content and content.get("session") == session and content.get("target") == {"kind": "environment"},
                "Read changed session/target")
        require(content.get("output_truncated") is False, "output incomplete")
        lines = content.get("output", "").splitlines()
        hits = [i for i, line in enumerate(lines) if line == marker]
        if len(hits) == 1 and any(re.fullmatch(r"\[Default [^\]]*\]\$\s*", l)
                                  for l in lines[hits[0]+1:]):
            break
        require(len(hits) <= 1 and time.monotonic() < deadline, "standalone marker plus returned prompt absent")
        time.sleep(0.5)
    after = processes(vm)
    require(after["uid"] == args.uid and len(after["processes"]) == 1, "fresh Terminal process ambiguous")
    source_probe = Path(__file__).with_name("acceptance").joinpath("runtime_backing.py").read_text()
    backing = first_json(vm.root_exec("python3 -c " + shlex.quote(source_probe) + " terminal " + str(args.uid)))
    require(backing.get("method") == "proc-map-files-fd" and backing.get("reader_uid") == 0
            and backing.get("uid") == args.uid and len(backing.get("processes", [])) == 1,
            "exact mapped Terminal backing unobservable or ambiguous")
    process = backing["processes"][0]
    require(process["pid"] == after["processes"][0]["pid"], "Terminal changed during SDK proof")
    runtime = process["runtime"]
    (out / "terminal-mapped-sdk.json").write_text(json.dumps(backing, indent=2))
    mapped = expected["path"].replace("/usr/lib/tilo-runtime/", "/run/tilo-runtime/", 1)
    installed = {f["path"].replace("/usr/lib/tilo-runtime/", "/run/tilo-runtime/", 1): f["sha256"]
                 for f in shown["files"] if f["layer"] == layer
                 and f["path"].startswith("/usr/lib/tilo-runtime/")}
    require(any(f["path"] == mapped for f in runtime)
            and any("/libstd-" in f["path"] for f in runtime), "native SDK/libstd mappings missing")
    require(all(installed.get(f["path"]) == f["sha256"] for f in runtime),
            "mapped SDK closure differs from installed verified layer")
    final = first_json(vm.run("--json", "slot", "show"))
    require(final["slot"] == shown["slot"], "slot cohort changed during proof")
    proof = {"source": source, "layer": layer, "cohort_receipt_sha256": hashlib.sha256(args.cohort_receipt.read_bytes()).hexdigest(),
             "vm": args.vm, "slot": args.slot, "session": session, "binding": bound,
             "marker": marker, "process": process, "standalone_output_and_returned_prompt": True}
    (out / "proof.json").write_text(json.dumps(proof, indent=2))
    print(json.dumps(proof))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--vm", required=True)
    parser.add_argument("--slot", required=True)
    parser.add_argument("--uid", required=True, type=int)
    parser.add_argument("--cohort-receipt", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    run(parser.parse_args())
