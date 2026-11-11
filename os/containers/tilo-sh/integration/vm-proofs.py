"""What a hosted workspace's VM must be for the Browser in it, held to.

The check runs this on the stack's machine after the agent filled the form:
it runs `integration/vm-proofs.sh` inside the workspace's VM (through the VM
host's own `tilo-legacy hosted run`, the path an operator's check takes) and
prints what it found as a table, failing on anything wrong:

  - every renderer of the engine host is sandboxed: Seccomp 2, NoNewPrivs 1,
    and network, pid and user namespaces of its own;
  - no process runs the engine with `--no-sandbox`;
  - the session and the engine host run as the guest's browser user;
  - the device key file is that user's, and only they may read it;
  - the image's entitlement lists name Browser;
  - what the session, its Chromium and the VM hold in memory.

usage: vm-proofs.py WORKSPACE ACCOUNT OUT_DIR [BROWSER_USER]

The VM host's container is named by VM_HOST_CONTAINER (the stack's
`tilo-sh-vm-host-0`), and the command that reaches a workspace's VM by
VM_HOST_RUN (a prefix that runs the operator's `tilo-legacy` in it).
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

WORKSPACE, ACCOUNT, OUT = sys.argv[1:4]
BROWSER_USER = sys.argv[4] if len(sys.argv) > 4 else "tilo-browser"
CONTAINER = os.environ.get("VM_HOST_CONTAINER", "tilo-sh-vm-host-0")
# What runs the host's own `tilo-legacy` as its service: root on the stack's
# machine reaches the container, which runs the service as uid 10001.
RUN = os.environ.get(
    "VM_HOST_RUN",
    f"podman exec {CONTAINER} setpriv --reuid=10001 --regid=10001 --init-groups "
    "--inh-caps=-all tilo-legacy",
)
SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "vm-proofs.sh")

# Uids the session and the engine must not run as: the tenant and root.
WRONG_USERS = {"root", "tilo"}


def say(step: str) -> None:
    print(f"== {step}", flush=True)


def fail(reason: str) -> None:
    print(f"FAIL: {reason}", file=sys.stderr, flush=True)
    raise SystemExit(1)


def in_guest() -> dict[str, list[str]]:
    """The proof script's fields, as it printed them on the guest's stdout."""
    with open(SCRIPT) as script:
        body = f"BROWSER_USER={BROWSER_USER}\n" + script.read()
    command = [
        *RUN.split(),
        "hosted",
        "run",
        "--run",
        "/run/tilo-hosted",
        "--workspace",
        WORKSPACE,
        "--account",
        ACCOUNT,
        "--",
        "/bin/sh",
        "-s",
    ]
    say(f"running the proofs in {WORKSPACE}'s VM")
    ran = subprocess.run(
        command,
        input=body,
        text=True,
        capture_output=True,
        timeout=900,
    )
    sys.stderr.write(ran.stderr)
    if ran.returncode != 0:
        fail(f"the proofs did not run in the VM ({ran.returncode}): {ran.stdout[-2000:]}{ran.stderr[-2000:]}")
    fields: dict[str, list[str]] = {}
    for line in ran.stdout.splitlines():
        if "\t" not in line:
            continue
        name, _, value = line.partition("\t")
        fields.setdefault(name, []).append(value)
    return fields


def one(fields: dict[str, list[str]], name: str) -> str:
    values = fields.get(name) or []
    return values[0] if values else ""


def kib(value: str) -> int:
    try:
        return int(value)
    except ValueError:
        return 0


def main() -> int:
    fields = in_guest()
    problems: list[str] = []
    rows: list[tuple[str, str, str]] = []

    def row(what: str, found: str, verdict: str) -> None:
        rows.append((what, found, verdict))

    def hold(name: str, found: str, want: str, verdict: str) -> None:
        ok = verdict == "ok"
        row(name, found or "none", "ok" if ok else f"WRONG: want {want}")
        if not ok:
            problems.append(f"{name}: {found or 'none'} (want {want})")

    if one(fields, "root") not in ("yes", "sudo"):
        fail("the proofs could not take root in the VM, so another user's processes are unreadable")

    session = one(fields, "session_pid")
    engine = one(fields, "engine_pid")
    if session in ("", "none"):
        fail("no hosted Browser session runs in the VM: nothing to hold the sandbox to")
    if engine in ("", "none"):
        fail("the Browser's engine host does not run in the VM: the window holds no page")
    row("session", f"pid {session}", "ok")
    row("engine host", f"pid {engine}", "ok")

    session_user = one(fields, "session_user")
    engine_user = one(fields, "engine_user")
    hold("session user", session_user, BROWSER_USER, "ok" if session_user == BROWSER_USER else "wrong")
    hold("engine user", engine_user, BROWSER_USER, "ok" if engine_user == BROWSER_USER else "wrong")
    if session_user in WRONG_USERS or engine_user in WRONG_USERS:
        problems.append(f"the session or engine runs as {session_user}/{engine_user}")

    renderers = fields.get("renderer") or []
    if not renderers:
        fail("the engine host runs no renderer: no page is loaded, so its sandbox proves nothing")
    engine_namespaces = (
        one(fields, "engine_ns_net"),
        one(fields, "engine_ns_pid"),
        one(fields, "engine_ns_user"),
    )
    for renderer in renderers:
        parts = renderer.split(" ")
        if len(parts) < 6:
            problems.append(f"a renderer's line is not readable: {renderer}")
            continue
        pid, _user, seccomp, nonewprivs, net, pidns, userns = parts[:7]
        label = f"renderer {pid}"
        hold(f"{label} Seccomp", seccomp, "2", "ok" if seccomp == "2" else "wrong")
        hold(f"{label} NoNewPrivs", nonewprivs, "1", "ok" if nonewprivs == "1" else "wrong")
        for kind, found, parent in zip(("net", "pid", "user"), (net, pidns, userns), engine_namespaces):
            if "unreadable" in (found, parent):
                problems.append(f"a renderer's {kind} namespace could not be read ({found} vs {parent})")
                row(f"{label} ns {kind}", found, "WRONG: unreadable")
                continue
            same = found == parent
            hold(f"{label} ns {kind}", found, f"its own, not {parent}", "ok" if not same else "wrong")

    sandboxed_off = fields.get("no_sandbox_pid") or []
    hold(
        "no process runs --no-sandbox",
        "; ".join(sandboxed_off)[:200] if sandboxed_off else "none",
        "none",
        "ok" if not sandboxed_off else "wrong",
    )

    key = one(fields, "device_key")
    if key in ("", "none"):
        problems.append("the session's device key file is not on the VM's disk")
        row("device key", "none", "WRONG: the session took no key file")
    else:
        mode = one(fields, "device_key_mode")
        owner = one(fields, "device_key_owner")
        hold("device key", key, "a file of the browser user's", "ok")
        hold("device key mode", mode, "600", "ok" if mode == "600" else "wrong")
        hold("device key owner", owner, BROWSER_USER, "ok" if owner == BROWSER_USER else "wrong")

    for list in ("jit", "web-engine"):
        name = f"entitlement_{list}-entitled"
        found = one(fields, name)
        hold(f"{list}-entitled names Browser", "yes" if found == "yes" else "no", "yes",
             "ok" if found == "yes" else "wrong")
    for missing in fields.get("entitlement_missing") or []:
        problems.append(f"{missing} is not in the image")

    rss, pss = kib(one(fields, "browser_rss_kib")), kib(one(fields, "browser_pss_kib"))
    total, available = kib(one(fields, "vm_mem_total_kib")), kib(one(fields, "vm_mem_available_kib"))
    chromium = one(fields, "chromium_processes")
    row("Chromium processes", chromium or "0", "ok")
    row("Browser memory", f"RSS {rss / 1024:.0f} MiB, PSS {pss / 1024:.0f} MiB", "ok")
    row("VM memory", f"{available / 1024:.0f} MiB free of {total / 1024:.0f} MiB", "ok")
    if rss > 1100 * 1024:
        problems.append(f"the session and its Chromium hold {rss / 1024:.0f} MiB RSS, past the VM's 2 GiB")

    width = max(len(what) for what, _, _ in rows)
    print()
    for what, found, verdict in rows:
        print(f"  {what:<{width}}  {found:<60}  {verdict}")
    print()

    findings = {
        "session_pid": session,
        "engine_pid": engine,
        "renderers": len(renderers),
        "chromium_processes": int(chromium or 0),
        "browser_rss_kib": rss,
        "browser_pss_kib": pss,
        "vm_mem_total_kib": total,
        "vm_mem_available_kib": available,
        "device_key": key,
        "problems": problems,
        "table": [
            {"what": what, "found": found, "verdict": verdict} for what, found, verdict in rows
        ],
    }
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "vm-proofs.json"), "w") as saved:
        json.dump(findings, saved, indent=2)
    print(json.dumps({key: findings[key] for key in ("renderers", "browser_rss_kib", "browser_pss_kib")}))

    if problems:
        fail("the VM is not what a hosted Browser needs:\n  " + "\n  ".join(problems))
    say("the VM is what a hosted Browser needs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
