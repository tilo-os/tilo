#!/usr/bin/env python3
"""Reproduce a review candidate from committed source and retained public proofs.

Writes only adjacent local JSON files. No API calls, policy installation or dispatch.
"""
import hashlib
import json
import re
from pathlib import Path
import subprocess
import sys
import tomllib

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.append(str(ROOT / "tools/apps"))
import app_packaging_product as product
import app_release


def raw_source(source, path):
    return subprocess.check_output(["git", "show", source + ":" + path], cwd=ROOT)


def generate():
    inputs = json.loads((HERE / "inputs.json").read_text())
    source = inputs["source_sha"]
    product.digest(source, 40)
    # Derive required paths from the exact reviewed product contract loaded here.
    assert raw_source(source, "tools/app_packaging_product.py") == (ROOT / "tools/app_packaging_product.py").read_bytes()
    descriptor = inputs["descriptor"]
    native = json.loads((HERE / "native-semantic.json").read_text())
    pod = json.loads((HERE / "native-runtime.json").read_text())
    signer = json.loads((HERE / "runtime-publisher.json").read_text())
    assert native["status"] == signer["result"] == "passed"
    assert native["uid"] == 1000 and native["architecture"] == "x86_64"
    assert native["signed_root_corruption_rejected"] and native["runtime_payload_corruption_rejected"]
    assert native["verifier_bundle_sha256"] == signer["bundle_sha256"] == descriptor["sha256"]
    assert native["canonical_binaries"] == descriptor["canonical_binaries"]
    assert signer["runtime_binary_sha256"] == descriptor["canonical_binaries"]["tilo_runtime_bundle_sha256"]
    assert pod["phase"] == "Succeeded" and pod["exit_code"] == 0
    assert pod["image"] == native["builder_image_operator_assertion"] == descriptor["compatible_builder"]
    assert signer["source_sha"] == inputs["runtime_proof_source"]
    final = {"app_publishers": [inputs["app_publisher"]],
             "runtime_publisher": signer["runtime_publisher"],
             "verifier_sha256": descriptor["verifier_sha256"],
             "allowed_package_isa": {"x86_64": ["v3"]},
             "canonical_verifier_bundles": {"x86_64": descriptor}}
    final_raw = encode(final)
    policy = {"enabled": True, "qualification_only": True,
              "bundle_compatibility_approved": True, "service_user_id": inputs["service_user_id"],
              "builder_image": descriptor["compatible_builder"], "collection": inputs["collection"],
              "architectures": inputs["architectures"],
              "final_receipt_policy_sha256": hashlib.sha256(final_raw).hexdigest(),
              "trusted_files": {path: hashlib.sha256(raw_source(source, path)).hexdigest()
                                for path in sorted(product.REQUIRED_PATHS)}}
    # Read the app set without cargo, resolving only reviewed checkout manifests.
    tracked = subprocess.check_output(["git", "ls-tree", "-r", "--name-only", source, "--", "apps"],
                                      cwd=ROOT, text=True).splitlines()
    manifests = {path for path in tracked if path.endswith("/tilo-app.toml")}
    assert manifests == {path.relative_to(ROOT).as_posix() for path in (ROOT / "apps").glob("*/tilo-app.toml")}
    for manifest in manifests:
        for path in (manifest, str(Path(manifest).parent / "Cargo.toml").replace("\\", "/")):
            assert raw_source(source, path) == (ROOT / path).read_bytes(), path
    catalog = app_release.app_catalog(ROOT)
    config = tomllib.loads(raw_source(source, "dist/apps/app-collections.toml").decode())
    apps = app_release.resolve_collection(inputs["collection"], catalog, config)
    summary = {"source_sha": source, "coordinator_policy_sha256": product.core.receipt_key(policy),
               "coordinator_file_sha256": hashlib.sha256(encode(policy)).hexdigest(),
               "final_receipt_policy_sha256": policy["final_receipt_policy_sha256"],
               "trusted_file_count": len(policy["trusted_files"]),
               "selected_apps": apps, "selected_app_count": len(apps),
               "baseline": None, "baseline_evidence": None,
               "installed": False, "dispatched": False, "published": False}
    service_doc = raw_source(source, "experiments/ci-forgejo/PACKAGING-SERVICE.md").decode()
    controller_paths = re.search(r"```\n(ci/python.py\n.*?)\n```", service_doc, re.S).group(1).splitlines()
    assert len(controller_paths) == len(set(controller_paths)) == 12
    controller = {"source_sha": source, "files": {
        "tools/" + path: hashlib.sha256(raw_source(source, "tools/" + path)).hexdigest()
        for path in controller_paths}}
    return {"final-policy.json": final_raw, "coordinator.json": encode(policy),
            "review.json": encode(summary), "controller-source-hashes.json": encode(controller)}


def encode(value):
    return (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()


if __name__ == "__main__":
    for name, raw in generate().items():
        (HERE / name).write_bytes(raw)
    print("Prepared local qualification-only policy; nothing installed or dispatched.")
