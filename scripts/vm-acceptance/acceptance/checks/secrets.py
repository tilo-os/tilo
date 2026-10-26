"""Secret custody: what the compositor keeps reaches tilo-secret-service, not a plain file.

The compositor loads the device key, which makes it non-dumpable: no other process,
the store included, can read its `/proc/<pid>/exe`. It proves on each bus connection
that it holds the device key instead (`so.tilo.Secrets1.CallerChallenge`/`ProveCaller`,
docs/identity-security.md, "Callers the store cannot examine"), and the store then
answers it as `binary:/usr/bin/shell`. `crates/system/secret-service/tests/identity_custody.rs`
proves that protocol with the production client code; these checks prove it with the
real compositor on the installed image.

C01 judges the run's slot: the compositor is non-dumpable, the store is open and lists
the compositor with the MLS key it keeps there, its sealed MLS state sits in an
owner-only directory, an app's portal secret (org.freedesktop.portal.Secret, asked the
way a host-launched app asks, from its own `app-tilo-<id>-<pid>.scope`) is answered from
the store, and the same secret is answered again after `tilo-secret-service` restarts
under the running compositor, whose connection must prove itself again.

C02 judges a person's own login: the dev account signs in at seat0's greeter (as R08
does), goes through first use on its control socket's `onboarding` actions, and its new
identity must name the Secret Service (`secret_ref.backend = secret-service-v1`, which
Settings > Users shows as "Secret Service + app-scoped signing broker") with no key file
beside it. A slot's throwaway identity keeps its private share in owner-only files by
design (tilo-vm guide driving), so only a person's login can show this. Then it signs out
and in again: the new compositor still finds what the store keeps for it, with no
custody warning, and the sealed MLS state is still there.

No check prints a secret: a portal secret is compared with itself inside the guest, and
the evidence names presence, sizes, modes and owners only.
"""

from __future__ import annotations

import json
import re
import shlex
import time

from ..core import CheckFailed, Unobservable, check, expect
from ..vm import wait_until
from .resources import SIGN_OUT, dev_account, sign_in

AREA = "secrets"

COMPOSITOR_APP = "binary:/usr/bin/shell"
PORTAL_APP = "so.tilo.notes"
MLS_UNAVAILABLE = "MLS collaboration unavailable"
# The compositor's warnings when it could not use what the store keeps for it.
CUSTODY_WARNINGS = (MLS_UNAVAILABLE, "channel encryption unavailable", "MLS key")
SESSION_LOG = ".local/state/tilo/session-logs/latest.log"
SECRET_LEN = 32

# As root: the uid's compositor, and what a process of that user sees of its executable.
# The exit status of `ls` is what matters: 0 would mean the kernel shows the link.
DUMPABLE = r"""
pid=$(pgrep -u __UID__ -x shell | head -1)
[ -n "$pid" ] || { echo "NO-COMPOSITOR"; exit 0; }
echo "PID=$pid"
out=$(runuser -u "$(id -nu __UID__)" -- ls -l "/proc/$pid/exe" 2>&1); echo "LS-EXIT=$?"
printf '%s\n' "$out" | sed 's/^/LS: /'
state=$(tr '\0' '\n' < "/proc/$pid/environ" | sed -n 's/^XDG_STATE_HOME=//p')
echo "STATE=${state}"
"""

# As root, for the uid: the store's answer to `status` and `apps list --json`, asked
# over the user's own session bus as the user.
STORE = r"""
u=$(id -nu __UID__)
bus="unix:path=/run/user/__UID__/bus"
as_user() { runuser -u "$u" -- env XDG_RUNTIME_DIR=/run/user/__UID__ DBUS_SESSION_BUS_ADDRESS="$bus" "$@"; }
echo "STATUS-BEGIN"; as_user tilo-secret-service status 2>&1; echo "STATUS-END"
echo "APPS-BEGIN"; as_user tilo-secret-service apps list --json 2>&1; echo "APPS-END"
echo "UNIT=$(as_user systemctl --user show tilo-secret-service.service -p ActiveState -p MainPID -p NRestarts --value | paste -sd, -)"
"""

