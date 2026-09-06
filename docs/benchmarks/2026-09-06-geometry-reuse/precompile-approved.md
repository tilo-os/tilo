Root reviewed the corrected accessor, span extraction, telemetry update/eviction,
post-timer integration, and the five vendor / ten shell pure tests. Both reporting
bugs from review 1 are fixed, and the missing coverage from review 2 is written.
Only the three allowed source files differ from the pre-candidate manifest;
rendering/shader/budget/recording/config sources remain unchanged.

Source is frozen for version v1 validation. This authorizes the nine declared
checks, not a release/timing run or performance claim. Source owners will not
edit during Cargo/runtime. No cache output reuse has been implemented.
