# Joined-device Notes: startup and public editing

The installed Notes app creates, edits, undoes, redoes and saves a note on a
public-only joined test identity. Reopening the packaged process recovers the
exact saved text. The earlier failure did not demonstrate a schema, focus or
permission defect: its interaction deadline included cold rendering startup.
The original gate failure remains a failure; no gate assertion or deadline was
changed to obtain the separate journey below.

## Artifact and environment

- Source: `12f4dfb2d535f0ea9c64278b66a9bef435d2e4b3`.
- Installed image: `registry.tilo.so/tilo/tilo-vm@sha256:ec4aff79035255d6aec7f9cf3c3e7555f26a8a1890b3d24c8bc95310708c228e`.
- QEMU 10.2.2, TCG, four virtual CPUs, 4 GiB, virtio 2D at 1280×800;
  llvmpipe. This unsigned development image enables developer diagnostics.
- One sample per journey, fresh disposable COW disks. No hardware performance,
  optimized production-image qualification or speedup comparison is claimed.
- The existing joined-device script enrolled a public-only member identity,
  removed its temporary root signing key, verified login and checked the
  externally readable established compositor surface. No authorization,
  sandbox, enrollment or shell-preference policy was changed.

## What failed

The unchanged full gate failed because `New note` exposed no editable
`Block text` within its existing 20-second deadline. A bounded diagnostic then
activated the same AT-SPI control once, retaining that deadline and recording
the failure before doing any later observation:

| Observation | Result |
| --- | --- |
| AT-SPI accepted activation | 158.8 ms |
| Editable field by 20 seconds | No |
| Frame sequence and buffer at the deadline | `0`, no buffer |
| Public editor inspection | No selected/bound page; zero pending page events |
| Same activation eventually exposes the editor | 34,500.2 ms |
| AT-SPI edits the resulting field | Exact `Joined Notes baseline` reads back |

There was no second activation or mutation retry. Read-only observation retried
replaced AT-SPI objects. `presented=true` alone was insufficient: the frame
observer reported that flag while the frame sequence was zero and no buffer
existed. Accessible control bounds were also nonzero before a submitted frame.
Neither observation establishes a finished native startup.

## Separate public journey

The corrected measurement fixture treats startup and interaction as separate
phases. It first requires a nonzero frame sequence, a real buffer matching the
window rectangle, `presented=true`, and no frame error within 90 seconds. It
then starts the unchanged 20-second creation deadline and activates `New note`
through AT-SPI. A missed phase deadline fails the fixture.

| Check | Result |
| --- | --- |
| Remaining cold frame wait after AT-SPI discovery/extents queries | 30,603.1 ms |
| Ready window's activation to editable field | 4,313.0 ms |
| AT-SPI `EditableText.SetTextContents` | Exact test text reads back |
| Save feedback after edit | `Saved` |
| Published Undo | Succeeded; receipt identifies `Splice text`; body becomes empty |
| Published Redo | Succeeded; the same change returns and exact text reads back |
| Save feedback after redo | `Saved` |
| Reopen packaged Notes, require a real frame, read its text | Exact saved test text; `Saved` |

The 30.6 seconds measures **remaining startup wait**, not time from a launch
request. It follows app discovery and control queries. The 4.3 seconds starts
at the single activation after actual frame readiness. Neither value is a
laptop result or evidence that a production optimization improved latency.

The existing component accessibility creation regression passes. The added
native regression mounts the actual Notes definition in `App`, uses its
AccessKit dispatch and production `surface_frame`, and checks both reachable
controls at the initial wide size plus the narrow tiled size. Each notebook
starts empty; activation creates exactly one page and exports the body's
`SetValue` capability. No app or SDK production implementation was changed.

## Reproduction and retained failures

Use `os/dev/test-joined-device-startup.sh` against a disposable installed image
for the unchanged full gate. The diagnostic uses that script's joined-device
setup, the installed external AT-SPI client and the existing control socket's
`foreign_frames`, `actions_enumerate`, `actions_invoke` and `actions_status`.
Action IDs are opaque: locate Notes actions by their application and operation
metadata, re-enumerating before Undo and Redo. Do not click a second time merely
because an asynchronous activation has not finished.

`notes-joined-runtime-2026-10-06.json` retains the relevant frame observations,
editor inspection, edit/history receipts, saved-state reopening and raw-file
hashes. Full raw diagnostics remain at the recorded local paths. Probe 01
failed on a replaced AT-SPI object; probe 02 corrected read observation.
Probe 03's launcher parser initially failed because the save-status helper
printed three success lines before the JSON. Its unmodified raw output was
retained and decoded directly; every journey assertion passed. These observer
errors are not counted as runtime successes or silently discarded.

All guests stopped, COWs were deleted, and the additional 10,026,745,856-byte
installed test base was removed. No physical device was rebooted, no package
was deployed and no release was dispatched. Release 69901's older compositor
segmentation fault remains a separate unqualified production-image issue;
this development-image journey does not resolve it.