# As root: the MLS state directory's entries (mode, owner, size, path; digests of the
# sealed files, which are ciphertext), and how often the session log names a custody
# warning.
MLS_STATE = r"""
d=__STATE__/tilo/collaboration/mls
if [ -d "$d" ]; then
  echo "DIR $(stat -c '%a %U' "$d")"
  find "$d" -mindepth 1 -printf 'ENTRY %y %m %u %s %P\n' | sort -k6
  find "$d" -type f -print0 | sort -z | xargs -0r sha256sum | sed "s|$d/||; s/^/DIGEST /"
else
  echo "NO-DIR $d"
fi
"""
LOG_WARNINGS = r"""
log=$(getent passwd "$(id -nu __UID__)" | cut -d: -f6)/__LOG__
[ -f "$log" ] || { echo "NO-LOG $log"; exit 0; }
__GREPS__
"""

# As the slot's user, in its session's view: ask the portal for the app's secret from a
# scope named the way the launcher names the app's own (`app-tilo-<id>-<pid>.scope`,
# crates/packaging/launcher/src/scope.rs), so xdg-desktop-portal derives the app's id
# and the compositor resolves it to the running app. The secret arrives on the
# descriptor; only its length and whether it equals the earlier answer are printed.
PORTAL = r"""
keep="$XDG_RUNTIME_DIR/acceptance-portal-secret"
f=$(mktemp "$XDG_RUNTIME_DIR/acceptance-portal.XXXXXX")
chmod 600 "$f"
systemd-run --user --scope --quiet --unit="app-tilo-__APP__-$$" -- \
  busctl --user call org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop \
  org.freedesktop.portal.Secret RetrieveSecret 'ha{sv}' 3 0 3>"$f" > "$f.reply" 2>&1
echo "CALL-EXIT=$?"
sed 's/^/REPLY: /' "$f.reply"
i=0; while [ $i -lt 50 ] && [ "$(stat -c %s "$f")" -lt __LEN__ ]; do i=$((i + 1)); sleep 0.2; done
echo "LENGTH=$(stat -c %s "$f")"
if [ -f "$keep" ]; then
  cmp -s "$f" "$keep" && echo "SAME=yes" || echo "SAME=no"
else
  cp "$f" "$keep" && chmod 600 "$keep" && echo "SAME=first"
fi
rm -f "$f" "$f.reply"
"""
PORTAL_FORGET = 'rm -f "$XDG_RUNTIME_DIR/acceptance-portal-secret"'
FIELD = re.compile(r"^([A-Z-]+)=(.*)$", re.M)


def fields(text: str) -> dict:
    return {k: v.strip() for k, v in FIELD.findall(text)}


def between(text: str, start: str, end: str) -> str:
    return text.split(start, 1)[-1].split(end, 1)[0].strip() if start in text else ""


def compositor(ctx, uid: int, label: str) -> dict:
    """The uid's compositor: its pid, its state root, and whether its executable is hidden
    from a process of the same user (the kernel refuses a non-dumpable process's link)."""
    text = ctx.vm.root_exec(DUMPABLE.replace("__UID__", str(uid)), timeout=60)
    ctx.write(f"{label}-compositor.txt", text)
    if "NO-COMPOSITOR" in text:
        raise Unobservable(f"no compositor runs for uid {uid}")
    f = fields(text)
    ls = "\n".join(l[4:] for l in text.splitlines() if l.startswith("LS: "))
    return {"pid": int(f["PID"]), "state_home": f.get("STATE") or "",
            "exe_hidden": f.get("LS-EXIT") != "0" and "Permission denied" in ls, "ls": ls}


