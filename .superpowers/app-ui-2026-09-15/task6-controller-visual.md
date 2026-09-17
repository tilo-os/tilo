# Shared Task 6 — controller visual inspection

2026-09-16. Inspected all six actual Notes PNGs individually: wide 1200×800, compact 800×700, narrow 480×900, large-text 800×900 at 2×, scale-baseline 800×900 at 1×, short 800×480.

Evidence: `task6-notes-20260916/capture-results.json` and `captures/notes/`. Binary SHA256 `6b5d2cbbb5ed9f4b582606b0aa25686edca09015941e44707f9e8aba7fd45c97`; manifest confirms identical after capture (`binary_stable=true`). Source HEAD and working diff hash recorded in manifest; source was a candidate, not yet committed.

Bold glyphs now appear once and are crisp. Normal hierarchy, bounded reading width, code block and checklist rhythm remain intact. Narrow layout retains readable body composition. Large text expands rows and switches to compact navigation; content continues below the viewport. Short capture keeps the main checklist and code block available with the status capsule at the lower edge. No new crowding or irregular spacing observed in these default-state captures.

Provisional visual pass only: actual caret, selection, chip, and overlapping link interaction evidence and independent source review are separate gates. Re-capture if subsequent production changes affect rendering. This does not close all-app review or assert every Notes route/state was exercised.

Controller also individually inspected ten actual Notes interaction PNGs under `task6-after-20260916/interactions-accepted/`: caret, selection, code-selection, bold-link and link-opened, each at 1× and 2×. Caret aligns at bold ending, selected bold and code remain visible, bold link is single-painted and underlined, destination shows Reading list selected with its actual body. These images support paint correctness; the harness assertions in worker report are the event/byte/true-edge evidence.

## Final rebuild correspondence

Final post-review Notes binary changed to `c1f09b8aede7e4a2021a9f0704818a13d0ae739dda8ba782d722bfd8eb655039`. Recaptured all six variants in `task6-notes-final-20260916/`; manifest records identical binary before/after (`binary_stable=true`). Compared every PNG SHA256 against the earlier individually inspected six: **all six byte-identical**. Thus the final images retain the exact inspected pixels; no new visual inference is needed. Worker is refreshing interaction evidence against the same final artifact graph.

Final interaction evidence is `task6-after-20260916/interactions-final-verified/`. Controller checked provenance hashes against all three owned current source files and all twelve final PNGs (ten inspected interactions plus two initial-state captures); all final PNGs are byte-identical to the earlier accepted set. Thus the ten individually inspected interaction pixels also correspond to the final linked artifact graph.
