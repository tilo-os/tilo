# Rulings I made

- Ruling: Implement the already-described all-app automatic-start/join design without another permission gate — user explicitly reiterated implementation and rejected discussion-only responses — if interpreted wrongly, local changes remain reviewable and reversible.

- Ruling: Keep actual Cargo package names and implement at shared live startup, including custom and definition paths — user explicitly said all packages — wrong coverage would strand an app; require shared path tests and independent native launches.

- Ruling: Reexec through the native host and reuse inherited descriptors rather than mutate UI environment or create another broker crate — avoids unsafe environment races and shell/ui dependency cycle — costs an extra exec at first unmanaged startup.

- Ruling: Auto-build the existing host only when absent in a developer checkout; installed layouts find their shared executable — plain cargo run must not require a hidden manual build step — first launch may incur a host build and failures must be visible/bounded.

- Ruling: Preserve intentional private/local app modes via a generic native attachment predicate defaulting to shared, plus Browser-owned live-local construction — unconditional attachment would persist private browser page titles in broker effect receipts — costs a small explicit policy seam; normal apps remain automatic, and private regression evidence is required before delivery. Browser owner implements App::from_scoped_parts_local and Browser predicate/tests; our worker owns LaunchSpec::runtime_attachment predicate only.

- Ruling: Correct the plan's ambiguous no-download restriction to allow ordinary Cargo dependency fetching/settings during automatic source builds — forcing --offline can make a fresh app-only checkout fail because shell dependencies were never cached, contrary to automatic startup — cost is normal Cargo network/build work; no opaque runtime binary download or extra installer is added. Task1 hardcoded --offline is a plan-mandated defect for the fix loop.

- Ruling: Add shared automatic-transfer process provenance and honor it in startup and SDK descriptor discovery — an ordinary spawned child must not treat its parent's now-CLOEXEC fd numbers as its own authority — costs a small ownership marker and descendant regressions; true current/legacy inherited transfers must remain authoritative and malformed ones fail closed. Shared fresh descriptor preparation must clear ancestor provenance, while automatic exec stamps its own PID and selected profile.

- Ruling: Final review covers every change owned by this automatic-runtime feature plus its relevant Browser privacy/packaging integration, excluding unrelated concurrent work on this long-lived branch — a whole branch diff would mix other users tasks and obscure this acceptance decision — cost is that unrelated branch changes retain their separate review responsibility.

- Ruling: Recover accidental workspace formatting only from verified pre-format bytes or owner-reviewed format-equivalent reconstruction, with saved current backups and guarded writes — preserve every concurrent semantic change — cost is recovery time and explicit limits for any remaining formatting differences.