def store(ctx, uid: int, label: str) -> dict:
    """`tilo-secret-service status` and `apps list --json` for the uid, and its unit."""
    text = ctx.vm.root_exec(STORE.replace("__UID__", str(uid)), timeout=60)
    ctx.write(f"{label}-store.txt", text)
    status = between(text, "STATUS-BEGIN", "STATUS-END")
    raw = between(text, "APPS-BEGIN", "APPS-END")
    try:
        apps = json.loads(raw)
    except ValueError as exc:
        raise Unobservable(f"`tilo-secret-service apps list --json` did not answer JSON: {raw[-300:]!r}") from exc
    unit = fields(text).get("UNIT", "")
    return {"status": status, "apps": {a["id"]: a for a in apps}, "unit": unit}


def mls_state(ctx, state_home: str, label: str) -> dict:
    if not state_home.startswith("/"):
        raise Unobservable(f"the compositor names no XDG_STATE_HOME ({state_home!r})")
    text = ctx.vm.root_exec(MLS_STATE.replace("__STATE__", shlex.quote(state_home)), timeout=60)
    ctx.write(f"{label}-mls-state.txt", text)
    if text.startswith("NO-DIR") or "\nNO-DIR" in text:
        return {"dir": None, "entries": [], "digests": {}}
    lines = text.splitlines()
    directory = next(l.split()[1:] for l in lines if l.startswith("DIR "))
    entries = [l.split(None, 5)[1:] for l in lines if l.startswith("ENTRY ")]
    digests = {l.split(None, 2)[2]: l.split()[1] for l in lines if l.startswith("DIGEST ")}
    return {"dir": directory, "entries": entries, "digests": digests}


def log_warnings(ctx, uid: int) -> dict:
    greps = "\n".join(f"echo {shlex.quote(w)}=$(grep -cF {shlex.quote(w)} \"$log\")" for w in CUSTODY_WARNINGS)
    text = ctx.vm.root_exec(LOG_WARNINGS.replace("__UID__", str(uid)).replace("__LOG__", SESSION_LOG)
                            .replace("__GREPS__", greps), timeout=60)
    if "NO-LOG" in text:
        raise Unobservable(f"the session wrote no log: {text.strip()}")
    counts = {}
    for line in text.splitlines():
        name, _, count = line.rpartition("=")
        if name in CUSTODY_WARNINGS:
            counts[name] = int(count or 0)
    return counts


def portal_secret(ctx, label: str) -> dict:
    text = ctx.vm.exec(PORTAL.replace("__APP__", PORTAL_APP).replace("__LEN__", str(SECRET_LEN)),
                       timeout=90, check=False)
    ctx.write(f"{label}.txt", text)
    f = fields(text)
    return {"exit": f.get("CALL-EXIT"), "length": int(f.get("LENGTH") or 0), "same": f.get("SAME"),
            "reply": [l[7:] for l in text.splitlines() if l.startswith("REPLY: ")]}


def judge_compositor_row(apps: dict, what: str, min_items: int = 1) -> dict:
    row = apps.get(COMPOSITOR_APP)
    expect(row is not None, f"{what}: the store does not list the compositor ({COMPOSITOR_APP}); "
                            f"it lists {sorted(apps)}")
    expect(row.get("access") == "allowed",
           f"{what}: the store does not let the compositor in: {row}")
    expect(int(row.get("items") or 0) >= min_items,
           f"{what}: the compositor keeps {row.get('items')} items in the store, expected at least {min_items}")
    return row


@check("C01", AREA, "The compositor keeps its MLS key and apps' portal secrets in tilo-secret-service, "
                    "proves itself while non-dumpable, and outlives a store restart")
