#!/usr/bin/env python3
"""Raw guest EFI evidence for the disposable owner-only VM test, not attestation."""
import json
from pathlib import Path
import sys
import uuid

GUID = "8be4df61-93ca-11d2-aa0d-00e098032b8c"


def collect(nonce, phase, efivars=Path("/sys/firmware/efi/efivars"),
            boot_id=Path("/proc/sys/kernel/random/boot_id")):
    values = {}
    for name in ("SecureBoot", "SetupMode"):
        raw = (efivars / f"{name}-{GUID}").read_bytes()
        if len(raw) != 5 or raw[4] not in (0, 1):
            raise ValueError(f"invalid raw {name} EFI variable")
        values[name] = raw[4]
    return dict(schema=1, nonce=nonce, phase=phase,
                boot_id=boot_id.read_text().strip(), **values)


def check(report, nonce, phase, secure_boot, setup_mode):
    expected = {"schema", "nonce", "phase", "boot_id", "SecureBoot", "SetupMode"}
    if not isinstance(report, dict) or set(report) != expected:
        raise ValueError("missing or unexpected evidence fields")
    if (type(report["schema"]) is not int or report["schema"] != 1
            or report["nonce"] != nonce or report["phase"] != phase):
        raise ValueError("wrong evidence identity or phase")
    if str(uuid.UUID(report["boot_id"])) != report["boot_id"]:
        raise ValueError("invalid boot ID")
    for name, value in (("SecureBoot", secure_boot), ("SetupMode", setup_mode)):
        if type(report[name]) is not int or report[name] != value:
            raise ValueError(f"unexpected {name}; expected {value}")


def validate(before, current, nonce, phase):
    check(before, nonce, "before", 0, 1)
    if phase == "before":
        return
    if phase not in ("refused", "after"):
        raise ValueError("unknown phase")
    check(current, nonce, phase, int(phase == "after"), int(phase != "after"))
    changed = before["boot_id"] != current["boot_id"]
    if changed != (phase == "after"):
        raise ValueError("stale post-reboot evidence or unexpected pre-enrollment reboot")


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate evidence field")
        result[key] = value
    return result


def main():
    mode, nonce, *inputs = sys.argv[1:]
    if mode == "collect":
        print(json.dumps(collect(nonce, inputs[0]), sort_keys=True))
    else:
        reports = [json.loads(Path(path).read_text(), object_pairs_hook=unique_object)
                   for path in inputs]
        validate(reports[0], reports[-1], nonce, mode)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, TypeError, KeyError, IndexError, OSError) as error:
        sys.exit(f"Secure Boot evidence rejected: {error}")
