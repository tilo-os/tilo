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


def invoke(vm, row, args, out, name, timeout=45, require_job=True):
    receipt = vm.invoke(row["provider"], row["id"], args)
    (out / f"{name}-invoke.json").write_text(json.dumps(receipt, indent=2))
    job = receipt.get("receipt", {}).get("broker_invocation_id")
    if not require_job and job is None:
        return succeeded(receipt)
    require(isinstance(job, int) and not isinstance(job, bool), "missing broker job identity")
    expected_invocation = receipt.get("receipt", {}).get("update", {}).get("invocation")
    require(bool(expected_invocation), "missing invocation identity")

    def probe():
        reply = vm.control_json("actions", "status", str(job))
        require(reply.get("receipt", {}).get("broker_invocation_id") == job
                and reply.get("receipt", {}).get("update", {}).get("invocation") == expected_invocation,
                "action status changed invocation identity")
        state = reply.get("receipt", {}).get("update", {}).get("state")
        if state in ("succeeded", "failed", "cancelled"):
            return reply
        require(state in ("accepted", "running"), "unobservable action status")
        return None

    final = wait_until(probe, timeout, interval=0.5)
    require(final is not None, "action did not reach authoritative terminal status")
    require(final.get("receipt", {}).get("broker_invocation_id") == job
            and final["receipt"]["update"].get("invocation") == receipt["receipt"]["update"].get("invocation"),
            "action status changed invocation identity")
    (out / f"{name}-status.json").write_text(json.dumps(final, indent=2))
    return succeeded(final)


