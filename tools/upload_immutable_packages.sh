#!/usr/bin/env bash
# Upload file/URL pairs concurrently while preserving immutable package semantics.

set -euo pipefail

manifest=${1:?usage: upload_immutable_packages.sh MANIFEST}
workers=${TILO_PACKAGE_UPLOAD_WORKERS:-8}
temp_root=${RUNNER_TEMP:-${TMPDIR:-/tmp}}

[[ $workers =~ ^[1-9][0-9]*$ ]] || {
  printf 'TILO_PACKAGE_UPLOAD_WORKERS must be a positive integer\n' >&2
  exit 2
}
[[ -n ${TILO_PACKAGE_TOKEN:-} ]] || {
  printf 'TILO_PACKAGE_TOKEN is required\n' >&2
  exit 2
}

declare -a files=() urls=()
while IFS=$'\t' read -r file url extra || [[ -n $file || -n $url || -n ${extra:-} ]]; do
  [[ -n $file && -n $url && -z ${extra:-} ]] || {
    printf 'invalid immutable-upload manifest row\n' >&2
    exit 2
  }
  [[ -f $file ]] || {
    printf 'immutable-upload input does not exist: %s\n' "$file" >&2
    exit 2
  }
  files+=("$file")
  urls+=("$url")
done < "$manifest"

count=${#files[@]}
(( count > 0 )) || exit 0
(( workers < count )) || workers=$count

state=$(mktemp -d "$temp_root/tilo-package-uploads.XXXXXX")
cleanup() { rm -rf -- "$state"; }
trap cleanup EXIT

upload_one() {
  local index=$1 file=${files[$1]} url=${urls[$1]}
  local response="$state/$index.response" remote="$state/$index.remote"
  local errors="$state/$index.err" status redirect

  if ! status=$(curl --silent --show-error --retry 3 --retry-all-errors \
    --output "$response" --write-out '%{http_code} %{redirect_url}' \
    --header "Authorization: Bearer $TILO_PACKAGE_TOKEN" \
    --request PUT --upload-file "$file" "$url" 2>"$errors"); then
    printf 'upload request failed: %s\n' "$url" >> "$errors"
    return 1
  fi
  redirect=${status#* }
  status=${status%% *}

  case "$status" in
    3??)
      # A moved package base (an owner rename) answers every upload with a
      # redirect. Following it would resend the token and the body to another
      # URL, so name the new location and let the base URL be corrected.
      printf 'package upload was redirected (HTTP %s) to %s: %s\n' "$status" "${redirect:-an unnamed location}" "$url" >> "$errors"
      printf 'the package base URL has moved; update TILO_PACKAGE_PUBLIC_BASE_URL (or the caller'"'"'s base) to the new location\n' >> "$errors"
      return 1
      ;;
    200|201|204)
      printf 'published %s\n' "${file##*/}" > "$state/$index.out"
      ;;
    409)
      if ! status=$(curl --fail --silent --show-error --retry 3 --retry-all-errors \
        --header "Authorization: Bearer $TILO_PACKAGE_TOKEN" \
        "$url" --output "$remote" --write-out '%{http_code}' 2>>"$errors"); then
        printf 'immutable conflict verification failed: %s\n' "$url" >> "$errors"
        return 1
      fi
      if [[ $status != 200 ]]; then
        printf 'immutable conflict verification returned HTTP %s: %s\n' "$status" "$url" >> "$errors"
        return 1
      fi
      if ! cmp --silent "$file" "$remote"; then
        printf 'immutable package version conflicts: %s\n' "$url" >> "$errors"
        return 1
      fi
      printf 'already published identically: %s\n' "${file##*/}" > "$state/$index.out"
      ;;
    *)
      if [[ -s $response ]]; then
        cat "$response" >> "$errors"
        printf '\n' >> "$errors"
      fi
      printf 'immutable package upload returned HTTP %s: %s\n' "$status" "$url" >> "$errors"
      return 1
      ;;
  esac
}

run_worker() {
  local slot=$1 index result
  for ((index = slot; index < count; index += workers)); do
    result=0
    upload_one "$index" || result=$?
    printf '%s\n' "$result" > "$state/$index.status"
  done
}

declare -a pids=()
for ((slot = 0; slot < workers; slot++)); do
  run_worker "$slot" &
  pids+=("$!")
done
for pid in "${pids[@]}"; do
  wait "$pid"
done

failed=0
for ((index = 0; index < count; index++)); do
  status=$(<"$state/$index.status")
  if [[ $status == 0 ]]; then
    cat "$state/$index.out"
  else
    cat "$state/$index.err" >&2
    failed=1
  fi
done
exit "$failed"
