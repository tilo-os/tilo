#!/usr/bin/env python3
"""Render generation two from the clean generation-one snapshot, never job disks."""
import argparse
import json
import re

import docker_seed_probe as initial

SNAPSHOT = "ci-trusted-tools-v2"
SOURCE = "ci-trusted-builder-seed-snapshot"


def render(node, images, clone=False, snapshot=False, fresh=False):
    if len(images) != 2 or len(set(images)) != 2 or any(
        not re.fullmatch(r"code\.podesta\.ai/qos/[^\s]+@sha256:[0-9a-f]{64}", image)
        for image in images
    ):
        raise ValueError("supply two distinct immutable qOS image references")
    suffix = "-ax41" if node == "talos-2a014f91aa0832" else ""
    snapshot_name = SNAPSHOT + suffix
    name = ("ci-trusted-tools-clone-v2" if clone else "ci-trusted-tools-seed-v2") + suffix
    if snapshot:
        result = initial.snapshot(node)
        result["metadata"]["name"] = snapshot_name
        result["metadata"]["annotations"]["trusted-extra-images"] = json.dumps(images)
        result["spec"]["source"]["persistentVolumeClaimName"] = "ci-trusted-tools-seed-v2" + suffix
        return result
    result = initial.manifest(node, clone=not fresh or clone)
    pvc, job = result["items"]
    pvc["metadata"]["name"] = job["metadata"]["name"] = name
    if "dataSource" in pvc["spec"]:
        pvc["spec"]["dataSource"]["name"] = snapshot_name if clone else SOURCE
    pod = job["spec"]["template"]["spec"]
    pod["volumes"][0]["persistentVolumeClaim"]["claimName"] = name
    container = pod["containers"][0]
    container["env"].append(dict(name="EXTRA_TRUSTED_IMAGES", value=" ".join(images)))
    container["env"].append(dict(name="VERIFY_ALL_LOCAL", value="true" if clone else "false"))
    extra = r'''
for image in $EXTRA_TRUSTED_IMAGES; do
  if [ "$VERIFY_ALL_LOCAL" = true ]; then
    docker image inspect "$image" --format 'EXTRA_LOCAL_HIT {{.Id}} {{json .RepoDigests}}'
  fi
  start=$(date +%s)
  docker pull "$image"
  stop=$(date +%s)
  echo "EXTRA_PULL_SECONDS=$((stop-start)) IMAGE=$image"
  docker image inspect "$image" --format 'EXTRA_VERIFIED_IMAGE {{.Id}} {{json .RepoDigests}}'
done
test -z "$(docker ps -aq)"
test -z "$(docker volume ls -q)"
printf '%s\n' "$EXTRA_TRUSTED_IMAGES" > /var/lib/docker/.trusted-extra-images
'''
    marker = "test ! -e /var/lib/docker/config.json"
    assert marker in container["args"][0]
    container["args"][0] = container["args"][0].replace(marker, extra + marker, 1)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node", required=True)
    parser.add_argument("--image", action="append", required=True)
    phase = parser.add_mutually_exclusive_group()
    phase.add_argument("--clone", action="store_true")
    phase.add_argument("--snapshot", action="store_true")
    parser.add_argument("--fresh", action="store_true", help="Fresh disk for a node without an existing trusted seed")
    args = parser.parse_args()
    print(json.dumps(render(args.node, args.image, args.clone, args.snapshot, args.fresh), indent=2))
