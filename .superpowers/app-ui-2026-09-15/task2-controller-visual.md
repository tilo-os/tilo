# Shared editor Task2 controller visual evidence

## Before

Root individually inspected baseline Notes/Calendar `captures/{notes,calendar}/large-text.png`: Notes text vastly exceeds row widths and an inline link crosses underlying text; Calendar Agenda and body oversized/cropped, notes label also close to separator. Standard routes already honored scale for these two apps.

Root inspected corrected Typst `capture-corrected/captures/typst_editor/large-text.png` (use this, NOT older `captures/typst_editor/large-text.png`, whose handler ignored scale). Corrected source toolbar overflows and gutter numbers crowd body rows. Toolbar remains AppComposition6, shared gutter remains Shared2.

## After

Built Notes, Calendar and Typst after 3f13fb4eb; all 15 captures in task2-after-20260916/captures were individually inspected by root. Capture manifests record binary and source provenance. No closure until Task2 review fixes pass.

- Notes: normal wide/compact/narrow composition preserved. At2× paragraph/checklist sizing is now coherent; content below viewport still requires scrolling verification. Bold word in first paragraph is visibly doubled/misaligned at1× and2×. Source RichTextLine draws plain input then styled glyphs over it; separate diagnosis queued and finding marked.
- Calendar: normal bounded form and narrow full-pane hierarchy preserved. At2× Agenda/body now fit their expected scale, but metadata chips remain shallow and NOTES label crosses the separator. New app geometry task required; do not close all Calendar on shared editor fix.
- Typst: normal and narrow source remain readable; at2× gutter width/number rows now follow wrapped text correctly. Toolbar labels and status capsules still overlap or exceed their shallow surfaces; these remain Composition6. Preview is a document page, not accessibility-scaled app text.

Normal-size baseline check: root also viewed Notes and Calendar wide PNGs individually. Notes has a useful compact outline and writing area; Calendar event form has clear field/group separation and a readable bounded width. Do not redesign these normal compositions merely because their 200% editor text fails. The fix should preserve their normal hierarchy and recover readable scaled editor content.

## Accepted fix round1

After83c269dac, rebuilt Notes/Calendar/Typst (24.81sec) and captured9fresh wide/narrow/large-text images in task2-fix1-20260916. Root individually inspected all9: ordinary hierarchy preserved, scaled editor/gutter geometry remains coherent. No additional defect from lifecycle fixes observed. Independent fixreview approves all findings. Notes styled text, Calendar chips/section rule and Typst toolbar/status remain open in their explicitly assigned tasks; those flows still need final interaction verification.