def compositor_secrets_in_the_store(ctx, res):
    uid = ctx.vm.slot_uid()
    comp = compositor(ctx, uid, "C01")
    res.metrics["compositor_exe_hidden"] = comp["exe_hidden"]
    expect(comp["exe_hidden"], f"the compositor's /proc/<pid>/exe is readable by its own user, so it is "
                               f"dumpable: {comp['ls']!r}")

    before = store(ctx, uid, "C01-before")
    res.metrics["store_status"] = before["status"]
    expect(before["status"].splitlines()[:1] == ["open"], f"the store is not open: {before['status']!r}")
    row = judge_compositor_row(before["apps"], "after sign-in")
    res.metrics["compositor_row"] = {k: row.get(k) for k in ("id", "access", "items", "uses")}

    warnings = log_warnings(ctx, uid)
    res.metrics["log_warnings"] = warnings
    expect(not any(warnings.values()), f"the session log names a custody failure: {warnings}")
    mls = mls_state(ctx, comp["state_home"], "C01-before")
    res.metrics["mls_dir"] = mls["dir"]
    expect(mls["dir"] is not None, "the compositor made no MLS state directory")
    expect(mls["dir"][0] == "700", f"the MLS state directory is mode {mls['dir'][0]}, not 700")
    loose = [e for e in mls["entries"] if e[0] == "f" and e[1] not in ("600", "400")]
    expect(not loose, f"MLS state files readable beyond their owner: {loose}")

    # An app's portal secret: answered from the store, and again after it restarts.
    ctx.vm.exec(PORTAL_FORGET, check=False)
    ctx.open_app(PORTAL_APP)
    first = portal_secret(ctx, "C01-portal-first")
    res.metrics["portal_first"] = {k: first[k] for k in ("exit", "length", "same")}
    expect(first["exit"] == "0" and first["length"] == SECRET_LEN,
           f"the portal answered no {SECRET_LEN}-byte secret: {first}")
    after_portal = store(ctx, uid, "C01-after-portal")
    expect(f"sandbox:{PORTAL_APP}" in after_portal["apps"],
           f"the store has no record of {PORTAL_APP} after its portal secret: {sorted(after_portal['apps'])}")

    ctx.vm.exec("systemctl --user restart tilo-secret-service.service", timeout=60)
    restarted = wait_until(lambda: (s := store(ctx, uid, "C01-store-restarted"))["status"].startswith("open") and s,
                           30, interval=1.0)
    if not restarted:
        raise CheckFailed("tilo-secret-service did not come back open after a restart")
    res.metrics["store_unit_before_after"] = [before["unit"], restarted["unit"]]
    second = portal_secret(ctx, "C01-portal-after-store-restart")
    res.metrics["portal_after_store_restart"] = {k: second[k] for k in ("exit", "length", "same")}
    expect(second["exit"] == "0" and second["length"] == SECRET_LEN,
           f"after the store restarted, the compositor's next secret call failed: {second}")
    expect(second["same"] == "yes", "after the store restarted, the portal answered a different secret")

    ctx.vm.exec(PORTAL_FORGET, check=False)


# -- a person's own login --------------------------------------------------------

# As root: the account's control socket, run as the account (a dev image's login gives
# a person's own session the socket and keeps first use, os/usr/libexec/tilo-session).
CONTROL = r"""
u=__USER__; uid=$(id -u "$u")
runuser -u "$u" -- env XDG_RUNTIME_DIR=/run/user/$uid \
  tilo-control --socket /run/user/$uid/tilo-session/test-control.sock __ARGS__ 2>&1
"""
# As root: the account's active identity record, by what it names: its secret's backend
# and whether a key file sits beside it. The record holds no secret either way.
IDENTITY = r"""
u=__USER__; home=$(getent passwd "$u" | cut -d: -f6); d="$home/.config/tilo/identity"
[ -d "$d" ] || { echo "NO-IDENTITY-DIR"; exit 0; }
echo "DIR $(stat -c '%a %U' "$d")"
id=primary; [ -f "$d/active" ] && id=$(head -c 128 "$d/active" | tr -dc 'A-Za-z0-9_.-')
rec="$d/$id.json"
[ -f "$rec" ] || { echo "NO-RECORD $id"; ls -la "$d" | sed 's/^/LS /'; exit 0; }
echo "RECORD $id.json $(stat -c '%a %U %s' "$rec")"
python3 -I -c 'import json, sys
r = json.load(open(sys.argv[1]))
ref = r.get("secret_ref") or {}
print("BACKEND=" + str(ref.get("backend")))
print("NAME-THROWAWAY=" + str("hrowaway" in str(r.get("display_name", ""))).lower())' "$rec"
find "$d" -name '*.key' -printf 'KEYFILE %m %u %s %P\n'
[ -e "$d/secrets" ] && find "$d/secrets" -printf 'SECRETS %y %m %u %s %P\n'
echo "END"
"""
# First use, through the onboarding actions a person's clicks reach: acknowledge once,
# then go forward; a backup is declined with its own acknowledgement, the tour skipped.
ACKNOWLEDGE = {"onboarding.understood": {"checked": True},
               "onboarding.backup.skip-acknowledged": {"acknowledge_without_backup": True}}
