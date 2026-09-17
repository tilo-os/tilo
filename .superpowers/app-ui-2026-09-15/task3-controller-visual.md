# Shared Task3 writing surface — controller visual evidence

## Before

Root individually inspected baseline Scratchpad wide1200×800 and narrow480×900 screenshots. The input is only38px high at y122–160, with most of the page empty between it and Save near the bottom. The main problem is that the visually indicated editing target does not occupy the writing workspace. Title/subtitle and Save/persistence hint are individually readable and not intrinsically crowded; reassess their balance after the editor becomes a real surface rather than removing copy preemptively.

## After

Candidate fba9ea50f: root captured and individually inspected all6standard variants in task3-writing-20260916/captures/scratchpad:1200×800,800×700,480×900,800×900@2,matched800×900@1,and800×480. Editor now occupies the writing region; placeholder no longer vertically clipped; Save/status/instruction remain visible. Root also individually inspected all4final long-bottom harness PNGs in task-3-captures-final (1000×700,480×600,640×480@2,480×600@2): explicit final marker visible and Save stays outside scroll content.

Framework and app interaction evidence in task-3-report.md covers lower-area typing, repeated click, exact Save/reopen persistence, single first wheel, default compact/manual dimensions and resize/content changes. Independent code review is pending, so findings are not closed yet.

Hierarchy assessment: after the writing surface expands, it is the dominant normal-size region. Title/tagline and Save/persistence reminder are readable secondary groups. Removing the reminder would remove useful information about manual saving; no further copy or heading changes are needed to address the original hierarchy imbalance. This is an evidence-based proposed closure ruling after the code review passes, not a claim that every sparse editor needs additional chrome.

Independent code review now PASSspec/Approvedquality. Controller visual gate completed as above; Task3 and its four marked Scratchpad findings accepted. Trailing-newline diagnostic remains a separate investigation.
