#!/usr/bin/env python3
"""Render an isolated AWS amd64 seed; never submits or modifies live resources."""
import argparse
import json
import re

import trusted_seed_generation as generation

IMAGES = [
    "code.podesta.ai/qos/ci-fast@sha256:fd2d462eaa130052fded4fbb05c2e9c7e44b6d6fa70b3bc6eae484447920551a",
    "code.podesta.ai/qos/ci-control@sha256:cc33624d156302fa0c62c065b9babf1d18c6d3103116ff9b0ecf4e0f316d9ce5",
]
SEED = "ci-trusted-tools-ebs-seed-v1"
CLONE = "ci-trusted-tools-ebs-clone-v1"
SNAPSHOT = "ci-trusted-tools-ebs-v1"


def render(node, zone, phase):
    if not node or not re.fullmatch(r"eu-central-1[a-z]", zone):
        raise ValueError("require explicit admitted node and eu-central-1 availability zone")
    result = generation.render(node, IMAGES, clone=phase == "clone",
                               snapshot=phase == "snapshot", fresh=True)
    if phase == "snapshot":
        result["metadata"]["name"] = SNAPSHOT
        result["metadata"]["annotations"].update({
            "seed-region": "eu-central-1", "seed-zone": zone,
            "seed-cluster": "aws-eu-central-1",
            "seed-architecture": "amd64", "architecture": "amd64",
        })
        result["spec"]["volumeSnapshotClassName"] = "agent-workspace-eks"
        result["spec"]["source"]["persistentVolumeClaimName"] = SEED
        return result
    pvc, job = result["items"]
    name = CLONE if phase == "clone" else SEED
    pvc["metadata"]["name"] = job["metadata"]["name"] = name
    pvc["spec"]["storageClassName"] = "agent-workspace-eks"
    if phase == "clone":
        pvc["spec"]["dataSource"]["name"] = SNAPSHOT
    pod = job["spec"]["template"]["spec"]
    pod["volumes"][0]["persistentVolumeClaim"]["claimName"] = name
    pod["runtimeClassName"] = "kata-qemu"
    pod["nodeSelector"].update({
        "topology.kubernetes.io/zone": zone,
        "kubernetes.io/arch": "amd64",
        "agent-sandbox.podesta.ai/kata": "true",
        "agent-sandbox.podesta.ai/runtime-ready": "true",
        "agent-sandbox.podesta.ai/pool": "forgejo-ci",
    })
    pod["tolerations"] = [{"key": "agent-sandbox.podesta.ai/dedicated",
                           "operator": "Equal", "value": "true", "effect": "NoSchedule"}]
    job["spec"]["activeDeadlineSeconds"] = 900
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node", required=True)
    parser.add_argument("--zone", required=True)
    parser.add_argument("--phase", choices=["seed", "snapshot", "clone"], required=True)
    args = parser.parse_args()
    print(json.dumps(render(args.node, args.zone, args.phase), indent=2))
