"""Audit captured component replacement evidence; never deploy or grant trust.

Receipts establish what the existing manager reported, not artifact authenticity.
Authentication and native closure remain the signed cohort installer's job.
"""

import hashlib
import json
import re
from pathlib import Path


class ReplacementError(ValueError):
    """A required observation is missing or contradicts another observation."""


def require(condition, message):
    if not condition:
        raise ReplacementError(message)


def receipt(data, vm, command, status="success"):
    require(isinstance(data, dict), f"{command}: missing canonical receipt")
    require(data.get("tool") == "tilo-vm" and data.get("vm") == vm,
            f"{command}: receipt tool/VM mismatch")
    require(data.get("status") == status, f"{command}: expected {status} receipt")
    words = data.get("command", [])
    require(isinstance(words, list) and all(isinstance(w, str) for w in words),
            f"{command}: invalid command")
    wanted = command.split()
    require(any(words[i:i + len(wanted)] == wanted for i in range(len(words))),
            f"{command}: receipt belongs to another command")
    return data.get("result") if status == "success" else data.get("error")


def shown(data, vm, slot):
    result = receipt(data, vm, "slot show")
    require(isinstance(result, dict) and isinstance(result.get("slot"), dict),
            "slot show: missing slot object")
    info = result["slot"]
    require(info.get("name") == slot,
            "slot show: wrong or missing slot")
    require(type(info.get("generation")) is int and info["generation"] >= 0 and
            isinstance(info.get("components"), dict), "slot show: invalid generation/components")
    require(isinstance(result.get("files"), list) and
            all(isinstance(f, dict) for f in result["files"]), "slot show: invalid inventory")
    return result


def provenance(shown, component):
    records = shown.get("provenance")
    require(isinstance(records, dict), f"{component}: selected layer import provenance unavailable")
    value = records.get(component)
    require(isinstance(value, dict) and value.get("component") == component,
            f"{component}: selected layer import provenance unavailable")
    require(re.fullmatch(r"[0-9a-f]{40}", value.get("source_commit") or ""),
            f"{component}: full source commit unavailable")
    require(value.get("dirty_tree_hash") is None or
            re.fullmatch(r"[0-9a-f]{64}", value.get("dirty_tree_hash") or ""),
            f"{component}: invalid dirty tree hash")
    for field in ("target", "toolchain", "cargo_lock_hash"):
        require(isinstance(value.get(field), str) and bool(value[field]),
                f"{component}: {field} unavailable")
    require(re.fullmatch(r"[0-9a-f]{64}", value["cargo_lock_hash"]),
            f"{component}: invalid Cargo.lock hash")
    return value


