# Preserving user applications during Flatpak acceptance

`flatpak-owned.py` exercises the real Store and account Flatpak install route through
its declared actions. It requires an explicitly selected, currently uninstalled app;
it never uninstalls or closes another application to manufacture a first-install
result. If the proposed app is already installed, choose another actual offered app
before running. It performs no automatic fallback or repeated installation.

After the matching Shell and app cohort have been activated, the owner operator can
run, for example (the app and executable must match the actual verified offer):

```sh
python3 scripts/vm-acceptance/flatpak-owned.py \
  --binary "$QUALIFIED_TILO_VM" --vm yoga --slot device --uid "$SLOT_UID" \
  --cohort-receipt "$ACTIVATED_COHORT_RECEIPT" --out "$NEW_OUTPUT_DIRECTORY" \
  --app org.gnome.TextEditor --query 'Text Editor' --executable gnome-text-editor
```

The runner uses the existing `OwnerVm` adapter and normal action/job door. It requires
an unlocked workspace, the exact activated cohort, one current Store worker with a
resident matching SDK, enabled official Flathub, no obsolete account opt-in action,
real browse/search results and readable details. It verifies installation using the
Store inspector and normal account Flatpak deployment readback. Errors and pending
jobs are never successful acceptance.

GTK Open remains a unit job. Its original invocation is retained by the existing
host launch observer, and only the actual adopted Wayland peer descended from the
registered launcher receives a surface-local custody witness. Owner-only
`state-dump` exposes `chrome.host_window_provenance`: original invocation, attempt
and surface lifetime UUIDs, current room/tab/surface, resolved desktop entry, and
kernel peer/launcher PID and start times. This is independent of client-announced
app IDs and titles, and confers no native package authority. Missing observations,
unknown or daemonized ancestry, exited/reused processes, moved/closed/replaced
surfaces and truncated inventories fail closed.

The runner matches its actual tracked job invocation to that current witness,
revalidates it before and after the bounded resident executable FD hash, and compares
that hash with the installed Flatpak deployment's declared executable component.
It makes **no Tilo native SDK claim for GTK**. Existing native `.tapp` installation
and exact newly opened native-worker SDK proof remain in `store-chooser.py`.

The output is content-free package/process metadata and authoritative receipts.
This runner does not capture screenshots, read application documents, send keys,
change identity, alter sandbox policy, stage packages or reboot. No hardware result
is implied by its unit or Wayland wire tests.
