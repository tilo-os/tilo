# Validation v5 failed

All four lint gates pass: production/vendor checks are warning-clean, and the test-inclusive gate reports zero relevant diagnostics. The default vendor runtime suite finishes with 112 passed, 1 failed and 2 ignored. The failure is the new shared-input clip proof: wgpu rejects binding 4 (clip_els), whose fixture supplies 4 bytes while the ordinary shader requires a minimum 32. No numerical output comparison passed in that test; do not call this a pixel mismatch or equivalence proof.

The new route-boundary test passes full/sparse small-target checks, but logs three bump-buffer growth/recovery events before successful settlement. Its printed overflow=false describes only the final state, not absence of earlier overflows; root requested an explicit settled-state label. The candidate-only oversized-257 sentinel test passes. The native clip proof remains ignored in this suite and has not been explicitly run yet. Existing configuration-guard successes are not counted as executed GPU coverage.

All commands are terminal. Terra was asked to repair only fixture buffer minimum sizes and the ambiguous positive log wording. No release build or performance attempt has run. v1-v5 failure evidence remains retained.