def audit(evidence, vm, slot, source):
    """Check a fresh slot's successful shell replacement and failed recovery.

    Input observations are local evidence, not a remote attestation. A passing
    result means these captures agree; it cannot substitute for running the
    acceptance journey or the independent package signature verification.
    """
    require(re.fullmatch(r"replace-[0-9a-f]{12}", slot), "expected fresh replace-<12 hex> slot")
    require(re.fullmatch(r"[0-9a-f]{40}", source), "expected full candidate source commit")
    created = receipt(evidence["create-run"], vm, f"slot create {slot}")
    require(isinstance(created, dict) and created.get("created") is True,
            "slot create: fresh ownership not acknowledged")
    info = created.get("slot", {})
    user = "tds-" + slot
    require(isinstance(info, dict) and info.get("name") == slot and info.get("kind") == "headless" and
            info.get("user") == user and info.get("generation") == 0 and
            info.get("components") == {}, "slot create: fixture is not an empty disposable slot")
    before = shown(evidence["before-run"], vm, slot)
    active = shown(evidence["active-run"], vm, slot)
    recovered = shown(evidence["recovered-run"], vm, slot)
    sdk = provenance(active, "apps+runtime")
    candidate = provenance(active, "shell")
    baseline = provenance(before, "shell")
    require(provenance(before, "apps+runtime") == sdk,
            "activation: runtime provenance changed")
    for field in ("target", "toolchain", "cargo_lock_hash"):
        require(baseline[field] == sdk[field], f"baseline/runtime: {field} mismatch")
    require(candidate["source_commit"] == source, "shell: unexpected source commit")
    for field in ("source_commit", "dirty_tree_hash", "target", "toolchain", "cargo_lock_hash"):
        require(candidate.get(field) == sdk.get(field), f"shell/runtime: {field} mismatch")
    components = active["slot"].get("components", {})
    for component in ("shell", "apps+runtime"):
        require(re.fullmatch(r"[0-9a-f]{64}", components.get(component, "")),
                f"{component}: active layer hash unavailable")
    require(any(f.get("component") == "apps+runtime" and
                f.get("layer") == components["apps+runtime"] and
                re.fullmatch(r"/usr/lib/tilo-runtime/[^/]+/lib/libruntime_dylib\.so", f.get("path", ""))
                for f in active.get("files", [])), "runtime: selected layer inventory unavailable")
    activation = evidence["activate-run"]
    applied = receipt(activation, vm, "swap shell")
    require(activation.get("slot") == slot and activation.get("component") == "shell" and
            activation.get("layer") == components["shell"], "activation: target/layer mismatch")
    require(isinstance(applied, dict) and applied.get("slot") == slot and
            applied.get("generation") == active["slot"].get("generation") and
            applied.get("changed") is True and applied.get("policy") == "session_restart",
            "activation: successful session activation unavailable")
    require(active["slot"]["generation"] > before["slot"]["generation"] and
            components["shell"] != before["slot"].get("components", {}).get("shell"),
            "activation: no replacement observed")
    require(components["apps+runtime"] == before["slot"].get("components", {}).get("apps+runtime"),
            "activation: runtime cohort changed during shell replacement")
    failure = evidence["fail-run"]
    error = receipt(failure, vm, "swap shell", "failure")
    require(failure.get("slot") == slot and failure.get("component") == "shell" and
            re.fullmatch(r"[0-9a-f]{64}", failure.get("layer", "")) and
            failure["layer"] != components["shell"], "failure: target/layer mismatch")
    detail = error.get("error", "") if isinstance(error, dict) else ""
    generation = active["slot"]["generation"]
    require(re.search(rf"\brestored generation {generation}\b", detail) and "also failed" not in detail,
            "failure: successful recovery unavailable (preflight rejection is not rollback)")
    for field in ("name", "user", "generation", "components"):
        require(recovered["slot"].get(field) == active["slot"].get(field),
                f"recovery: {field} changed")
    require(recovered.get("provenance") == active.get("provenance"),
            "recovery: provenance changed")
    require(recovered.get("files") == active.get("files"), "recovery: file inventory changed")
    fixture = evidence["fixture-before"]
    require(isinstance(fixture, dict), "fixture: missing observation")
    require(fixture == evidence["fixture-after"], "recovery: fixture state changed")
    require(fixture.get("vm") == vm and fixture.get("slot") == slot and
            fixture.get("user") == user and type(fixture.get("uid")) is int and
            fixture["uid"] > 0, "fixture: dedicated user unavailable")
    home = fixture.get("home", "")
    require(home in (f"/home/{user}", f"/var/home/{user}") and
            fixture.get("identity_root") == home + "/.local/share/tilo/test-identity" and
            re.fullmatch(r"[0-9a-f]{64}", fixture.get("sentinel_sha256", "")),
            "fixture: isolated home, identity root or sentinel unavailable")
    removed = receipt(evidence["remove-run"], vm, f"slot rm {slot}")
    require(isinstance(removed, dict) and removed.get("removed") is True,
            "teardown: manager removal not acknowledged")
    teardown = evidence["teardown"]
    require(isinstance(teardown, dict) and teardown.get("vm") == vm and teardown.get("slot") == slot and
            teardown.get("user") == user and teardown.get("uid") == fixture["uid"] and
            teardown.get("home") == home, "teardown: probe target mismatch")
    for field in ("slot_present", "account_present", "home_present", "session_active",
                  "user_manager_active", "session_dropins_present", "user_dropins_present"):
        require(teardown.get(field) is False, f"teardown: {field} absent/unproven")
    return {"status": "captured_evidence_consistent", "vm": vm, "slot": slot,
            "source_commit": source, "layers": components,
            "live_execution": "not_performed_by_auditor"}


FILES = ("create-run", "before-run", "activate-run", "active-run", "fail-run",
         "recovered-run", "fixture-before", "fixture-after", "remove-run", "teardown")


def audit_directory(directory: Path, vm: str, slot: str, source: str):
    evidence, hashes = {}, {}
    for name in FILES:
        path = directory / (name + ".json")
        require(path.stat().st_size <= 8 * 1024 * 1024, f"{name}: evidence too large")
        raw = path.read_bytes()
        require(len(raw) <= 8 * 1024 * 1024, f"{name}: evidence too large")
        evidence[name] = json.loads(raw)
        hashes[path.name] = hashlib.sha256(raw).hexdigest()
    result = audit(evidence, vm, slot, source)
    result["input_sha256"] = hashes
    return result
