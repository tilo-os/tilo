# People completed activation

People controls now use the shared PaintedTargets interaction lifecycle. A
pointer press shows the shared ButtonFace pressed state and applies its action
on a matching release. Reordering, departure, resize, focus loss and dismissal
cancel the armed gesture; reopening or a returning participant cannot revive it.
Follow keeps the painted participant identity established by the preceding
People identity increment.

Enter and Space activate once. Their repeat and release edges remain owned by
People after an action closes it, so holding Enter cannot resume Follow after a
local navigation key has handed control back. Escape cancels before dismissing.
Status text editing keeps its existing keyboard route.

Seven native regression journeys fail at their intended assertions on unchanged
production base ed9a5e938cc3ab29b5baae86a37f6ecf11190e6d. They exercise real
WindowEvent input through ShellOverlayHost rather than a surrogate helper.
Existing Follow/Stop, Activity and Invite/Join assertions are retained at the
completed-click boundary, with additional inert-press assertions. Invite and
Join tests only assert queued prompt requests; no network operation is run.

The screenshot fixture draws the actual native host model after input. Four
original and six fixed screenshots cover pressed, reordered and focus-lost states
in light and dark with 125% text. Positive containment assertions require the
whole People panel to be observed. Earlier truncated captures were rejected and
are excluded from the public qualification. Six unaffected identity screenshots
from the preceding increment are reused only after byte-for-byte comparison.

PNG artifacts and a revision/hash/check receipt are attached to the ready PR.
The source tree contains this manifest and fixture code, not generated images.
Follow-label elision at 125% text remains visible; broader People text scaling,
status control adoption, viewport/paging and installed-device acceptance remain
separate inventory work. Transport, capability admission and security policy
are untouched. Shared overlay_host edits avoid PR3300's downstream Follow focus
installation, Files3009 transfer/undo and Messages/Dictation/display routes.
