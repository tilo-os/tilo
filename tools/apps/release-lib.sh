#!/usr/bin/env bash
# What the app and engine release steps share.
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

# How many apps compile at once in a release build: the cores and the memory the
# job is given, whichever runs out first, at 2 GiB a job.
#
# t gives a job its cores (`CARGO_BUILD_JOBS`) and its memory in the job's
# cgroup: the booking in `memory.low`, and `memory.high` (or, for a fixed limit,
# `memory.max`) at the booking plus room for the page cache of what it reads. A
# release link maps the rlibs of everything it links, so jobs past the booking
# are the ones a node ends. Outside t no cgroup bounds the job, and the machine's
# own memory is the bound there.
cargo_jobs() {
  local cpus memory limit quota period jobs file
  cpus=$(nproc)
  if read -r quota period 2>/dev/null < /sys/fs/cgroup/cpu.max && [[ $quota != max ]]; then
    cpus=$(( (quota + period - 1) / period ))
  fi
  memory=$(( $(sed -n 's/^MemTotal: *\([0-9]*\) kB$/\1/p' /proc/meminfo) << 10 ))
  for file in memory.low memory.max memory.high; do
    [[ -r /sys/fs/cgroup/$file ]] || continue
    limit=$(<"/sys/fs/cgroup/$file")
    [[ $limit =~ ^[1-9][0-9]*$ ]] || continue
    (( limit < memory )) && memory=$limit
  done
  jobs=$(( memory / (2 << 30) ))
  (( jobs > cpus )) && jobs=$cpus
  if [[ ${CARGO_BUILD_JOBS:-} =~ ^[1-9][0-9]*$ ]] && (( CARGO_BUILD_JOBS < jobs )); then
    jobs=$CARGO_BUILD_JOBS
  fi
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
