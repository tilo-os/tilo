#!/usr/bin/env bash
# Admit one paired development input without changing release defaults.
set -euo pipefail
[[ ${GITHUB_REF:-} == refs/heads/main ]] \
  && { [[ ${GITHUB_EVENT_NAME:-} == workflow_dispatch && ${HARDWARE_PROFILE:-} == lenovo-83ed ]] \
       || [[ ${GITHUB_EVENT_NAME:-} == workflow_call && ${BUILD_MODE:-} == dev ]]; } || {
  echo '::error::Windows provider candidates require a Lenovo main dispatch or a main dev build'; exit 1;
}
[[ ${FEX_PAYLOAD_CANDIDATE:-} =~ ^(registry|code)\.tilo\.so/tilo/[a-z0-9][a-z0-9._/-]*@sha256:[0-9a-f]{64}$ \
   && ${WINDOWS_GUEST_CANDIDATE:-} =~ ^(registry|code)\.tilo\.so/tilo/[a-z0-9][a-z0-9._/-]*@sha256:[0-9a-f]{64}$ ]] || {
  echo '::error::Windows provider candidates require two immutable registry digests'; exit 1;
}
