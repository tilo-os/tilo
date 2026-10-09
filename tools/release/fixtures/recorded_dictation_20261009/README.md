# Recorded Whisper Base compositor fixture snapshots

These are the exact successful 9 October qualification drivers, retained for
review and reproduction of `docs/architecture/dictation-recorded-compositor-evidence.json`.
Their hashes are recorded there. `recorded-inner.py` runs only inside the private
namespace projection prepared by `recorded-outer.py`; `run-qualified.py` is the
rootless builder driver with independently checked limits and an owned startup gate.

They intentionally retain this task's exact builder, artifact, result and source
identities. Reusing a result name fails. They are historical fixture snapshots,
not a command to install or activate dictation. For another execution, rebuild the
current binaries through rcargo, verify the pinned public inputs and official RPM
signature, use fresh result/CID/gate names and record the actual source head and
hashes. Preserve the sandbox and all bounds from the qualification plan.

No physical microphone, host session/home/device mount, release publication,
persistent trust/catalogue/model-store change or deployment is part of this fixture.
