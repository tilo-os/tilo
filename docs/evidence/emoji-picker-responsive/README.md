# Emoji picker measured footer and group targets

At production main c00597488, 200% text makes the picker footer's name and
shortcut hint overlap. At 320 px the nine group glyphs are elided into dots.
Both focused production regressions fail with production sources unchanged.

Footer shaping uses the toolkit's Caption style, grapheme-safe elision and
shared line clamping. The measured height places the hint below the name when
both cannot fit side by side. The glyph keeps its original control size.

Group geometry uses shared SegmentedFace measurements. Existing wide rows stay
intact; narrow hosts wrap the same ToggleGroup faces into balanced rows with
full shared target widths. Keyboard order, pointer mapping and localized
accessible names retain their original group identities. The grid gives room
to the measured footer and keeps its selected emoji visible after resize.

Focused tests cover measured text/target room, wrapped keyboard and pointer
selection, accessible bounds, resizing to 320 x 480, Escape/cancel, existing
search/picks and skin tones. Public captures show the failing 800/320 px 200%
text layouts, last-group selection, and normal light comparison. Three before
hint images reuse the inspected #3719 evidence; its painter matches the current
base exactly. This increment does not repeat the full earlier screenshot
matrix. Normal comparisons and the unchanged wide selected-group capture are
byte-identical to baseline.

These are source-only production captures and synthetic input; no installed
VM/device acceptance is claimed. Other inventory surfaces and extremely small
viewports remain separate follow-ups. Existing PR heads, launch diagnostics,
display routing, Files transfer, grants, credentials and trust are unchanged.
