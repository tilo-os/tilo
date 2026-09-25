#!/usr/bin/env bash
# Fast, deterministic repository-policy suite shared by local development and CI.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"

run() {
  local label=$1
  shift
  printf '\n==> %s\n' "$label"
  "$@"
}

run 'Workspace membership' ./scripts/check-workspace-complete.sh
run 'Repository layout' python3 tools/policy/check_repository_layout.py
run 'Workflow timeouts' python3 tools/policy/check_workflow_timeouts.py
run 'Workflow dependency and supply-chain invariants' \
  bash os/tests/workflow_dependency_invariants.sh
run 'Path-filtered package dependency triggers' \
  python3 tools/policy/check_workflow_package_paths.py \
  --check atomic-file .forgejo/workflows/build-builder-image.yml \
  --check tilo-crypto .forgejo/workflows/development-source.yml \
  --check fs .forgejo/workflows/fs-fuse.yml \
  --check tilo-relay .forgejo/workflows/tilo-relay.yaml
run 'Repository policy unit tests' \
  python3 -m unittest discover -s tools/tests -p 'test_*.py'
run 'Compiler-cache helper tests' bash tools/tests/ci_sccache.sh
run 'Cargo shim tests' bash tools/tests/cargo_shim.sh
run 'CI receipt tests' bash tools/tests/test_ci_receipt.sh
run 'Builder candidate workflow tests' python3 os/tests/test_builder_candidate_workflow.py
run 'Every test suite outside Cargo is gated or explained' python3 tools/ci/suites.py check
run 'Codex hook manifest' bash -c 'python3 -m json.tool .codex/hooks.json >/dev/null'
run 'Codex CI watcher tests' python3 -m unittest discover -s .codex/hooks -p 'test_*.py'
run 'Application text-stack contract' python3 tools/policy/check_app_text_contract.py
run 'Application resource/action contract' \
  python3 tools/policy/check_app_resource_action_contract.py
run 'AT-SPI endpoint contract' python3 tools/policy/check_atspi_endpoint_contract.py
run 'OS component manifest' python3 tools/policy/check_component_manifest.py
run 'Dev image stays out of production' python3 tools/policy/check_dev_image_contract.py
run 'Dev kernel build follows the release build' python3 tools/policy/check_kernel_dev_build.py
run 'Dev-override features stay out of production images' \
  python3 tools/policy/check_dev_overrides_contract.py
run 'UI host-inputs contract' python3 tools/policy/check_ui_host_inputs_contract.py
run 'UI clock contract' python3 tools/policy/check_ui_clock_contract.py
run 'Raw cryptography stays behind tilo-seal' \
  python3 tools/policy/check_crypto_dependencies.py
run 'Automerge apply contract' python3 tools/policy/check_automerge_apply_contract.py
run 'Desktop model dependency boundary' \
  python3 tools/policy/check_desktop_model_dependencies.py

printf '\nRepository policy suite OK\n'
