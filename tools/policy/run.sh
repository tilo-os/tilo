#!/usr/bin/env bash
# Fast, deterministic repository-policy suite shared by local development and CI.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"

run() {
  local label=$1 status=0
  shift
  printf '\n==> %s\n' "$label"
  "$@" || status=$?
  if (( status != 0 )); then
    printf '::error::repository policy check failed: %s (exit %s)\n' "$label" "$status"
    exit "$status"
  fi
}

# Tests build Git repositories of their own. Under t, the suite finds the tree
# through a throwaway repository named by GIT_DIR (tools/ci/t-git-index.sh),
# which would take over their Git commands.
run_tests() {
  local label=$1
  shift
  run "$label" env -u GIT_DIR -u GIT_WORK_TREE "$@"
}

run 'Source blobs are text and LF or explicitly pinned assets' \
  python3 tools/policy/check_source_content.py
run 'Tracked files reference no internal tracker' \
  python3 tools/policy/check_public_references.py
run 'Workspace membership' ./scripts/check-workspace-complete.sh
run 'Repository layout' python3 tools/policy/check_repository_layout.py
run 'Integration tests are compiled by a test target' \
  python3 tools/policy/check_integration_test_modules.py
run 'Checked-in capnp bindings match their schemas' \
  python3 tools/policy/check_capnp_bindings.py
run 'Interface schemas match the code and their versions' \
  python3 tools/policy/check_interfaces.py check
run 'Versioned on-disk formats name the oldest version they bring up' \
  python3 tools/policy/check_format_migrations.py check
run 'Workflow timeouts' python3 tools/policy/check_workflow_timeouts.py
run 'Workflow dependency and supply-chain invariants' \
  bash os/tests/workflow_dependency_invariants.sh
run 'Path-filtered package dependency triggers' \
  python3 tools/policy/check_workflow_package_paths.py \
  --check tilo-advisory-lock .forgejo/workflows/build-ci-builder.yml \
  --check tilo-crypto 't.toml#development-signing-context-x86_64' \
  --check tilo-fs .forgejo/workflows/ci-filesystem.yml \
  --check tilo-relay .forgejo/workflows/release-relay.yml \
  --check tilo-relay 't.toml#image-relay' \
  --check registry-server 't.toml#image-registry-server' \
  --check tilo-runtime 't.toml#runtime-macos' \
  --check tilo-app-cli 't.toml#sdk-linux'
run 'Developer docs pages and links' python3 tools/docs/developer_docs.py check
run 'Code map is current' python3 tools/docs/codemap.py --check
run_tests 'Repository policy unit tests' \
  python3 -m unittest discover -s tools/tests -p 'test_*.py' --durations 20
run_tests 'Compiler-cache helper tests' bash tools/tests/ci_sccache.sh
run_tests 'Cargo shim tests' bash tools/tests/cargo_shim.sh
run_tests 'CI receipt tests' bash tools/tests/test_ci_receipt.sh
run_tests 'Builder candidate workflow tests' python3 os/tests/test_builder_candidate_workflow.py
run_tests 'Signed development board dispatch and policy tests' python3 os/tests/test_signed_development_board.py
run 'Every test suite outside Cargo is gated or explained' python3 tools/ci/suites.py check
run 'Codex hook manifest' bash -c 'python3 -m json.tool .codex/hooks.json >/dev/null'
run_tests 'Codex CI watcher tests' python3 -m unittest discover -s .codex/hooks -p 'test_*.py'
run 'Application text-stack contract' python3 tools/policy/check_app_text_contract.py
run 'Copy used by source is in every shipped catalogue' python3 tools/policy/check_app_copy.py
run 'Copy follows the house language and glossary' python3 tools/policy/check_copy_language.py
run 'Application resource/action contract' \
  python3 tools/policy/check_app_resource_action_contract.py
run 'AT-SPI endpoint contract' python3 tools/policy/check_atspi_endpoint_contract.py
run 'OS component manifest' python3 tools/policy/check_component_manifest.py
run 'Dev image stays out of production' python3 tools/policy/check_dev_image_contract.py
run 'Sealed images share one UKI pipeline' python3 tools/policy/check_uki_pipeline.py
run 'Image builders copy what they compile, and only that' \
  python3 tools/policy/check_image_builder_inputs.py
run 'Dev kernel build follows the release build' python3 tools/policy/check_kernel_dev_build.py
run 'Dev-override features stay out of production images' \
  python3 tools/policy/check_dev_overrides_contract.py
run 'UI host-inputs contract' python3 tools/policy/check_ui_host_inputs_contract.py
run 'UI clock contract' python3 tools/policy/check_ui_clock_contract.py
run 'Animations are composed from the motion library' python3 tools/policy/check_motion_library.py
run 'Dates, times, numbers and sizes go through ui::format' python3 tools/policy/check_formats.py
run 'Settings are written through typed keys' python3 tools/policy/check_settings_writes.py
run 'Widgets never write the document' python3 tools/policy/check_widget_document_writes.py
run 'App content store contract' python3 tools/policy/check_app_content_store.py
run 'Apps and portable SDK crates hold no platform code' \
  python3 tools/policy/check_app_portability.py
run 'Apps declare the capabilities their code uses' \
  python3 tools/policy/check_app_capabilities.py
run 'Browser desktop registry matches app manifests' python3 tools/web/generate_web_desktop.py --check
run 'Raw cryptography stays behind tilo-seal' \
  python3 tools/policy/check_crypto_dependencies.py
run 'Own crates use one RustCrypto generation' \
  python3 tools/policy/check_crypto_generation.py
run 'Automerge apply contract' python3 tools/policy/check_automerge_apply_contract.py
run 'Document content stays behind the document layer' \
  python3 tools/policy/check_document_engine_boundary.py
run 'Desktop crates dependency boundary' \
  python3 tools/policy/check_desktop_model_dependencies.py
run 'Shell service modules stay below the compositor' \
  python3 tools/policy/check_shell_layers.py
run 'Shared app runtime carries no OS internals or dead weight' \
  python3 tools/policy/check_runtime_dylib_surface.py
run 'Browser wasm32 builds stay off the workspace-hack' \
  python3 tools/policy/check_wasm_workspace_hack.py
run 'No new interpreter scripts in the OS image' \
  python3 tools/policy/check_image_interpreters.py
run 'State seeded once keeps every version upgraded machines hold' \
  python3 tools/policy/check_seeded_state_history.py
run 'GStreamer is the only media stack' python3 tools/policy/check_media_stack.py
run 'rustls is the only TLS stack' python3 tools/policy/check_tls_stack.py
run 'No telemetry, analytics or crash-upload client' python3 tools/policy/check_no_telemetry.py
run 'No setup-specific references' python3 tools/policy/check_setup_neutral.py
run 'Vendored crates carry licence and provenance' \
  python3 tools/policy/check_vendor_provenance.py
run 'At most eight parameters, no escape hatch' \
  python3 tools/policy/check_too_many_arguments.py

printf '\nRepository policy suite OK\n'