FORWARD = ["onboarding.finish", "onboarding.tour.skip", "onboarding.codecs.skip",
           "onboarding.codecs.continue", "onboarding.backup.continue", "onboarding.create",
           "onboarding.begin", "migration.later"]
FIRST_USE_STEPS = 24


def control(ctx, user: str, *args: str, timeout: float = 60) -> str:
    return ctx.vm.root_exec(CONTROL.replace("__USER__", shlex.quote(user))
                            .replace("__ARGS__", shlex.join(args)), timeout=timeout)


def onboarding_rows(ctx, user: str) -> list[dict] | None:
    """The onboarding provider's rows, or None while the socket does not answer."""
    text = control(ctx, user, "actions", "list", "--json", "--limit", "200")
    start = text.find("{")
    try:
        listing = json.JSONDecoder().raw_decode(text[start:])[0] if start >= 0 else None
    except ValueError:
        listing = None
    if not isinstance(listing, dict):
        return None
    return [a for a in listing.get("actions", []) if a.get("provider") == "onboarding"]


def next_first_use_step(offered: set[str], acknowledged: set[str]) -> str | None:
    """The onboarding action to invoke next: an acknowledgement not yet given, else the
    furthest step forward on offer; None when nothing offered is one of them."""
    return next((i for i in ACKNOWLEDGE if i in offered and i not in acknowledged), None) \
        or next((i for i in FORWARD if i in offered), None)


def go_through_first_use(ctx, user: str) -> list[str]:
    """Invoke onboarding actions until none is offered; return the ids invoked."""
    done: list[str] = []
    acknowledged: set[str] = set()
    for _ in range(FIRST_USE_STEPS):
        # A listing, even an empty one, once the socket answers.
        listed = wait_until(lambda: (rows,) if (rows := onboarding_rows(ctx, user)) is not None else None,
                            30, interval=1.0)
        if listed is None:
            raise Unobservable(f"{user}'s control socket does not list actions")
        offered = {r["id"] for r in listed[0]}
        if not offered:
            return done
        step = next_first_use_step(offered, acknowledged)
        if step is None:
            raise CheckFailed(f"first use offers nothing this check knows how to go forward with: "
                              f"{sorted(offered)} (after {done})")
        args = ACKNOWLEDGE.get(step)
        out = control(ctx, user, "actions", "invoke", "onboarding", step,
                      *(["--args-json", json.dumps(args)] if args else []), timeout=120)
        ctx.write(f"C02-first-use-{len(done):02}-{step}.txt", out)
        if step in ACKNOWLEDGE:
            acknowledged.add(step)
        done.append(step)
        time.sleep(1.5)
    raise CheckFailed(f"first use did not end within {FIRST_USE_STEPS} steps: {done}")


def identity_record(ctx, user: str) -> dict:
    text = ctx.vm.root_exec(IDENTITY.replace("__USER__", shlex.quote(user)), timeout=60)
    ctx.write("C02-identity.txt", text)
    if "NO-IDENTITY-DIR" in text or "NO-RECORD" in text:
        return {}
    f = fields(text)
    return {"backend": f.get("BACKEND"), "throwaway": f.get("NAME-THROWAWAY") == "true",
            "key_files": [l for l in text.splitlines() if l.startswith("KEYFILE ")],
            "secrets_dir": [l for l in text.splitlines() if l.startswith("SECRETS ")]}


