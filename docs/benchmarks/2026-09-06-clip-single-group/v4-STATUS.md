# Validation v4 failed

Format/diff and strict production Clippy pass. The vendor-tests command exits zero but emits nine new unused-qualification warnings, so it is not counted as warning-clean. The test-inclusive changed-line/compiler check fails with those nine warnings plus two E0433 errors from a missing ClipSingleGroupAttemptAccounting test import in ui/offscreen.rs (11 relevant diagnostics). Both raw logs are retained.

After every command terminated, Terra was asked to repair only the test import and redundant qualifications. No lint policy, production behavior, GPU test, release build, or timing changed. v1-v4 failures remain retained.
