#!/usr/bin/env bash
# What the app and engine release steps share (tools/apps/t-release.sh, t-engine-release.sh).
# Sourced after tools/ci/publish/lib.sh, not run.

release_only() {
  [[ $(channel) == release ]] || die "releases are signed and published from stable or a release tag, not ${T_REF}"
  require_commit
}

# A file holding secret $1's value, readable only by this user, removed on exit.
secret_file() {
  local name=$1 file
  [[ -n ${!name:-} ]] || die "$name is not set: the check needs secrets = [\"$name\"]"
  file=$(mktemp "$scratch/$name.XXXXXX")
  (umask 077 && printf '%s' "${!name}" > "$file")
  unset "$name"
  printf '%s' "$file"
}

# Every core compiles, bounded by memory: 2 GiB per job.
cargo_jobs() {
  local cpus memory limit quota period jobs
  cpus=$(nproc)
  if read -r quota period 2>/dev/null < /sys/fs/cgroup/cpu.max && [[ $quota != max ]]; then
    cpus=$(( (quota + period - 1) / period ))
  fi
  memory=$(( $(sed -n 's/^MemTotal: *\([0-9]*\) kB$/\1/p' /proc/meminfo) << 10 ))
  if read -r limit 2>/dev/null < /sys/fs/cgroup/memory.max && [[ $limit =~ ^[0-9]+$ ]]; then
    (( limit < memory )) && memory=$limit
  fi
  jobs=$(( memory / (2 << 30) ))
  (( jobs > cpus )) && jobs=$cpus
  (( jobs < 1 )) && jobs=1
  printf '%s' "$jobs"
}

# The commit's time in seconds, which the release sequence and the packages' timestamps are.
# The trusted steps on x86_64 run without Git (t's x86 micro-VMs have no room for a Git disk
# beside the needs' outputs), so it comes from the forge, which serves the public history.
commit_epoch() {
  local commit=${1:-$T_COMMIT} date
  if git rev-parse --verify --quiet "$commit^{commit}" >/dev/null 2>&1; then
    git show -s --format=%ct "$commit"
    return
  fi
  date=$(curl --fail --silent --show-error --proto '=https' --retry 3 \
    "https://code.tilo.so/api/v1/repos/tilo/tilo/git/commits/$commit?stat=false&verification=false&files=false" \
    | jq -er '.commit.committer.date') || die "could not read $commit's time from the forge"
  date -d "$date" +%s
}
