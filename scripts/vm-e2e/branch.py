#!/usr/bin/env python3
"""Branching a configured box, end to end, on real VMs.

    scripts/vm-e2e/branch.py --disk DEV.qcow2 --out DIR --taps BOX,A,B [--tilo-vm PATH] [--hold SECONDS]

tools/ci/vm-e2e-branch.sh runs it as the e2e-branch check, with the taps of the boxes' local
network.

A person sets one box up once, with their keys, and branches boxes off it that start
configured and then work on their own (`tilo-vm vm branch`, docs/architecture/workspace.md,
"A machine copied from another"). This proves it with this tree's dev disk and tilo-vm:

1. `box` boots and records its machine; a person signs in as the VM's person
   (`slot create --identity device`: the session's own onboarding creates the identity in the
   machine's identity tree and certifies the machine) and sets the box up: a Secret Service
   secret, a model provider's API key in their vault, a login in their passwords vault,
   `~/.gitconfig`, an SSH key, and a workspace of their own with a window on its desk.
2. `box` is stopped and branched into `a` and `b`, all three on one local network beside their
   own way out; they boot (b once box has done its part, where the host has no room for three).
3. Each branch has a device key of its own (the source's and the other branch's differ), a
   machine record of its own UUID, a valid certificate under the same person's root, the
   person's secret, API key, passwords vault, SSH key and git config readable as them, and a
   machine id and SSH host keys of its own; `box` keeps its own.
4. `box` and `a` hold the workspace: a window opened on either shows on the other, also
   after both reboot.
5. `vm rm box` leaves `a` and `b` bootable.

Everything goes through `tilo-vm` (scripts/vm-acceptance/acceptance/vm.py): `slot exec` as
the person, `vm ssh` as root, `control` for the desktop. The report (report.json,
report.md) and evidence go to --out. No secret value is printed: values are compared with
what was stored inside the guest.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import shlex
import subprocess
import sys
import time
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "vm-acceptance"))

from acceptance.vm import TiloVm, VmError, first_json, wait_until  # noqa: E402

SLOT = "me"
SOURCE, BRANCHES = "box", ("a", "b")
NAMES = (SOURCE, *BRANCHES)
# The boxes' local network (tools/ci/vm-e2e-branch.sh's bridge): each box's address on it and
# the MAC of its NIC there.
LAN = {name: (f"10.78.0.{n}/24", f"52:54:00:e2:b0:0{n}") for n, name in enumerate(NAMES, start=1)}
OUTPUT = "1280x800"
WORKSPACE = "Branch e2e"
# What the person stores: names only, never the values, appear in the report.
SECRET_ATTRS = {"service": "tilo-e2e-branch", "account": "person"}
GIT_NAME, GIT_EMAIL = "Ada Branch", "ada@branch.example"
PASSWORD_TITLE = "Branch e2e mail"
PROVIDER_LABEL = "Branch e2e"
PROVIDER_MODEL = "e2e-branch"
MODEL_PORT = 18431


class Failed(Exception):
    """A step observed something wrong."""


def expect(cond, message: str) -> None:
    if not cond:
        raise Failed(message)


class Run:
    def __init__(self, binary: str, out: Path, taps: dict[str, str]):
        self.binary = binary
        self.out = out
        self.taps = taps
        self.steps: list[dict] = []
        self.vms: dict[str, TiloVm] = {}
        self.facts: dict = {}
        (out / "data").mkdir(parents=True, exist_ok=True)

    def vm(self, name: str) -> TiloVm:
        if name not in self.vms:
            self.vms[name] = TiloVm(self.binary, name, SLOT, self.out / f"tilo-vm-{name}.log")
        return self.vms[name]

    def write(self, name: str, text: str) -> None:
        (self.out / "data" / name).write_text(text)

    def step(self, sid: str, title: str, fn, *args):
        started = time.monotonic()
        entry = {"id": sid, "title": title, "status": "pass", "detail": "", "facts": {}}
        print(f"{sid} ... {title}", flush=True)
        try:
            value = fn(entry["facts"], *args)
        except (Failed, VmError) as exc:
            entry["status"], entry["detail"] = "fail", str(exc)[:4000]
            value = None
        except Exception as exc:  # noqa: BLE001 - a broken step is reported, the run goes on
            entry["status"] = "fail"
            entry["detail"] = f"{type(exc).__name__}: {exc}\n{traceback.format_exc()[-3000:]}"
            value = None
        entry["seconds"] = round(time.monotonic() - started, 1)
        self.steps.append(entry)
        print(f"{sid} {entry['status'].upper():4} {entry['seconds']:7.1f}s  {title}"
              + (f"\n      {entry['detail']}" if entry["detail"] else ""), flush=True)
        return value

    def passed(self, sid: str) -> bool:
        return any(s["id"] == sid and s["status"] == "pass" for s in self.steps)


# -- what the guest says --------------------------------------------------------

# As root: the machine's names and keys, by digest; the device key's public half where the
# image's Python can derive it. No private byte leaves the guest.
MACHINE_FACTS = r"""
cd /var/lib/tilo
echo "UUID=$(tr -d '-' < /sys/class/dmi/id/product_uuid | tr 'A-F' 'a-f')"
[ -f device-private/machine.bin ] && echo "RECORD=$(od -An -tx1 -v device-private/machine.bin | tr -d ' \n')"
[ -f device-private/keypair.bin ] && echo "KEY-DIGEST=$(sha256sum < device-private/keypair.bin | cut -c1-16)"
[ -f device-private/branch.bin ] && echo "JOURNAL=present"
echo "DEVICE-PUBLIC=$(python3 -I -c '
import sys
try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives import serialization
except ImportError:
    sys.exit(0)
