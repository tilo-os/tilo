"""Signed layers: the dev image runs the production merge with a development key.

The `vm` slot's layers reach the VM's /usr only through tilo-extension-activate,
the production verifier, which checks each against the extension authorities the
owner enrolled (tilo-vm enrolls its host's key in a VM it runs at the first swap
into the `vm` slot). These checks need a layer in the `vm` slot (`--swap vmd`)
and an image built from the commit under test (t's dev-trust-acceptance check).
Each one puts back what it changed, so the next starts from a merged layer.
They change the VM (a reboot among them), so a run includes them only when asked
for (`--only trust`; run.py's ON_REQUEST).
"""

from __future__ import annotations

import json
import shlex

from ..core import Unobservable, check, expect
from ..vm import wait_until

AREA = "trust"

STATUS = "/run/tilo/signed-layers.json"
DESIRED = "/var/lib/tilo/signed-layers"
AUTHORITIES = "/etc/tilo/extension-authorities.d"
# An Ed25519 key no build host holds: enrolled in place of the real one, it must
# not make any layer acceptable.
STRANGER = ("-----BEGIN PUBLIC KEY-----\n"
            "MCowBQYDK2VwAyEAiojj3XQJ8ZX9UtstPLpdcspnCb8dlBIb83SIAbQPb1w=\n"
            "-----END PUBLIC KEY-----\n")


def _raw(ctx, script: str, timeout: float = 300) -> str:
    """Root in the throwaway VM, past vm ssh's guard: these checks change what it
    guards (the layer store, the desired set, the enrolled authorities)."""
    return ctx.vm.vm_cmd("ssh", "--raw", "--", "sh -c " + shlex.quote(script), timeout=timeout)


def _status(ctx) -> dict:
    text = _raw(ctx, f"cat {STATUS} 2>/dev/null || echo '{{}}'")
    try:
        return json.loads(text)
    except ValueError as exc:
        raise Unobservable(f"{STATUS} is not JSON: {text[:200]!r}") from exc


def _activate(ctx) -> dict:
    """Run the activation as the dev image runs it on every change; its record."""
    _raw(ctx, "systemctl start tilo-extension-activate.service || true")
    return _status(ctx)


def _merged(status: dict) -> dict[str, dict]:
    return {layer["name"]: layer for layer in status.get("merged", [])}


def _refusal(status: dict, name: str) -> str:
    for layer in status.get("refused", []):
        if layer.get("name") == name:
            return layer.get("reason", "")
    return ""


def _layer(ctx) -> dict:
    """A merged signed layer of the vm slot, with its tree's path."""
    merged = _merged(_activate(ctx))
    if not merged:
        raise Unobservable("no signed layer is merged: run with --swap vmd (a layer in the vm slot)")
    name, record = sorted(merged.items())[0]
    payload = _raw(ctx, f"readlink -f {DESIRED}/{name}/payload").strip()
    expect(payload, f"the desired entry of {name} has no payload")
    return {"name": name, "payload": payload, **record}


@check("T01", AREA, "Only the activation merges system layers, with an enrolled dev key")
def merger(ctx, res):
    sysext = _raw(ctx, "systemctl is-enabled systemd-sysext.service || true").strip()
    res.metrics["systemd_sysext"] = sysext
    expect(sysext == "masked", f"systemd-sysext.service is {sysext!r}, not masked as on a production image")
    activation = _raw(ctx, "systemctl is-enabled tilo-extension-activate.service || true").strip()
    expect(activation == "enabled", f"tilo-extension-activate.service is {activation!r}")
    legacy = _raw(ctx, "ls -A /var/lib/extensions 2>/dev/null || true").split()
    expect(not legacy, f"/var/lib/extensions holds {legacy}: something merges behind the activation")
    layer = _layer(ctx)
    res.metrics.update(layer=layer["name"], authority=layer.get("authority"),
                       fingerprint=layer.get("fingerprint"))
    expect(layer.get("authority") == "dev", f"layer {layer['name']} is signed by {layer.get('authority')!r}")
    keys = _raw(ctx, f"ls {AUTHORITIES}").split()
    expect("dev.pub" in keys, f"{AUTHORITIES} holds {keys}, not the enrolled dev key")
    merged = _raw(ctx, "systemd-sysext status --json=short || true")
    expect(layer["name"] in merged, f"systemd-sysext does not list {layer['name']} as merged")
    ctx.facts["signed_layer"] = True


