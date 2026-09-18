#!/usr/bin/env bash
# Native CI orchestration. Owns only its run-specific containers and volumes.
set -euo pipefail
target=${1:?target required}
runtime=${2:?immutable tooling image required}
source_dir=${3:?verified source directory required}
evidence=${4:?new evidence directory required}
case "$target" in x86_64) config=fedora-44-x86_64 ;; i686) config=fedora-44-i386 ;; *) exit 2 ;; esac
[[ "$runtime" =~ ^code\.podesta\.ai/qos/wine-tools@sha256:[0-9a-f]{64}$ ]]
[[ "${GITHUB_SHA:?}" =~ ^[0-9a-f]{40}$ ]]
[[ "${GITHUB_RUN_ID:?}" =~ ^[0-9]+$ ]]
test "$(uname -m)" = x86_64
test ! -e "$evidence"
test -s "$source_dir/wine-11.0-3.fc44.src.rpm"
mkdir -p "$evidence"
nonce=$(python3 -c 'import uuid; print(uuid.uuid4().hex)')
name="tilo-wine-${GITHUB_RUN_ID}-${target}-${nonce}"
inputs="$name-inputs"
outputs="$name-outputs"
resolver="$name-resolve"
builder="$name-build"
owned_containers=()
owned_volumes=()
resolver_owned=false
builder_owned=false
cleanup() {
    status=$?
    trap - EXIT
    # Copy diagnostics even if resolution, compilation or validation fails.
    if "$builder_owned"; then
        docker cp "$builder":/work/output "$evidence/raw" || true
    fi
    if "$resolver_owned"; then
        # Mock root.log includes package-manager/scriptlet diagnostics omitted
        # from its console summary; retain the full result directory on failure.
        docker cp "$resolver:/work/bundle/resolution" "$evidence/resolution" || true
        docker cp "$resolver:/work/bundle/build-source" "$evidence/build-source" || true
        for file in resolve.log repository.log resolved-config.log manifest.json \
            i686-execution.S i686-execution i686-compile.log i686-execution.log; do
            docker cp "$resolver:/work/bundle/$file" "$evidence/$file" || true
        done
    fi
    for container in "${owned_containers[@]}"; do docker rm -f "$container" >/dev/null || true; done
    for volume in "${owned_volumes[@]}"; do docker volume rm "$volume" >/dev/null || true; done
    exit "$status"
}
trap cleanup EXIT
# Existing names belong to another attempt; never clean them speculatively.
for container in "$resolver" "$builder"; do
    if docker inspect "$container" >/dev/null 2>&1; then
        echo "refusing existing container: $container" >&2
        exit 1
    fi
done
for volume in "$inputs" "$outputs"; do
    if docker volume inspect "$volume" >/dev/null 2>&1; then
        echo "refusing existing volume: $volume" >&2
        exit 1
    fi
done
docker pull "$runtime"
docker volume create "$inputs" >/dev/null
owned_volumes+=("$inputs")
docker volume create "$outputs" >/dev/null
owned_volumes+=("$outputs")
docker create --name "$resolver" --privileged --mount "type=volume,src=$inputs,dst=/work" \
    --entrypoint sleep "$runtime" infinity >/dev/null
owned_containers+=("$resolver")
resolver_owned=true
docker start "$resolver" >/dev/null
docker exec "$resolver" sh -c 'test "$(df -Pk /work | awk "NR==2 {print \$4}")" -ge 26214400'
docker cp "$source_dir" "$resolver:/work/source"
docker exec "$resolver" runuser -u tilo-build -- sudo -n /usr/bin/python3 /tilo/wine-mock-producer.py \
    resolve --target "$target" --runtime-image "$runtime" --pin /tilo/wine-source.json \
    --source /work/source --config "$config" --output /work/bundle --jobs 2
snapshot_tag="code.podesta.ai/qos/wine-resolution:candidate-${GITHUB_SHA}-${GITHUB_RUN_ID}-${target}"
docker exec "$resolver" tar -C /work/bundle -cf - . | docker import --platform linux/amd64 - "$snapshot_tag"
docker push "$snapshot_tag"
snapshot_ref=$(docker image inspect --format '{{index .RepoDigests 0}}' "$snapshot_tag")
[[ "$snapshot_ref" =~ ^code\.podesta\.ai/qos/wine-resolution@sha256:[0-9a-f]{64}$ ]]
printf '%s\n' "$snapshot_ref" > "$evidence/resolution-image.txt"
docker stop "$resolver" >/dev/null

# Fresh root and container storage; only the checked resolution bundle crosses.
docker create --name "$builder" --privileged --network none \
    --mount "type=volume,src=$inputs,dst=/inputs,readonly" \
    --mount "type=volume,src=$outputs,dst=/work" \
    --entrypoint sleep "$runtime" infinity >/dev/null
owned_containers+=("$builder")
builder_owned=true
docker start "$builder" >/dev/null
docker exec "$builder" runuser -u tilo-build -- sudo -n /usr/bin/python3 /tilo/wine-mock-producer.py \
    build --target "$target" --runtime-image "$runtime" --pin /tilo/wine-source.json \
    --bundle /inputs/bundle --output /work/output --jobs 2
docker exec "$builder" python3 /tilo/wine_rpm_contract.py import-mock "$target" /tilo/wine-source.json \
    /work/output /inputs/bundle /work/wine-output "$snapshot_ref"
artifact_tag="code.podesta.ai/qos/wine-rpms:candidate-${GITHUB_SHA}-${GITHUB_RUN_ID}-${target}"
docker exec "$builder" tar -C /work -cf - wine-output | docker import --platform linux/amd64 - "$artifact_tag"
docker push "$artifact_tag"
artifact_ref=$(docker image inspect --format '{{index .RepoDigests 0}}' "$artifact_tag")
[[ "$artifact_ref" =~ ^code\.podesta\.ai/qos/wine-rpms@sha256:[0-9a-f]{64}$ ]]
printf '%s\n' "$artifact_ref" > "$evidence/wine-rpms-image.txt"
docker cp "$builder":/work/wine-output/provenance "$evidence/provenance"
