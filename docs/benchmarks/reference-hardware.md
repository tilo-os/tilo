# Reference hardware for the latency KPIs

Tilo's performance KPIs, what it markets and how each is measured, set two
latency targets:

- **Input to photon:** p50 at most one refresh plus 8 ms, p99 at most two
  refreshes plus 8 ms.
- **Workspace switch:** first frame within one refresh, no dropped frame,
  at most 300 ms in total.

One job measures them: a weekly, report-only run on a reference machine below,
through `tilo-vm`. It never blocks a merge.

| Job | Where | Clock | KPI ids |
|---|---|---|---|
| Weekly, report-only | a reference machine below, through `tilo-vm` | the flip's vblank | `hw.workspace_switch.{first_frame,settled,dropped_frames}.<hz>hz` |

The clock is the true number up to the presentation timestamp. It does not see
peripheral scan time (USB polling) or panel pixel response: only a photodiode
does, and a marketed photon-to-photon figure must either add one or say that it
stops at presentation.

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

## Running the weekly job

The device is shared: book it as a `tilo-vm` slot (AGENTS.md, "Dev VMs"), never
with raw ssh. Deploy the build under test and unlock the session. Then:

```sh
TILO_VM=yoga tilo device vm status            # the booted image digest
python3 scripts/perf/device_latency_bench.py --device yoga --slot device \
  --commit <deployed shell commit> --image-digest sha256:<digest> \
  --out target/device-latency-$(date +%F)
```

It times 100 workspace switches through `tilo device input`, the kernel input
path. A frame without hardware presentation feedback or a chord the compositor
did not report fails the run; a partial run never produces records. The records land in `<out>/records.jsonl`, compared
read-only with the store and the budgets (`<out>/summary.md`). Attach both to the
week's performance report; the KPI figures are filled from them.