seed = open("device-private/keypair.bin", "rb").read()
key = Ed25519PrivateKey.from_private_bytes(seed).public_key()
print(key.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex())' 2>/dev/null)"
[ -f user-identity/pubkey.bin ] && echo "USER-PUBLIC=$(od -An -tx1 -v user-identity/pubkey.bin | tr -d ' \n')"
[ -f user-identity/privkey.bin ] && echo "ROOT-HERE=yes"
for c in device/certs/*.bin; do [ -f "$c" ] && echo "CERT=$(basename "$c" .bin)"; done
[ -f device/branch-label ] && echo "PENDING-LABEL=$(head -c 64 device/branch-label)"
echo "MACHINE-ID=$(cat /etc/machine-id)"
for k in /etc/ssh/ssh_host_*_key.pub; do [ -f "$k" ] && echo "HOST-KEY=$(ssh-keygen -lf "$k" | awk '{print $2}')"; done
echo "BRANCH-UNIT=$(systemctl show tilo-device-branch.service -p ActiveState -p Result --value | paste -sd, -)"
"""

# As the person, in their session: their identity and this device's certificate, what the
# Secret Service gives back for the secret and the API key (as digests), and their files.
PERSON_FACTS = r"""
echo "USER=$(id -un)"
echo "IDENTITY-BEGIN"; tilo-identity --json status 2>&1; echo "IDENTITY-END"
echo "STORE=$(tilo-secret-service status 2>&1 | head -1)"
echo "SECRET-DIGEST=$(python3 -I "$XDG_RUNTIME_DIR/e2e-secret.py" get __SERVICE__ __ACCOUNT__ | sha256sum | cut -c1-16)"
echo "GIT-NAME=$(git config --global user.name)"
echo "GIT-EMAIL=$(git config --global user.email)"
[ -f ~/.ssh/id_ed25519 ] && echo "SSH-KEY=$(ssh-keygen -lf ~/.ssh/id_ed25519 | awk '{print $2}')"
[ -f ~/.ssh/id_ed25519 ] && echo "SSH-KEY-READABLE=$(head -c 35 ~/.ssh/id_ed25519)"
"""

# The Secret Service as libsecret and secret-tool speak it (org.freedesktop.secrets, plain
# session): store a secret under attributes, or read one back by attributes. Run as the person.
SECRET_CLIENT = r'''
import sys
from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
NAME, PATH = "org.freedesktop.secrets", "/org/freedesktop/secrets"


def call(path, iface, method, args, reply):
    return bus.call_sync(NAME, path, iface, method, args, GLib.VariantType(reply),
                         Gio.DBusCallFlags.NONE, 30000, None).unpack()


_, session = call(PATH, "org.freedesktop.Secret.Service", "OpenSession",
                  GLib.Variant("(sv)", ("plain", GLib.Variant("s", ""))), "(vo)")


def unlock(paths):
    if paths:
        call(PATH, "org.freedesktop.Secret.Service", "Unlock", GLib.Variant("(ao)", (paths,)), "(aoo)")


def search(attrs):
    unlocked, locked = call(PATH, "org.freedesktop.Secret.Service", "SearchItems",
                            GLib.Variant("(a{ss})", (attrs,)), "(aoao)")
    unlock(locked)
    return unlocked + locked


def get(attrs):
    items = search(attrs)
    if not items:
        sys.exit("no such item")
    (secret,) = call(items[0], "org.freedesktop.Secret.Item", "GetSecret",
                     GLib.Variant("(o)", (session,)), "((oayays))")
    sys.stdout.buffer.write(bytes(secret[2]))


def store(attrs, label, value):
    collection = "/org/freedesktop/secrets/aliases/default"
    props = {"org.freedesktop.Secret.Item.Label": GLib.Variant("s", label),
             "org.freedesktop.Secret.Item.Attributes": GLib.Variant("a{ss}", attrs)}
    secret = (session, b"", value, "text/plain")
    call(collection, "org.freedesktop.Secret.Collection", "CreateItem",
         GLib.Variant("(a{sv}(oayays)b)", (props, secret, True)), "(oo)")


cmd = sys.argv[1]
if cmd == "store":
    store({"service": sys.argv[2], "account": sys.argv[3]}, "tilo e2e branch", sys.stdin.buffer.read())
elif cmd == "get":
    get({"service": sys.argv[2], "account": sys.argv[3]})
'''


# As root: a loopback OpenAI-compatible endpoint that lists one model to a caller presenting
# the API key whose digest is in /run/e2e-model.digest, and refuses anyone else. It logs
# whether each request's key was the one stored, never the key.
MODEL_SERVER = r'''
import hashlib, http.server, json, sys
want = open("/run/e2e-model.digest").read().strip()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        auth = self.headers.get("authorization") or ""
        key = auth[7:] if auth.lower().startswith("bearer ") else auth
        ok = bool(key) and hashlib.sha256(key.encode()).hexdigest() == want
        with open("/run/e2e-model.log", "a") as log:
            log.write(f"{self.path} key={'stored' if ok else ('other' if key else 'none')}\n")
        body = json.dumps({"object": "list", "data": [{"id": "__MODEL__", "object": "model"}]} if ok
                          else {"error": {"message": "invalid api key"}}).encode()
        self.send_response(200 if ok else 401)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
'''


def fields(text: str) -> dict:
    out: dict = {}
    for line in text.splitlines():
        key, sep, value = line.partition("=")
        if sep and key.replace("-", "").isalpha() and key.isupper():
            if key in out:
                out[key] = (out[key] if isinstance(out[key], list) else [out[key]]) + [value.strip()]
            else:
                out[key] = value.strip()
    return out


def as_list(value) -> list:
    return value if isinstance(value, list) else ([value] if value else [])


def machine_facts(run: Run, name: str, label: str) -> dict:
    text = run.vm(name).root_exec(MACHINE_FACTS, timeout=120)
    run.write(f"{label}-machine.txt", text)
    f = fields(text)
    f["HOST-KEY"] = sorted(as_list(f.get("HOST-KEY")))
    f["CERT"] = sorted(as_list(f.get("CERT")))
    return f


def install_secret_client(vm: TiloVm) -> None:
    vm.exec('cat > "$XDG_RUNTIME_DIR/e2e-secret.py" <<\'EOF\'\n' + SECRET_CLIENT + "\nEOF\n")


def person_facts(run: Run, name: str, label: str) -> dict:
    vm = run.vm(name)
    install_secret_client(vm)
    script = (PERSON_FACTS.replace("__SERVICE__", SECRET_ATTRS["service"])
              .replace("__ACCOUNT__", SECRET_ATTRS["account"]))
    text = vm.exec(script, timeout=120, check=False)
    run.write(f"{label}-person.txt", text)
    f = fields(text)
    raw = text.split("IDENTITY-BEGIN", 1)[-1].split("IDENTITY-END", 1)[0].strip()
    try:
        f["identity"] = first_json(raw)
    except VmError:
        f["identity"] = {"unparsed": raw[-600:]}
    return f


# The digest of nothing: what a failed read prints.
EMPTY_DIGEST = "e3b0c44298fc1c14"


# -- the steps ---------------------------------------------------------------------


def mem_available_mb() -> int:
    for line in Path("/proc/meminfo").read_text().splitlines():
        if line.startswith("MemAvailable:"):
            return int(line.split()[1]) // 1024
    return 0


# What one guest gets, and what the host keeps beside them.
GUEST_MB, HOST_MB = 3072, 1536


def boot_box(facts: dict, run: Run, disk: Path, memory: int) -> None:
    box = run.vm(SOURCE)
    facts["memory_mb"] = memory
    box.vm_cmd("create", SOURCE, "--image", str(disk), "--heads", "1", "--resolution", OUTPUT,
               "--memory-mb", str(memory), "--cpus", "2", timeout=900)
    box.vm_cmd("start", SOURCE, timeout=900)
    m = machine_facts(run, SOURCE, "S01-box")
    facts["machine"] = {k: m.get(k) for k in ("UUID", "RECORD", "BRANCH-UNIT")}
    expect(m.get("UUID"), "box reports no product UUID")
    expect(m.get("RECORD") == m.get("UUID"),
           f"box did not record its machine at boot: machine.bin {m.get('RECORD')!r}, "
           f"product_uuid {m.get('UUID')!r} (tilo-device-branch.service: {m.get('BRANCH-UNIT')})")


def wait_session(vm: TiloVm, timeout: float = 300) -> None:
    """Until the person's session answers and lists its apps; a slot whose session did not
    come up with the box is started as tilo-vm starts it."""
    try:
        vm.wait_catalogue(timeout)
    except VmError:
        vm.run("slot", "restart", SLOT, timeout=300, slot=False)
        try:
            vm.wait_catalogue(timeout)
        except VmError as exc:
            raise Failed(f"{vm.vm}'s session never answered with its apps: {exc}") from exc


# -- the boxes' local network --------------------------------------------------------


def virsh(*args: str, check: bool = True) -> str:
    proc = subprocess.run(["virsh", "--connect", "qemu:///session", *args],
                          capture_output=True, text=True, timeout=120)
    if check and proc.returncode != 0:
        raise Failed(f"virsh {' '.join(args)}: {proc.stderr.strip() or proc.stdout.strip()}")
    return proc.stdout


def plug_lan(run: Run, name: str) -> None:
    """Put the stopped `name` on the boxes' network: its tap, as a second NIC."""
    ip, mac = LAN[name]
    nic = run.out / "data" / f"lan-{name}.xml"
    nic.write_text(f"<interface type='ethernet'><mac address='{mac}'/>"
                   f"<target dev='{run.taps[name]}' managed='no'/><model type='virtio'/></interface>\n")
    virsh("attach-device", name, str(nic), "--config")


def address_lan(run: Run, name: str) -> str:
    """Give `name` its address on the boxes' network, as a person sets one by hand."""
    ip, mac = LAN[name]
    return run.vm(name).root_exec(f"""
set -e
dev=$(ip -o link | awk -v mac={mac} 'index($0, mac) {{ sub(":$", "", $2); print $2; exit }}')
[ -n "$dev" ] || {{ echo "no interface with {mac}"; ip -o link; exit 1; }}
if command -v nmcli >/dev/null 2>&1 && nmcli -t general status >/dev/null 2>&1; then
  nmcli con delete tilo-e2e >/dev/null 2>&1 || true
  nmcli con add type ethernet ifname "$dev" con-name tilo-e2e ipv4.method manual \
    ipv4.addresses {ip} ipv6.method link-local >/dev/null
  nmcli con up tilo-e2e >/dev/null
else
  ip link set "$dev" up
  ip addr replace {ip} dev "$dev"
fi
ip -o addr show dev "$dev"
""", timeout=120)


def sign_in(facts: dict, run: Run) -> None:
    box = run.vm(SOURCE)
    out = box.run("--json", "slot", "create", SLOT, "--headful", "--outputs", f"{OUTPUT}@1",
                  "--identity", "device", timeout=900, slot=False)
    run.write("S02-slot-create.json", out)
    wait_session(box)
    p = person_facts(run, SOURCE, "S02-box")
    ident = p.get("identity") or {}
    facts["identity"] = ident
    expect((ident.get("identity") or {}).get("id"), f"the person has no identity: {ident}")
    expect(ident.get("this_device"), f"box holds no valid certificate for the person: {ident}")
    m = machine_facts(run, SOURCE, "S02-box")
    expect(m.get("ROOT-HERE") == "yes",
           "the person's root is not in the machine's identity tree, so a branch cannot "
           "certify itself at boot")


def configure(facts: dict, run: Run) -> None:
    """A secret in their Secret Service (as `secret-tool store` puts one), git and SSH."""
    box = run.vm(SOURCE)
    install_secret_client(box)
    have_tool = box.exec("command -v secret-tool || true").strip()
    facts["secret_tool"] = bool(have_tool)
    value = "e2e-branch-secret-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \\n')"
    if have_tool:
        box.exec(f'printf %s "{value}" | secret-tool store --label "tilo e2e branch" '
                 f'service {SECRET_ATTRS["service"]} account {SECRET_ATTRS["account"]}')
    else:
        box.exec(f'printf %s "{value}" | python3 -I "$XDG_RUNTIME_DIR/e2e-secret.py" store '
                 f'{SECRET_ATTRS["service"]} {SECRET_ATTRS["account"]}')
    box.exec(f"git config --global user.name {shlex.quote(GIT_NAME)} && "
             f"git config --global user.email {shlex.quote(GIT_EMAIL)} && "
             "mkdir -p -m 700 ~/.ssh && ssh-keygen -q -t ed25519 -N '' -C ada@box -f ~/.ssh/id_ed25519")
    p = person_facts(run, SOURCE, "S03-box")
    facts["person"] = {k: p.get(k) for k in ("SECRET-DIGEST", "GIT-NAME", "SSH-KEY")}
    expect(p.get("SECRET-DIGEST") not in (None, EMPTY_DIGEST), "the stored secret does not read back")
    expect(p.get("GIT-NAME") == GIT_NAME, "git config did not take")
    expect(p.get("SSH-KEY"), "no SSH key")


def configure_provider(facts: dict, run: Run) -> None:
    """A model provider's API key, saved through Settings into their vault."""
    box = run.vm(SOURCE)
    key = "sk-e2e-" + os.urandom(18).hex()
    digest = hashlib.sha256(key.encode()).hexdigest()
    serve_model(box, digest)
    add_provider(box, key)
    facts["provider"] = provider_answers(box)
    facts["api_key_digest"] = digest


def configure_passwords(facts: dict, run: Run) -> None:
    """A login in their passwords vault, through Passwords' own controls."""
    box = run.vm(SOURCE)
    open_passwords(box)
    before = password_items(box)
    app_action(box, "", "add")
    box.control("click", "passwords.field.name")
    box.input("type", PASSWORD_TITLE)
    app_action(box, "", "generate")
    app_action(box, "", "save")
    new = wait_until(lambda: password_items(box) - before, 30, interval=1.0)
    expect(new, "Passwords lists no new item after the save")
    facts["password_items"] = sorted(new)


def open_passwords(vm: TiloVm) -> None:
    vm.invoke("applications", "so.tilo.passwords")
    expect(wait_until(lambda: app_rows(vm, "", "add"), 60, interval=1.0),
           f"Passwords on {vm.vm} offers no add")


def password_items(vm: TiloVm) -> set[str]:
    """The vault items Passwords publishes rows for, by their resource ids."""
    listed = vm.control_json("actions", "list", "--json", "--programmatic", "--wait=8000",
                             "--limit", "2000")
    return {part for row in listed.get("actions", []) if row.get("provider") == "app-actions"
            for part in row["id"].split("\x1f") if part.startswith("passwords:item:")}


def configure_workspace(facts: dict, run: Run) -> None:
    """A workspace of their own, hosted on this device, with a window on its desk."""
    box = run.vm(SOURCE)
    ws = make_workspace(box)
    facts["workspace"] = ws
    facts["window_on_box"] = open_window(box, ws)


# -- a model provider's API key, through Settings ------------------------------------


def serve_model(vm: TiloVm, key_digest: str) -> None:
    """The loopback endpoint the provider points at, as root, for this boot."""
    vm.root_exec(f"printf %s {key_digest} > /run/e2e-model.digest && rm -f /run/e2e-model.log && "
                 "cat > /run/e2e-model.py <<'EOF'\n" + MODEL_SERVER.replace("__MODEL__", PROVIDER_MODEL)
                 + "\nEOF\nsystemctl stop e2e-model.service 2>/dev/null; systemctl reset-failed e2e-model.service "
                 f"2>/dev/null; systemd-run --unit e2e-model --quiet python3 -I /run/e2e-model.py {MODEL_PORT}")


def app_rows(vm: TiloVm, query: str, action: str) -> list[dict]:
    """The app actions named `action` (the last part of a row's id) the desktop lists."""
    listed = vm.control_json("actions", "list", *([query] if query else []), "--json", "--programmatic",
                             "--wait=8000", "--limit", "2000")
    return [row for row in listed.get("actions", [])
            if row.get("provider") == "app-actions" and row["id"].split("\x1f")[-1] == action]


def app_action(vm: TiloVm, query: str, action: str, args: dict | None = None, part: str = "") -> dict:
    """Invoke the app action `action` a live window offers (else its directory's row), as the
    agent-chat e2e does, and its answer once it succeeded."""
    rows = sorted((r for r in app_rows(vm, query, action) if part in r["id"]),
                  key=lambda row: row["id"].startswith("directory"))
    expect(rows, f"{vm.vm} offers no {action} (asked for {query!r})")
    argv = ["actions", "invoke", rows[0]["provider"], rows[0]["id"], "--wait=20000"]
    if args is not None:
        argv += ["--args-json", json.dumps(args)]
    answer = vm.control_json(*argv, timeout=90)
    state = (answer.get("receipt") or {}).get("update", {}).get("state")
    expect(state == "succeeded", f"{action} did not succeed on {vm.vm}: {json.dumps(answer)[:800]}")
    return answer


def record_part(row_id: str) -> str:
    parts = row_id.split("\x1f")
    return parts[-2] if len(parts) >= 2 else ""


def open_provider_settings(vm: TiloVm) -> None:
    vm.control_json("actions", "invoke", "settings-pages", "page:agent_models", "--wait=20000", timeout=90)
    expect(wait_until(lambda: app_rows(vm, "Add provider", "add-provider"), 60, interval=1.0),
           f"Settings on {vm.vm} offers no add-provider")


def provider_part(vm: TiloVm) -> str | None:
    rows = app_rows(vm, PROVIDER_LABEL, "list-provider-models")
    return record_part(rows[0]["id"]) if rows else None


def add_provider(vm: TiloVm, key: str) -> str:
    """An OpenAI-compatible provider at the loopback endpoint, with `key` saved in the
    person's vault through Settings' own actions. Its record part."""
    open_provider_settings(vm)
    before = {r["id"] for r in app_rows(vm, "", "set-provider-details")}
    app_action(vm, "Add provider", "add-provider", {"preset": "openai-compatible"})
    new = wait_until(lambda: [r for r in app_rows(vm, "", "set-provider-details") if r["id"] not in before],
                     30, interval=1.0)
    expect(new, "Settings shows no row for the provider it added")
    part = record_part(new[0]["id"])
    app_action(vm, "", "set-provider-details", {"label": PROVIDER_LABEL,
               "endpoint": f"http://127.0.0.1:{MODEL_PORT}/v1", "model": PROVIDER_MODEL}, part)
    app_action(vm, "", "save-provider-key", {"key": key}, part)
    return part


def provider_answers(vm: TiloVm) -> dict:
    """Settings' Test connection for the provider: the gateway asks the endpoint for its models
    with the key it reads from the person's vault. Whether the endpoint saw the stored key."""
    open_provider_settings(vm)
    part = wait_until(lambda: provider_part(vm), 30, interval=1.0)
    expect(part, f"Settings on {vm.vm} lists no provider {PROVIDER_LABEL!r}")
    answer = app_action(vm, "", "list-provider-models", None, part)
    log = vm.root_exec("cat /run/e2e-model.log 2>/dev/null; true")
    expect("key=stored" in log, f"the endpoint never saw the stored key from {vm.vm}: {log.strip()!r}; "
                                f"the action answered {json.dumps(answer)[:600]}")
    expect(PROVIDER_MODEL in json.dumps(answer), f"Test connection listed no {PROVIDER_MODEL}: "
                                                 f"{json.dumps(answer)[:600]}")
    return {"endpoint_saw": sorted(set(log.split()))[:6]}


# -- the workspace and its desk -------------------------------------------------------


def workspaces(state: dict) -> list[dict]:
    return (state.get("workspace_tree") or {}).get("workspaces") or []


def held(state: dict, wid) -> bool:
    return wid in {row.get("workspace") for row in state.get("held_workspaces") or []}


def make_workspace(vm: TiloVm) -> dict:
    before = {w["id"] for w in workspaces(vm.state())}
    vm.invoke("actions", "workspace.create", {"name": WORKSPACE})
    made = wait_until(lambda: next((w for w in workspaces(vm.state()) if w["id"] not in before), None),
                      30, interval=0.5)
    expect(made, "workspace.create made no workspace")
    select_workspace(vm, made["room"])
    expect(wait_until(lambda: held(vm.state(), made["id"]), 60, interval=1.0),
           f"box does not hold the workspace it made: {made}")
    return {"id": made["id"], "room": made["room"]}


def find_workspace(state: dict, room: str) -> dict | None:
    return next((w for w in workspaces(state) if w.get("room") == room), None)


def select_workspace(vm: TiloVm, room: str) -> None:
    state = vm.state()
    ws = find_workspace(state, room)
    expect(ws, f"{vm.vm} has no workspace {room} on its desk: "
               f"{[(w.get('room'), w.get('name')) for w in workspaces(state)]}")
    if (state.get("workspace_tree") or {}).get("active") != ws["id"]:
        vm.invoke("actions", f"workspace.select.{room}")
        expect(wait_until(lambda: (vm.state().get("workspace_tree") or {}).get("active") == ws["id"],
                          15, interval=0.5), f"{vm.vm} did not show workspace {room}")


def groups_in(state: dict, room: str) -> list[dict]:
    ws = find_workspace(state, room)
    return [g for s in (ws or {}).get("strips") or [] for g in s.get("window_groups") or []]


def open_window(vm: TiloVm, ws: dict) -> object:
    """A window on the workspace's desk: a new Terminal there. Its group id."""
    select_workspace(vm, ws["room"])
    before = {g["id"] for g in groups_in(vm.state(), ws["room"])}
    vm.invoke("actions", "window.new.terminal")
    new = wait_until(lambda: [g for g in groups_in(vm.state(), ws["room"]) if g["id"] not in before],
                     60, interval=0.5)
    expect(new, f"{vm.vm} opened no window in the workspace")
    return new[0]["id"]


def session_excerpt(vm: TiloVm) -> str:
    return vm.exec("sed 's/\\x1b\\[[0-9;]*m//g' \"$HOME/.local/state/tilo/session-logs/latest.log\" "
                   "2>/dev/null | grep -aiE 'join_flow|channel|workspace|refus|admit|enter|ERROR|desktop|"
                   "branch|certif' | grep -avE 'FRAME-LIFECYCLE|paint-stats' | tail -n 300 | cut -c1-600; true",
                   timeout=60, check=False)


def tabs_in(state: dict, room: str) -> dict:
    """The workspace's windows on this desk: group id to its tabs' titles."""
    return {g["id"]: [t.get("title") for t in g.get("tabs") or []] for g in groups_in(state, room)}


def edit_shows_on(run: Run, ws: dict, writer: str, reader: str, label: str, timeout: float = 300) -> dict:
    """A window `writer` opens on the workspace's desk shows on `reader`'s desk: a window
    group `reader` did not show before, the one the writer opened (by id, or by its tabs'
    titles where the desk names it differently there)."""
    w, r = run.vm(writer), run.vm(reader)
    select_workspace(r, ws["room"])
    before = tabs_in(r.state(), ws["room"])
    gid = open_window(w, ws)

    def arrived():
        try:
            mine = tabs_in(w.state(), ws["room"]).get(gid)
            theirs = tabs_in(r.state(), ws["room"])
        except VmError:
            return None
        new = {g: t for g, t in theirs.items() if g not in before}
        return new if gid in new or (mine and mine in new.values()) else None

    seen = wait_until(arrived, timeout, interval=2.0)
    if not seen:
        run.write(f"{label}-{writer}-state.json", json.dumps(w.state(), indent=1))
        run.write(f"{label}-{reader}-state.json", json.dumps(r.state(), indent=1))
        run.write(f"{label}-{writer}-session.log", session_excerpt(w))
        run.write(f"{label}-{reader}-session.log", session_excerpt(r))
        raise Failed(f"a window {writer} opened in the workspace (group {gid}, "
                     f"{tabs_in(w.state(), ws['room']).get(gid)}) did not show on {reader} within "
                     f"{timeout:.0f}s; {reader} shows {tabs_in(r.state(), ws['room'])}")
    return {"group": gid, "shown": {str(k): v for k, v in seen.items()}}


# -- branching ----------------------------------------------------------------------


def branch(facts: dict, run: Run) -> None:
    box = run.vm(SOURCE)
    out = box.vm_cmd("branch", SOURCE, *BRANCHES, "--stop", timeout=900)
    run.write("S04-branch.txt", out)
    for name in NAMES:
        plug_lan(run, name)
    listing = box.vm_cmd("list", timeout=120, check=False)
    run.write("S04-list.txt", listing)


def boot(facts: dict, run: Run, names: tuple[str, ...]) -> None:
    for name in names:
        run.vm(name).vm_cmd("start", name, timeout=900)
    for name in names:
        wait_session(run.vm(name))
    for name in names:
        facts[f"lan_{name}"] = address_lan(run, name).strip()[-300:]
    # The sessions start again on the network the boxes now share, as on a box that came up
    # on it.
    for name in names:
        run.vm(name).restart_session(300)
        wait_session(run.vm(name))


def judge_machine(facts: dict, run: Run, name: str, source: dict, others: dict) -> dict:
    m = machine_facts(run, name, f"S05-{name}")
    facts["machine"] = {k: m.get(k) for k in ("UUID", "RECORD", "KEY-DIGEST", "DEVICE-PUBLIC",
                                               "CERT", "PENDING-LABEL", "JOURNAL", "BRANCH-UNIT")}
    problems = []
    if m.get("RECORD") != m.get("UUID"):
        problems.append(f"machine.bin {m.get('RECORD')} is not this machine's UUID {m.get('UUID')}")
    if name != SOURCE:
        if m.get("UUID") == source.get("UUID"):
            problems.append("the branch reports the source's product UUID")
        if m.get("KEY-DIGEST") == source.get("KEY-DIGEST"):
            problems.append("the branch kept the source's device key")
        if m.get("MACHINE-ID") == source.get("MACHINE-ID"):
            problems.append("the branch kept the source's /etc/machine-id")
        if set(m["HOST-KEY"]) & set(source["HOST-KEY"]):
            problems.append("the branch kept SSH host keys of the source's")
        if m.get("JOURNAL"):
            problems.append("the re-key journal is still there (a store was not re-sealed)")
    else:
        for key in ("KEY-DIGEST", "MACHINE-ID", "UUID"):
            if m.get(key) != source.get(key):
                problems.append(f"box's {key} changed across the branch")
        if m["HOST-KEY"] != source["HOST-KEY"]:
            problems.append("box's SSH host keys changed across the branch")
    for other, theirs in others.items():
        if theirs.get("KEY-DIGEST") == m.get("KEY-DIGEST"):
            problems.append(f"{name} and {other} share a device key")
        if theirs.get("MACHINE-ID") == m.get("MACHINE-ID"):
            problems.append(f"{name} and {other} share a machine id")
    if source.get("CERT") and m.get("CERT") != source.get("CERT"):
        problems.append(f"{name} holds certificates of {m.get('CERT')}, the source of {source.get('CERT')}")
    expect(not problems, "; ".join(problems))
    return m


def judge_person(facts: dict, run: Run, name: str, source_person: dict, source_ident: dict,
                 devices: dict, key_digest: str | None, passwords: list[str] | None) -> dict:
    vm = run.vm(name)
    # The certificate a branch without its root in the system tree gets at the person's
    # next sign-in; this one holds it, so the boot issued it. Give the session a moment.
    p = wait_until(lambda: (q := person_facts(run, name, f"S05-{name}"))
                   if (q.get("identity") or {}).get("this_device") else None, 120, interval=5.0) \
        or person_facts(run, name, f"S05-{name}")
    ident = p.get("identity") or {}
    this = ident.get("this_device") or {}
    facts["identity"] = {"id": (ident.get("identity") or {}).get("id"), "this_device": this}
    facts["person"] = {k: p.get(k) for k in ("SECRET-DIGEST", "GIT-NAME", "SSH-KEY", "STORE")}
    problems = []
    if (ident.get("identity") or {}).get("id") != (source_ident.get("identity") or {}).get("id"):
        problems.append(f"the person is {ident.get('identity')}, not the source's "
                        f"{source_ident.get('identity')}")
    if not this:
        problems.append(f"{name} holds no valid certificate for the person: {ident.get('next')}")
    else:
        mine = this.get("device")
        if devices.get(name, {}).get("DEVICE-PUBLIC") and mine != devices[name]["DEVICE-PUBLIC"]:
            problems.append(f"the certificate names device {mine}, this device's key is "
                            f"{devices[name]['DEVICE-PUBLIC']}")
        if name != SOURCE and mine == (source_ident.get("this_device") or {}).get("device"):
            problems.append("the branch's certificate is the source's")
    for key in ("SECRET-DIGEST", "GIT-NAME", "SSH-KEY"):
        if p.get(key) != source_person.get(key):
            problems.append(f"{key}: {p.get(key)!r}, on the source {source_person.get(key)!r}")
    if key_digest:
        serve_model(vm, key_digest)
        try:
            facts["provider"] = provider_answers(vm)
        except (Failed, VmError) as exc:
            problems.append(f"the model provider's API key: {exc}")
    if passwords:
        try:
            open_passwords(vm)
            listed = wait_until(lambda: set(passwords) <= (held := password_items(vm)) and held,
                                60, interval=2.0)
            facts["passwords"] = "listed" if listed else sorted(password_items(vm))
            if not listed:
                problems.append(f"Passwords lists {sorted(password_items(vm))}, not the vault's "
                                f"{passwords}")
        except (Failed, VmError) as exc:
            problems.append(f"the passwords vault: {exc}")
    if not str(p.get("SSH-KEY-READABLE", "")).startswith("-----BEGIN OPENSSH PRIVATE KEY"):
        problems.append("the SSH private key is not readable as the person")
    expect(not problems, "; ".join(problems))
    return p


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--disk", type=Path, required=True)
    ap.add_argument("--commit")
    ap.add_argument("--tilo-vm", default="tilo-vm")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--taps", required=True, help="the tap devices of box, a and b on their network")
    ap.add_argument("--hold", type=int, default=0, help="seconds to keep the VMs up after the steps")
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    run = Run(args.tilo_vm, args.out, dict(zip(NAMES, args.taps.split(","))))
    meta = {"commit": args.commit, "started": dt.datetime.now(dt.UTC).isoformat(timespec="seconds"),
            "disk": str(args.disk)}
    try:
        steps(run, args)
    finally:
        status = finish(run, meta, args)
    return status


def steps(run: Run, args) -> None:
    run.facts = {"host_mem_available_mb": mem_available_mb()}
    # Three guests at once where the host has room for them; else b boots once box has
    # done its part, and is judged against the facts a and box left.
    together = mem_available_mb() >= 3 * GUEST_MB + HOST_MB
    run.step("S01", "box boots on this image and records its machine", boot_box, run, args.disk, GUEST_MB)
    if not run.passed("S01"):
        return
    run.step("S02", "a person signs in on box: the VM's person, certified by their root",
             sign_in, run)
    run.step("S03", "the person sets box up: a Secret Service secret, git, an SSH key", configure, run)
    run.step("S03b", "the person saves a model provider's API key through Settings", configure_provider, run)
    run.step("S03d", "the person saves a login in their passwords vault", configure_passwords, run)
    run.step("S03c", "the person makes a workspace on box and opens a window on its desk",
             configure_workspace, run)
    try:
        source = machine_facts(run, SOURCE, "S03-box-source")
        source_person = person_facts(run, SOURCE, "S03-box-source")
    except VmError as exc:
        run.step("S03d", "box's facts before the branch", lambda f: (_ for _ in ()).throw(exc))
        return
    source_ident = source_person.get("identity") or {}
    key_digest = next((s["facts"].get("api_key_digest") for s in run.steps
                       if s["id"] == "S03b" and s["status"] == "pass"), None)
    passwords = next((s["facts"].get("password_items") for s in run.steps
                      if s["id"] == "S03d" and s["status"] == "pass"), None)
    run.step("S04", "box is stopped and branched into a and b", branch, run)
    if not run.passed("S04"):
        return
    first = (SOURCE, *BRANCHES) if together else (SOURCE, "a")
    devices: dict[str, dict] = {}

    def judge(names: tuple[str, ...]) -> None:
        for name in names:
            others = {o: devices[o] for o in devices if o != name}
            m = run.step(f"S06-{name}", f"{name}: its own device key, machine record, machine id and "
                         "SSH host keys", judge_machine, run, name, source, others)
            devices[name] = m or machine_facts(run, name, f"S06-{name}")
        for name in names:
            run.step(f"S07-{name}", f"{name}: a valid certificate under the person's root, and the "
                     "person's secret, API key, SSH key and git config", judge_person, run, name,
                     source_person, source_ident, devices, key_digest, passwords)

    run.step("S05", f"{', '.join(first)} boot", boot, run, first)
    judge(first)
    ws = next((s["facts"].get("workspace") for s in run.steps if s["id"] == "S03c"), None)
    if ws:
        run.step("S08", "an edit on a shows on box", lambda f: f.update(
            edit_shows_on(run, ws, "a", SOURCE, "S08")))
        run.step("S09", "an edit on box shows on a", lambda f: f.update(
            edit_shows_on(run, ws, SOURCE, "a", "S09")))
        run.step("S10", "box and a reboot", reboot, run, (SOURCE, "a"))
        run.step("S11", "after the reboot, an edit on a shows on box", lambda f: f.update(
            edit_shows_on(run, ws, "a", SOURCE, "S11")))
        run.step("S12", "after the reboot, an edit on box shows on a", lambda f: f.update(
            edit_shows_on(run, ws, SOURCE, "a", "S12")))
    if not together:
        run.step("S05-b", "b boots, with box stopped to make room", boot_later, run)
        judge(("b",))
    run.step("S13", "vm rm box leaves a and b bootable", remove_source, run)


def boot_later(facts: dict, run: Run) -> None:
    run.vm(SOURCE).vm_cmd("stop", SOURCE, timeout=600)
    boot(facts, run, ("b",))


def reboot(facts: dict, run: Run, names: tuple[str, ...]) -> None:
    for name in names:
        run.vm(name).vm_cmd("restart", name, timeout=900)
    for name in names:
        wait_session(run.vm(name))
        facts[f"lan_{name}"] = address_lan(run, name).strip()[-300:]


def remove_source(facts: dict, run: Run) -> None:
    run.vm(SOURCE).vm_cmd("rm", SOURCE, "--force", timeout=600)
    for name in BRANCHES:
        run.vm(name).vm_cmd("restart", name, timeout=900)
        wait_session(run.vm(name))
        m = machine_facts(run, name, f"S13-{name}")
        expect(m.get("RECORD") == m.get("UUID"), f"{name} after box was removed: {m}")


def finish(run: Run, meta: dict, args) -> int:
    if args.hold:
        print(f"holding the VMs for {args.hold}s", flush=True)
        time.sleep(args.hold)
    for name in reversed(list(run.vms)):
        try:
            run.vm(name).vm_cmd("rm", name, "--force", timeout=600, check=False)
        except VmError:
            pass
    meta.update(run.facts)
    meta["finished"] = dt.datetime.now(dt.UTC).isoformat(timespec="seconds")
    failed = [s for s in run.steps if s["status"] != "pass"]
    meta["overall"] = "fail" if failed or not run.steps else "pass"
    (args.out / "report.json").write_text(json.dumps({"meta": meta, "steps": run.steps}, indent=1))
    lines = [f"# Branching a configured box: {meta['overall'].upper()}", "",
             f"commit {meta.get('commit')}, {len(run.steps) - len(failed)}/{len(run.steps)} steps pass", ""]
    for s in run.steps:
        lines.append(f"- {s['id']} {s['status'].upper()} ({s['seconds']}s) {s['title']}")
        if s["detail"]:
            lines.append(f"  - {s['detail'][:1500]}")
    (args.out / "report.md").write_text("\n".join(lines) + "\n")
    return 0 if meta["overall"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
