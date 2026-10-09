"""Bridge a published fixture's management daemon through its matching client."""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

from acceptance.vm import TiloVm, VmError

PROTOCOL_PATH = "crates/dev/tilo-vm-protocol/src/lib.rs"


def protocol_at(commit: str, repo: Path | None = None) -> int:
    source = subprocess.run(["git", "show", f"{commit}:{PROTOCOL_PATH}"],
                            check=True, capture_output=True, text=True, cwd=repo).stdout
    match = re.search(r"pub const PROTOCOL_VERSION: u32 = (\d+);", source)
    if not match:
        raise ValueError(f"no declared protocol version at {commit}")
    return int(match[1])


def source_commit(source: Path) -> str:
    head = subprocess.run(["git", "-C", str(source), "rev-parse", "HEAD"],
                          check=True, capture_output=True, text=True).stdout.strip()
    dirty = subprocess.run(["git", "-C", str(source), "status", "--porcelain"],
                           check=True, capture_output=True, text=True).stdout
    if dirty:
        raise VmError("management bootstrap source is not a clean checkout")
    return head



def read_json(text: str) -> dict:
    try:
        value = json.loads(text)
    except ValueError as exc:
        raise VmError("management bootstrap command returned invalid JSON") from exc
    if not isinstance(value, dict):
        raise VmError("management bootstrap command returned no object")
    return value


def start_management(current: TiloVm, startup: TiloVm, source: Path | None,
                     ref: str, create: list[str] | None) -> dict | None:
    """Finish the bridge before the caller can create its first session."""
    if create:
        startup.vm_cmd(*create, timeout=900)
    startup.vm_cmd("start", current.vm, timeout=600)
    return bootstrap_management(startup, current, source, ref) if source else None

def bootstrap_management(base: TiloVm, current: TiloVm, source: Path, ref: str) -> dict:
    """Use ordinary swap admission; the new client's readback enforces its protocol."""
    head = source_commit(source)
    result = read_json(base.run("--json", "swap", "vmd", "--slot", "vm", "--builder", "host",
                                 timeout=5400, slot=False, cwd=source))
    try:
        receipt_path = Path(result["receipt"])
        if not receipt_path.is_absolute():
            receipt_path = source / receipt_path
        receipt = json.loads(receipt_path.read_text())
    except (KeyError, OSError, ValueError, TypeError) as exc:
        raise VmError("management bootstrap has no readable successful swap receipt") from exc
    if (receipt.get("status"), receipt.get("component"), receipt.get("slot"), receipt.get("vm")) != (
            "success", "vmd", "vm", current.vm):
        raise VmError("management bootstrap receipt does not name this VM's vmd swap")
    if source_commit(source) != head:
        raise VmError("management bootstrap source changed during the swap")
    shown = read_json(current.run("--json", "slot", "show", "vm", timeout=120, slot=False))
    layers = (shown.get("slot") or {}).get("components") or {}
    layer = layers.get("vmd")
    if not isinstance(layer, str) or not re.fullmatch(r"[0-9a-f]{64}", layer):
        raise VmError("management bootstrap has no verified vmd layer readback")
    if receipt.get("layer") != layer:
        raise VmError("management bootstrap layer differs from its activation receipt")
    return {"ref": ref, "commit": head, "components": [
        {"component": "vmd", "slot": "vm", "result": result, "layer": layer}],
        "slot_layers": {"vm": layers}}


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: management.py BASE_COMMIT TARGET_COMMIT")
    print(str(protocol_at(sys.argv[1]) != protocol_at(sys.argv[2])).lower())
