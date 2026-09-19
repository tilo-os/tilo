# Task 4 probe — fix round 1 re-review

## Verdict

**Spec compliance: PASS for the scoped observation-completeness fixes.** Both original Important findings are **ADDRESSED**, including the explicitly authorized related reparse classification correction.

**Code quality: PASS for this fix diff.** No new Critical or Important breakage found in the two-file fix diff. This accepts the source correction, not native Windows runtime qualification or completion of Task 4 provisioning.

## Findings disposition

1. **Preserve incomplete directory observations — ADDRESSED.**
   - `crates/system/atomic-file/src/native/windows/directory_probe.rs:446` returns `io::Result<observation::Directory<File>>`. Component validation and parent-clone failures remain outer errors (`:454`, `:461`). The native-call event must succeed (`:522`) before a negative returned NTSTATUS can construct `Directory::Refused` (`:542`). Pending, unknown positive status, malformed IOSB and missing-handle cases remain outer errors (`:539`, `:545`, `:548`, `:551`). An event sink returning the same OS error code as access denial or collision therefore cannot become refusal evidence.
   - The actual ancestor caller uses `observation::ancestor(...)?` at `crates/system/atomic-file/src/native/windows/directory_probe.rs:807`. Its policy propagates outer errors and refusal-log errors at `crates/system/atomic-file/src/directory_probe.rs:208` and `:211`; only a completed refusal above the anchor yields `Ok(None)`. Root admission is required at native `:874`, before fixture creation at `:1015`. Incomplete ancestor observation cannot reach that mutation.
   - The actual collision caller uses `observation::collision(...)?` at native `:1068`, before stage creation at `:1073`. The policy propagates missing observations and sink errors at shared `:221` and `:225`, while retaining genuine expected collision and completed-negative behavior. The mandatory-open adapter at shared `:197` propagates either failure without weakening required admission.
   - The denied-fixture caller also matches only a typed completed native refusal at native `:930`; the outer `?` preserves unrelated errors. The final native mapper at `:116` uses shared `outcome` (`:256`), where every outer error yields `fully_observed=false` (`:265`). Terminal-event failure still overrides completeness at native `:117`.

2. **Reject unknown nonfailure flush statuses — ADDRESSED.**
   - Shared `observation::flush` at `crates/system/atomic-file/src/directory_probe.rs:239` rejects all positive statuses, including pending and unknown informational values, before they can become a complete negative (`:240`). Exact success still requires successful initialized IOSB evidence (`:248`). Negative native returns retain complete-negative handling (`:254`).
   - The actual native loop invokes this policy with `?` at `crates/system/atomic-file/src/native/windows/directory_probe.rs:694`, after recording raw status/completion. This propagates incomplete results immediately even when an earlier repeat already produced `success=false`; the bitwise assignment does not short-circuit policy evaluation. Existing pending IOSB retention remains before logging at `:677`.

## Authorized related correction and ownership

- Reparse snapshot acquisition now propagates identity, tag, owner/DACL-descriptor and event failures at `crates/system/atomic-file/src/native/windows/directory_probe.rs:366`. The actual reparse caller obtains that snapshot at `:910`, records its fixture evidence with `?` at `:919`, and accepts only an observed reparse attribute through the policy at `:922`. The explicit refusal event must also succeed (`:923`). No generic PermissionDenied catch remains. The existing reopen/identity reconciliation still gates completion (`:941`). Ordinary `inspect` keeps the same directory/reparse and private-security admission after the extracted snapshot (`:420`, `:427`).
- The typed result does not transfer uncertain handles. Successful handles remain locally owned through event recording; outer-error unwinding drops only the established `File` value. The pre-existing pending path retains the IOSB, output slot, name, attributes, descriptor and parent custody before any fallible event write (`:510`). No cleanup of uncertain filesystem fixtures or production authority change was introduced.
- The unchanged drive-root access branch was inspected only to check the remaining apparent catch-all refusal risk: it wraps `File::options().open()` directly (`:740`, `:772`); identity and event failures occur outside that open-error match and propagate. It is not another consumer of the newly typed native completion result.

## Evidence and limits

Read the original review, brief, final “Review fix round 1” report section, and supplied two-file fix diff. Inspected the actual native callers, snapshot extraction, pending-resource ownership, event sink, root-before-mutation ordering and terminal mapper. The host tests exercise the same private policies used by those callers (`crates/system/atomic-file/src/directory_probe.rs:274`, `:301`, `:323`, `:335`, `:351`), rather than disconnected copies. Their continuation markers establish host control flow, not execution of native mutations.

The implementation report records the initial focused failures, final five-test pass, and successful Windows test/example compile check. Those successful checks were **not rerun**. No broad tests, product edits, git derivation, staging or subagents were used; only this requested review report was written.

**Native Windows execution remains absent and separately required.** Ordinary-user x64/ARM64 native access/options, export/completion behavior, all candidates, privacy, root mismatch and reopened-process replay remain unqualified. Abrupt storage-loss behavior, cache assumptions and production anchor establishment remain separate controller-owned requirements. This fix review does not certify any of them.
