# DB Client Task 12 controller visual gate

Root individually inspected original wide 1200×800, compact 800×700 and narrow 480×900 captures before implementation. The four-line query occupies about 70–90px of text, but the editor reserves about 260–345px; the result strip begins around y350/392/430. This confirms the marked excess blank editor space. Narrow wrapped first line demonstrates why automatic size must use rendered line metrics rather than raw newline count alone. Wide connection/schema rail is readable; compact/narrow retain a visible drawer trigger.

Task 12 default/manual split and interaction evidence are pending. Task 14 scale/grid metrics remain separately open. No finding closed by this baseline reinspection.

## Rejected first candidate — 2026-09-16

All six default captures in task12-db-final-20260916 were individually viewed. Despite the directory name, this is **rejected candidate evidence, not final acceptance**. Stable normal binary `3e2b5daa96fb62e1f55a451cebb19b74bcb4c992cc3cca87ecb17919709c42dd`; source app.rs hash reported `9c8706488e6b03866ed45914479c4b129451ece82f32ae1229f8ae632a460d72`.

Wide/compact/baseline/short editor is about66px, clipping the fourth four-line demo query line; narrow wraps the first line and clips from third onward. True2× editor shows only part of the first wrapped query. Divider/result strip stays aroundy150, leaving large empty grid area. This over-compresses the editor, violating content-based sizing and allowance. Existing grid/chrome2× collisions remain Task14, separately.

Source trace: automatic relayout reads TextInput::content_height before shaping, whose fallback is cached self.lines (documented one row before layout), not raw SQL newlines. Current root draw does not refine split layout after editor measurement. Sent bounded blocker/root-cause evidence to sole worker before commit; require actual first-frame and settled host geometry, not only helper upper-bound tests. No closure. All6capture statuses/dimensions and stable binary guard verified.

## Settled candidate default visual check — 2026-09-16

All6 task12-db-settled-20260916 defaults individually inspected. Stable binary `b6034bc385b14fe7d7bb3a8ee63ce94b7c463961568ad6832eb14087f0d94172`, current app.rs hash `4d78b83def6b8753f3d308da596b2cce17c12477d62a127fe2bdda736ae66b40`; all PNG hashes/dimensions/statuses verified. Default four-line query fully visible with two-line editing allowance. Narrow automatic height accommodates first-line wrap. Normal grid starts abouty231 (narrow248), substantially earlier than original baseline, without clipped SQL. Short480px tall shows Run/query/all8sample rows. True2× editor accommodates wrapped query; existing result-strip, grid/header and toolbar scale collisions explicitly remain Task14. Split composition visually passes this candidate; no task closure until named routes and independent review.

Final named Querywide/Querynarrow/Datawide/Structurewide individually inspected; all8manifest hashes (binary/source/logs/4PNGs)verified. Data/Structure populated shapes preserved; manual/drawer behavior evidenced in tests, not named PNGs. Independent review active on e58c94cf3.

## Fix1 final comparison — 2026-09-16

Six fresh standard captures in task12-db-fix1-final-20260916, stable binary `5b0521ca4b866acb285e0b5d54adc19b6323a8df43fb26eabebaa92a231c8bcb`, current source `863bb54b9445d5fbfe2632e8e652a902807971109fbed842508d8e8a7a07cdbb`. All6statuses/dimensions/PNGhashes verified. Every PNG is byte-identical to the individually inspected settled candidate, so those pixel judgments carry exactly; no redundant visual run claimed. The new Driver menu-open-short800×480 PNG was individually inspected:3boundedlegiblerows, Decreasehighlighted, draweropenfromresize andqueryscrim visible. It is an interactionstate, notdefaultshortview. Fix behavioral evidence83lib/check/build passes; independent scoped re-review pending.
