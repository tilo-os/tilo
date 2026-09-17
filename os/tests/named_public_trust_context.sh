#!/usr/bin/env bash
set -euo pipefail
engine="${1:-podman}"
command -v "$engine" >/dev/null
case "$(basename "$engine")" in
  podman) pull_flag=--pull=never ;;
  docker) pull_flag=--pull=false ;;
  *) echo 'fixture supports podman or docker' >&2; exit 2 ;;
esac
fixture=$(mktemp -d)
images=()
containers=()
cleanup() {
  for item in "${containers[@]}"; do "$engine" rm "$item" >/dev/null 2>&1 || :; done
  for item in "${images[@]}"; do "$engine" rmi "$item" >/dev/null 2>&1 || :; done
  rm -rf -- "$fixture"
}
trap cleanup EXIT
mkdir -p "$fixture/context/official" "$fixture/development"
printf 'official-public-only\n' > "$fixture/context/official/cosign.pub"
printf 'development-public-only\n' > "$fixture/development/cosign.pub"
cat > "$fixture/context/Containerfile" <<'EOF'
FROM scratch AS public-trust
COPY official/cosign.pub /cosign.pub
FROM scratch
COPY --from=public-trust /cosign.pub /trust/cosign.pub
EOF
for mode in official development; do
  args=()
  if [[ "$mode" == development ]]; then
    args+=(--build-context "public-trust=$fixture/development")
  fi
  "$engine" build "$pull_flag" --network=none --no-cache --iidfile "$fixture/$mode.iid" "${args[@]}" -f "$fixture/context/Containerfile" "$fixture/context"
  image=$(cat "$fixture/$mode.iid")
  images+=("$image")
  container=$("$engine" create "$image" /bin/true)
  containers+=("$container")
  "$engine" cp "$container:/trust/cosign.pub" "$fixture/$mode.pub"
  printf '%s-public-only\n' "$mode" > "$fixture/expected"
  cmp "$fixture/expected" "$fixture/$mode.pub"
done
printf 'PASS: default and named development contexts select distinct public bytes\n'
