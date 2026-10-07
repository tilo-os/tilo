"""Tracked semantic jobs through the existing tilo-vm control transport."""
from .vm import VmError, wait_until
import json

def require(ok, message):
    if not ok:
        raise VmError(message)


def succeeded(reply):
    update = reply.get("receipt", {}).get("update", {})
    require(update.get("state") == "succeeded", "action did not succeed")
    return update.get("output", {}).get("value")


class PendingJob:
    """One admitted invocation; permits a portal conversation before terminal polling."""
    def __init__(self, vm, receipt, out, name):
        self.vm, self.receipt, self.out, self.name = vm, receipt, out, name
        self.job = receipt.get("receipt", {}).get("broker_invocation_id")
        self.invocation = receipt.get("receipt", {}).get("update", {}).get("invocation")
        require(isinstance(self.job, int) and not isinstance(self.job, bool), "missing broker job identity")
        require(bool(self.invocation), "missing invocation identity")

    def poll(self):
        reply = self.vm.control_json("actions", "status", str(self.job))
        update = reply.get("receipt", {}).get("update", {})
        require(reply.get("receipt", {}).get("broker_invocation_id") == self.job
                and update.get("invocation") == self.invocation, "action status changed invocation identity")
        require(update.get("state") in ("accepted", "running", "succeeded", "failed", "cancelled"),
                "unobservable action status")
        return reply

    def pending(self):
        require(self.poll()["receipt"]["update"]["state"] in ("accepted", "running"),
                "original action ended before chooser completion")

    def finish(self, timeout=45):
        def probe():
            reply = self.poll()
            return reply if reply["receipt"]["update"]["state"] in ("succeeded", "failed", "cancelled") else None
        final = wait_until(probe, timeout, interval=0.5)
        require(final is not None, "action did not reach authoritative terminal status")
        (self.out / f"{self.name}-status.json").write_text(json.dumps(final, indent=2))
        return succeeded(final)


def begin(vm, row, args, out, name):
    receipt = vm.invoke(row["provider"], row["id"], args)
    (out / f"{name}-invoke.json").write_text(json.dumps(receipt, indent=2))
    return PendingJob(vm, receipt, out, name)


def invoke(vm, row, args, out, name, timeout=45, require_job=True):
    if require_job:
        return begin(vm, row, args, out, name).finish(timeout)
    receipt = vm.invoke(row["provider"], row["id"], args)
    (out / f"{name}-invoke.json").write_text(json.dumps(receipt, indent=2))
    if receipt.get("receipt", {}).get("broker_invocation_id") is None:
        return succeeded(receipt)
    return PendingJob(vm, receipt, out, name).finish(timeout)
