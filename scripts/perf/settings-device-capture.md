# Settings latency on an activated cohort

Run only after the deployment owner hands off an unlocked slot. The controller
does not deploy, reboot, unlock, restart the compositor, close tabs or take
screenshots. It opens one fresh Settings window and pins its authenticated
surface, certified record, worker lifetime, signed executable and mapped SDK.
All existing tabs, including Notes, must remain unchanged. The fresh Settings
window stays open, including on failure, and its exact identity is retained.

First activate the normally qualified, source-matched userspace cohort through
`tilo-vm`, retaining its successful full-cohort activation receipt. A mixed layer
stack fails the existing receipt reader; do not substitute a prepared receipt
for an activation, or relabel an older SDK as the candidate. Management CLI/VMD
protocol compatibility is also required. Startup GPU timestamps must already be
enabled in the compositor; the collector enables diagnostics for its new app,
but never retrofits a renderer or refreshes an existing session. Missing GPU
capability is a failed capture, with diagnostics restored.

```sh
TILO_VM_CONFIG="$TASK_REGISTRY" python3 scripts/perf/device_settings_latency.py \
  --binary "$SOURCE_MATCHED_TILO_VM" --vm "$OWNED_VM" --slot "$OWNED_SLOT" \
  --cohort-receipt "$ACTIVATION_RECEIPT" --source-commit "$ACTIVATED_SHA" \
  --uid "$SLOT_UID" --pane bluetooth --reset-pane mouse_keyboard \
  --samples 100 --out "$NEW_PRIVATE_CAPTURE_DIRECTORY"
```

The pane and reset pane must both have mounted sidebar rows. The measured pane
must differ from the selected pane. Coordinates come from public
`inspect-navigation` and the exact `foreign-surface-ID` target; ambiguous,
unmounted or distorted geometry refuses input. Only ordinary `tilo-vm input
move` reaches the slot's kernel/libinput route. VM injected kernel input and
Yoga injected kernel input are labelled `slot_uinput_kernel`; neither measures
physical touchpad/device transport before the kernel, or panel pixel response.
Use an actual hardware movement capture for those distinct input origins.

Every measured movement must change the actual mounted and drawn sidebar hover,
without changing selection. Each must have exactly one positive hardware KMS
outcome, the same pointer identity, app observer/PID, render worker/frame, and
exact foreign-buffer/capture/presenter/compositor/GPU ownership. Missing,
discarded, stale, coalesced, foreign, unsupported or partial evidence fails.
Requested sample count and main-thread stage coverage are mandatory.

`report.json` contains input-to-KMS p50/p95/p99 and lower/upper elapsed CPU-region
distributions for delivery, client-to-send, worker render and present. These
elapsed regions include scheduling/logging; they are not CPU utilization.
App state being mounted/drawn is a separate witness from compositor KMS feedback.
Native Vello pass/dispatch device durations are in `native_gpu`; compositor GPU
recording and nested output device durations are in `compositor.gpu_costs`.
Do not sum nested or overlapping regions. `gpu_completion_wait_us` remains a
host wait, never GPU execution time. Reports do not set `performance_certified`.

The retained timeline can be independently checked offline:

```sh
python3 scripts/perf/physical_notes_stages.py \
  --capture "$CAPTURE_DIRECTORY" --settings-hover bluetooth \
  --require-foreign-capture --require-gpu-stages --out "$NEW_OFFLINE_REPORT"
```

The `--settings-pane` variant supports ordinary native-key navigation, not
compositor-consumed Ctrl+K. Existing `settings_effects.py` separately joins an
exact broker invocation to the mounted/drawn pane and KMS outcome:

```sh
python3 scripts/perf/settings_effects.py --log "$BOUNDED_ACTIVATION_LOG" \
  --invocation "$OBSERVED_INVOCATION_UUID" --pane mouse_keyboard \
  --process-id "$OBSERVED_NAMESPACE_PID" --observer-session "$OBSERVED_UUID"
```

That activation proof cannot certify palette opening, query latency, result
hover, or clicking a result. The reported Ctrl+K stall sequence still requires
a fresh owned palette/query/hover/activation capture with exact public result
targets and phase coverage. Until that maintained chain is observed, those
metrics remain unavailable; semantic routing success alone is insufficient.

The 120 Hz goal gives a separate 8.33 ms rendering budget. Do not infer display
cadence from deliberately paced input samples. Yoga Slim 7x 14Q8X9 specifications
list a 2944×1840 OLED with a maximum 90 Hz; verify the actual device's EDID and
active mode after its owner returns. Report physical presentation constraints
separately from the 120 Hz rendering benchmark. VM VKMS modes also do not
certify Yoga GPU throughput or physical display cadence.
