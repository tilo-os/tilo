# Ctrl+K presentation evidence

This measures actual palette opening/query/hover and Settings activation. It is
not a hardware performance result until captured on an owned, source-matched
cohort. Use the same fresh Settings binding, selected executable/SDK, startup
observation flags, authenticated worker and preserved-tab guards as
`device_settings_latency.py`. Never borrow an existing user window, unlock,
refresh, reboot, screenshot or infer ownership from the first log entry.

Enable the existing session presentation/GPU diagnostics through its public
actions before launching the fresh Settings worker. GPU startup capability must
already be present; missing capability fails. Revalidate the owned slot,
selected source/SDK, worker and original tabs before and after each operation.

The ordinary kernel-input commands are:

```sh
"$TILO_VM" --vm "$VM" --slot "$SLOT" input key ctrl+k
"$TILO_VM" --vm "$VM" --slot "$SLOT" input type "$QUERY_PREFIX"
"$TILO_VM" --vm "$VM" --slot "$SLOT" input key "$FINAL_QUERY_KEY"
"$TILO_VM" --vm "$VM" --slot "$SLOT" control state-dump --pretty
"$TILO_VM" --vm "$VM" --slot "$SLOT" input move "$ROW_X" "$ROW_Y"
"$TILO_VM" --vm "$VM" --slot "$SLOT" input click left --at "$ROW_X" "$ROW_Y"
```

Read `chrome.palette.observer` to pin the actual compositor namespace PID,
random observer UUID and preceding input boundary. Read
`chrome.palette.inspection` for mounted/current query hash, results generation,
publication hash and exact row bounds/provider/action IDs. Use the center of the
observed row, not a label or guessed ordinal. Public app-action search IDs contain
U+001F-delimited peer/revision/resource/action components; preserve the exact
observed ID. The Settings target must identify the fresh authenticated worker's
resource and `open-pane` action. Before hover/click require the same actual drawn
query/results/target; live state alone is insufficient.

Collect and retain each opening, final query key, hover and click separately,
before sending the next input. Prefix entry prepares the query outside its
measurement; the final physical key must change it to the expected query hash.
Use the maintained `device_notes_trace.collect_palette_trace` with the
pinned `process_id`, `observer_session`, previous `after_input_id`, and exact
Settings worker namespace PID/observer UUID. Its Device.execute is the existing
canonical `tilo-vm slot exec` transport, not SSH or an alternate control door.
Restore diagnostics and preserve every tab after collection.

Analyze each bounded private capture independently:

```sh
python3 scripts/perf/palette_latency.py --log "$CAPTURE" --out "$NEW_REPORT" \
  --mode "$MODE" --process-id "$SHELL_NAMESPACE_PID" \
  --observer-session "$PALETTE_OBSERVER_UUID" \
  --expected-query-hash "$OBSERVED_QUERY_HASH" \
  --target-provider "$OBSERVED_PROVIDER" --target-action "$OBSERVED_SEARCH_ID"
# For --mode click also pass:
# --target-pane "$PANE" --settings-process-id "$SETTINGS_NAMESPACE_PID" \
# --settings-observer-session "$SETTINGS_OBSERVER_UUID"
```

The input witness retains the original kernel timestamp. The frame witness
retains actual completed widget draw state, positive production scene revision
and exact presentation-pipe serial. Independent compositor retirement and GPU
submission records supply physical presentation and device-duration evidence.
Click must retain the same input/observer/time and exact selected public action
through ordinary invocation, Settings adoption/drawing and the same KMS outcome.
Broker acceptance or mounted state is never presented latency.

Missing, stale, coalesced, duplicate, wrong-worker, wrong-action, revision-zero,
discarded or misaligned evidence fails. The current strict analyzer also refuses
multiple palette frames for one query, including an early stale frame; it does
not collapse intermediate observations into a favorable sample. An unattended
all-phase capture frontend remains a separate acceptance increment; these
commands and collector do not grant VM/device custody or claim a capture ran.

Reports separate elapsed dispatch/draw/queue CPU regions from actual GPU device
durations, including p50/p95/p99 and coverage. GPU completion waits are CPU waits.
The 120 Hz goal is an 8.33 ms render budget; it does not invent a display mode.
Verify actual EDID/active mode separately: Yoga Slim 7x 14Q8X9 specifications list
2944×1840 OLED at maximum 90 Hz. VKMS acceptance cannot certify Yoga GPU throughput
or panel cadence.
