# Reference hardware for the latency KPIs

The KPI document ("Performance and cost KPIs — what we market and how we
measure it") sets two latency targets:

- **Input to photon:** p50 at most one refresh plus 8 ms, p99 at most two
  refreshes plus 8 ms.
- **Workspace switch:** first frame within one refresh, no dropped frame,
  at most 300 ms in total.

Two lanes measure them. Neither blocks a merge.

| Lane | Where | Clock | KPI ids |
|---|---|---|---|
| Nightly, compositor side | `perf-scheduled.yml` job `latency`, a headless debug compositor on the shared `runner` class, lavapipe | input taken by the loop → frame finished (headless has no flip) | `latency.input_to_frame.pointer`, `latency.workspace_switch.{first_frame,settled,dropped_frames}` |
| Weekly, report-only | a reference machine below, through `tilo-vm` | kernel input event → KMS presentation (the flip's vblank) | `hw.key_to_present.notes.<hz>hz`, `hw.workspace_switch.{first_frame,settled,dropped_frames}.<hz>hz` |

The nightly lane answers "did the compositor's own work get slower". Its input
comes through the control socket rather than libinput, and it has no panel, so
its numbers are not input to photon and are never marketed. The weekly lane is
the true number up to the presentation timestamp. Neither sees peripheral scan
time (USB polling) or panel pixel response: only a photodiode does, and a
marketed photon-to-photon figure must either add one or say that it stops at
presentation.

## The cohort

| Machine | Display | Mode | Notes |
|---|---|---|---|
| Lenovo 83ED (the supported laptop profile, `os/hardware/`) | internal panel | 60 Hz | the default reference |
| the same machine | one external 4K DisplayPort monitor | 144 Hz | high-refresh reference |

Set the mode before a run and measure one display at a time: the bench reads the
refresh the compositor paced to off the switches, names the KPIs by it, and
refuses a run that mixed two. Budgets for 60 Hz and 144 Hz are in
`tools/perf/kpi-budgets.toml`.

Every record carries the environment `profile/gpu/none/device-<name>`, the DMI
product and CPU (`hardware`), the deployed shell's commit, and in `meta` the
panel refresh (`panel_hz`) and the booted image (`image_digest`).

## Running the weekly lane

The device is shared: book it as a `tilo-vm` slot (AGENTS.md, "Dev VMs"), never
with raw ssh. Deploy the build under test, unlock the session, open one Notes
window and enable the greeter's presentation diagnostics, as
`scripts/perf/README.md` ("Physical device Notes latency") describes. Then:

```sh
TILO_VM=yoga tilo-vm vm status            # the booted image digest
python3 scripts/perf/device_latency_bench.py --device yoga --slot device \
  --commit <deployed shell commit> --image-digest sha256:<digest> \
  --out target/device-latency-$(date +%F)
```

It types 500 keys into a fresh scratch Notes page (never an existing note) and
times 100 workspace switches, both through `tilo-vm input`, the kernel input
path. A key the editor did not adopt, a frame without hardware presentation
feedback or a chord the compositor did not report fails the run; a partial run
never produces records. The records land in `<out>/records.jsonl`, compared
read-only with the store and the budgets (`<out>/summary.md`). Attach both to the
week's performance report; the KPI document's rows are filled from them.

To prove the bench sees the compositor, repeat a run on the dev image with
`TILO_TEST_FRAME_STALL_MS=50` (as `os/dev/test-slow-frame-launch.sh` does): the
latencies must rise by about 50 ms.