@check("C02", AREA, "A person's first sign-in keeps their identity in tilo-secret-service, with no key file")
def person_identity_in_the_store(ctx, res):
    user, secret = dev_account()
    uid = int(ctx.vm.root_exec(f"id -u {shlex.quote(user)}").split()[0])
    if "PID=" not in ctx.vm.root_exec(f"pgrep -u {uid} -x shell | sed 's/^/PID=/' | head -1"):
        steps = sign_in(ctx, user, secret, "C02")
        res.metrics["sign_in"] = steps
        if "control_s" not in steps:
            raise Unobservable(f"{user} signed in at the greeter, but its control socket never answered: {steps}")
    try:
        if not identity_record(ctx, user):
            res.metrics["first_use"] = go_through_first_use(ctx, user)
        record = wait_until(lambda: identity_record(ctx, user), 60, interval=2.0)
        if not record:
            raise CheckFailed(f"first use ended without an identity record for {user}")
        res.metrics["identity"] = record
        expect(not record["throwaway"], "the account's identity is a throwaway test identity")
        expect(record["backend"] == "secret-service-v1",
               f"the identity names secret backend {record['backend']!r}, not secret-service-v1")
        expect(not record["key_files"] and not [l for l in record["secrets_dir"] if " f " in f" {l} "],
               f"a key file sits beside the identity: {record['key_files'] + record['secrets_dir']}")

        comp = compositor(ctx, uid, "C02")
        res.metrics["compositor_exe_hidden"] = comp["exe_hidden"]
        expect(comp["exe_hidden"], f"{user}'s compositor is dumpable: {comp['ls']!r}")
        held = store(ctx, uid, "C02")
        expect(held["status"].splitlines()[:1] == ["open"], f"{user}'s store is not open: {held['status']!r}")
        row = judge_compositor_row(held["apps"], f"{user}'s login")
        res.metrics["compositor_row"] = {k: row.get(k) for k in ("id", "access", "items", "uses")}
        warnings = log_warnings(ctx, uid)
        res.metrics["log_warnings"] = warnings
        expect(not any(warnings.values()), f"{user}'s session log names a custody failure: {warnings}")
        mls = mls_state(ctx, comp["state_home"], "C02-before")
        expect(mls["dir"] is not None and mls["dir"][0] == "700",
               f"{user}'s MLS state directory is missing or not owner-only: {mls['dir']}")

        # Signed out and in again: the new compositor reads what the store keeps for it
        # back instead of making it anew, and the sealed state it left is still there.
        ctx.write("C02-sign-out-1.txt",
                  ctx.vm.root_exec(SIGN_OUT.replace("__USER__", shlex.quote(user)), timeout=120))
        steps = sign_in(ctx, user, secret, "C02-again")
        res.metrics["sign_in_again"] = steps
        if "control_s" not in steps:
            raise Unobservable(f"{user}'s second sign-in never reached an answering desktop: {steps}")
        again = compositor(ctx, uid, "C02-again")
        expect(again["pid"] != comp["pid"], "signing in again left the same compositor running")
        expect(again["exe_hidden"], f"{user}'s new compositor is dumpable")
        later = store(ctx, uid, "C02-again")
        judge_compositor_row(later["apps"], f"{user}'s second login", int(row.get("items") or 1))
        later_warnings = log_warnings(ctx, uid)
        res.metrics["log_warnings_again"] = later_warnings
        expect(not any(later_warnings.values()),
               f"after signing in again, {user}'s session log names a custody failure: {later_warnings}")
        mls_again = mls_state(ctx, again["state_home"], "C02-again")
        gone = sorted(set(mls["digests"]) - set(mls_again["digests"]))
        res.metrics["mls_files"] = [len(mls["digests"]), len(mls_again["digests"])]
        expect(not gone, f"MLS state files vanished across a compositor restart: {gone}")
        expect(identity_record(ctx, user) == record, "the identity record changed across a sign-in")
    finally:
        print("HOLD: exploring", flush=True)
        time.sleep(90 * 60)
        ctx.write("C02-sign-out.txt",
                  ctx.vm.root_exec(SIGN_OUT.replace("__USER__", shlex.quote(user)), timeout=120))
