#!/usr/bin/env python3
"""Render isolated seed/clone Jobs; never submits resources itself."""
import argparse
import json

IMAGE = "code.podesta.ai/qos/tilo-builder@sha256:027e43c051f1f34ce9ac1539172af0e711958243115e2efd08901ffa58729c5a"
DIND = "docker.io/library/docker@sha256:aa3df78ecf320f5fafdce71c659f1629e96e9de0968305fe1de670e0ca9176ce"


def manifest(node, clone=False):
    name = "ci-trusted-builder-clone-probe" if clone else "ci-trusted-builder-seed-probe"
    script = r'''set -eu
apk add --no-cache e2fsprogs util-linux >/dev/null
if [ "$PROBE_MODE" = seed ]; then
  if blkid /dev/docker-storage >/dev/null 2>&1; then echo REFUSE_NONEMPTY_SEED; exit 1; fi
  mkfs.ext4 -F -E lazy_itable_init=1,lazy_journal_init=1 /dev/docker-storage >/dev/null
else
  test "$(blkid -s TYPE -o value /dev/docker-storage)" = ext4
fi
mkdir -p /var/lib/docker
mount -o noatime /dev/docker-storage /var/lib/docker
dockerd-entrypoint.sh --tls=false --host=unix:///var/run/docker.sock --storage-driver=overlay2 --mtu=1400 >/tmp/dockerd.log 2>&1 &
daemon=$!
cleanup() { kill -TERM "$daemon" 2>/dev/null || true; wait "$daemon"; sync; umount /var/lib/docker; }
trap cleanup EXIT
for i in $(seq 1 120); do docker info >/dev/null 2>&1 && break; sleep 1; done
test "$(docker info --format '{{.Driver}}')" = overlay2
test -z "$(docker ps -aq)"
test -z "$(docker volume ls -q)"
if [ "$PROBE_MODE" = clone ]; then
  test "$(cat /var/lib/docker/.trusted-image)" = "$TRUSTED_IMAGE"
  docker image inspect "$TRUSTED_IMAGE" --format 'CLONE_LOCAL_HIT {{.Id}} {{json .RepoDigests}}'
fi
start=$(date +%s)
docker pull "$TRUSTED_IMAGE"
stop=$(date +%s)
echo "PULL_SECONDS=$((stop-start))"
docker image inspect "$TRUSTED_IMAGE" --format 'VERIFIED_IMAGE {{.Id}} {{json .RepoDigests}}'
printf '%s' "$TRUSTED_IMAGE" > /var/lib/docker/.trusted-image
test ! -e /var/lib/docker/config.json
test ! -e /var/lib/docker/.docker
cleanup
trap - EXIT
echo CLEAN_DAEMON_STOP_AND_UNMOUNT
'''
    claim = dict(accessModes=["ReadWriteOnce"], volumeMode="Block", storageClassName="zfs-localpv-block-cache",
                 resources={"requests": {"storage": "50Gi"}})
    if clone:
        claim["dataSource"] = dict(apiGroup="snapshot.storage.k8s.io", kind="VolumeSnapshot", name="ci-trusted-builder-seed-snapshot")
    labels = {"experiment": "trusted-builder-seed", "authorized-repository": "qos"}
    pvc = dict(apiVersion="v1", kind="PersistentVolumeClaim", metadata=dict(name=name, namespace="agent-sandboxes", labels=labels), spec=claim)
    container = dict(name="probe", image=DIND, command=["/bin/sh", "-ec"], args=[script],
                     securityContext={"privileged": True},
                     env=[dict(name="DOCKER_TLS_CERTDIR", value=""), dict(name="DOCKER_HOST", value="unix:///var/run/docker.sock"),
                          dict(name="DOCKER_CONFIG", value="/registry-auth"), dict(name="PROBE_MODE", value="clone" if clone else "seed"),
                          dict(name="TRUSTED_IMAGE", value=IMAGE)],
                     resources={"requests": {"cpu": "2", "memory": "4Gi"}, "limits": {"cpu": "2", "memory": "4Gi"}},
                     volumeDevices=[dict(name="docker-storage", devicePath="/dev/docker-storage")],
                     volumeMounts=[dict(name="registry-auth", mountPath="/registry-auth", readOnly=True)])
    pod = dict(runtimeClassName="kata", restartPolicy="Never", automountServiceAccountToken=False,
               nodeSelector={"kubernetes.io/hostname": node}, terminationGracePeriodSeconds=120,
               tolerations=[dict(key="node-role.kubernetes.io/control-plane", operator="Exists", effect="NoSchedule")],
               containers=[container], volumes=[dict(name="docker-storage", persistentVolumeClaim={"claimName": name}),
                  dict(name="registry-auth", secret=dict(secretName="forgejo-pull-secret", items=[dict(key=".dockerconfigjson", path="config.json")]))])
    job = dict(apiVersion="batch/v1", kind="Job", metadata=dict(name=name, namespace="agent-sandboxes", labels=labels),
               spec=dict(activeDeadlineSeconds=900, backoffLimit=0, template=dict(metadata=dict(labels=labels,
                   annotations={"io.katacontainers.config.hypervisor.kernel": "/var/lib/kata/vmlinux-kvm"}), spec=pod)))
    return dict(apiVersion="v1", kind="List", items=[pvc, job])


def snapshot(node):
    return dict(apiVersion="snapshot.storage.k8s.io/v1", kind="VolumeSnapshot",
                metadata=dict(name="ci-trusted-builder-seed-snapshot", namespace="agent-sandboxes",
                              annotations={"trusted-image": IMAGE, "dind-image": DIND, "seed-node": node}),
                spec=dict(volumeSnapshotClassName="zfs-localpv-snap",
                          source={"persistentVolumeClaimName": "ci-trusted-builder-seed-probe"}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node", required=True)
    parser.add_argument("--clone", action="store_true")
    parser.add_argument("--snapshot", action="store_true", help="Only after clean seed completion and seed Pod deletion")
    args = parser.parse_args()
    if args.clone and args.snapshot:
        parser.error("choose clone or snapshot")
    print(json.dumps(snapshot(args.node) if args.snapshot else manifest(args.node, args.clone), indent=2))
