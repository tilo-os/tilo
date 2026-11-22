"""Tracked semantic jobs through the existing tilo-vm control transport."""
from .vm import Invoked, VmError, completed, ended, wait_until
import json

def require(ok, message):
    if not ok:
        raise VmError(message)


def succeeded(status):
    """The value an invocation that completed answered with; any other status raises."""
    return completed(status).get("value")


class PendingJob:
    """One admitted invocation; permits a portal conversation before terminal polling."""
    def __init__(self, vm, invoked: Invoked, out, name):
        self.vm, self.invocation, self.out, self.name = vm, invoked.invocation, out, name
        require(bool(self.invocation), "missing invocation identity")

    def pending(self):
        require(not ended(self.vm.observe(self.invocation)),
                "original action ended before chooser completion")

    def finish(self, timeout=45):
        def probe():
            status = self.vm.observe(self.invocation)
            return status if ended(status) else None
        final = wait_until(probe, timeout, interval=0.5)
        require(final is not None, "action did not reach authoritative terminal status")
        (self.out / f"{self.name}-status.json").write_text(json.dumps(final, indent=2))
        return succeeded(final)


def begin(vm, row, args, out, name):
    invoked = vm.invoke_row(row, args)
    (out / f"{name}-invoke.json").write_text(json.dumps(invoked._asdict(), indent=2))
    return PendingJob(vm, invoked, out, name)


def invoke(vm, row, args, out, name, timeout=45, require_job=True):
    if require_job:
        return begin(vm, row, args, out, name).finish(timeout)
    invoked = vm.invoke_row(row, args)
    (out / f"{name}-invoke.json").write_text(json.dumps(invoked._asdict(), indent=2))
    if ended(invoked.status):
        return succeeded(invoked.status)
    return PendingJob(vm, invoked, out, name).finish(timeout)