@check("T02", AREA, "A tampered layer is refused, and merges again once it is whole", needs=("signed_layer",))
def tampered(ctx, res):
    layer = _layer(ctx)
    victim = _raw(ctx, f"find {shlex.quote(layer['payload'])}/usr -type f | sort | head -1").strip()
    expect(victim, f"layer {layer['name']} has no file to change")
    keep = "/var/tmp/trust-acceptance-original"
    q = shlex.quote(victim)
    # A sealed file cannot be written; replacing it is what tampering looks like.
    _raw(ctx, f"cp -a {q} {keep} && rm -f {q} && cp -a {keep} {q} && printf x >> {q}")
    try:
        status = _activate(ctx)
        reason = _refusal(status, layer["name"])
        res.metrics["refusal"] = reason
        expect(reason, f"layer {layer['name']} with a changed {victim} was not refused: {status}")
        expect(layer["name"] not in _merged(status), "the changed layer is still merged")
        failed = _raw(ctx, "systemctl is-failed tilo-extension-activate.service || true").strip()
        expect(failed == "failed", f"the refusal left the activation unit {failed!r}, not failed")
    finally:
        _raw(ctx, f"rm -f {q} && cp -a {keep} {q} && rm -f {keep}")
    expect(layer["name"] in _merged(_activate(ctx)), "the restored layer did not merge again")


@check("T03", AREA, "An unsigned layer is refused", needs=("signed_layer",))
def unsigned(ctx, res):
    layer = _layer(ctx)
    sig = shlex.quote(f"{DESIRED}/{layer['name']}/manifest.json.sig")
    _raw(ctx, f"mv {sig} {sig}.aside")
    try:
        status = _activate(ctx)
        reason = _refusal(status, layer["name"])
        res.metrics["refusal"] = reason
        expect("unsigned" in reason, f"the unsigned layer was not refused as unsigned: {status}")
    finally:
        _raw(ctx, f"mv {sig}.aside {sig}")
    expect(layer["name"] in _merged(_activate(ctx)), "the signed layer did not merge again")


@check("T04", AREA, "A layer signed by a key the VM has not enrolled is refused", needs=("signed_layer",))
def unenrolled(ctx, res):
    layer = _layer(ctx)
    dev = f"{AUTHORITIES}/dev.pub"
    _raw(ctx, f"mv {dev} /var/tmp/trust-acceptance-dev.pub && "
              f"printf %s {shlex.quote(STRANGER)} > {dev} && chmod 0644 {dev}")
    try:
        status = _activate(ctx)
        reason = _refusal(status, layer["name"])
        res.metrics["refusal"] = reason
        expect("not signed by an enrolled" in reason,
               f"a layer signed by an unenrolled key was not refused: {status}")
    finally:
        _raw(ctx, f"mv /var/tmp/trust-acceptance-dev.pub {dev}")
    expect(layer["name"] in _merged(_activate(ctx)), "the layer did not merge again with its key enrolled")


@check("T05", AREA, "A signed layer stays merged across a reboot", needs=("signed_layer",))
def reboot(ctx, res):
    layer = _layer(ctx)
    ctx.vm.vm_cmd("restart", ctx.vm.vm, timeout=900)

    def merged_again():
        try:
            return layer["name"] in _merged(_status(ctx))
        except Exception:  # noqa: BLE001 - the VM is still coming up
            return False

    expect(wait_until(merged_again, 300, interval=5.0),
           f"layer {layer['name']} was not merged by the activation after the reboot")
    merged = _raw(ctx, "systemd-sysext status --json=short || true")
    expect(layer["name"] in merged, "systemd-sysext does not list the layer after the reboot")
