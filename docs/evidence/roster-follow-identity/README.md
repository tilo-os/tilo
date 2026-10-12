# People Follow identity

The People popover must activate the person painted in a Follow button. Presence
updates can reorder or remove that person before input is handled, and a repaint
can move their button while it has keyboard focus.

This increment resolves the painted participant ID against the current roster
instead of looking up the same numeric row in that roster. A surviving person
keeps keyboard focus at their new position; an absent, unavailable or hidden
person loses focus rather than handing it to a replacement row. Follow uses the
shared ButtonFace focus state, with the existing panel and control styling.

The source-only capture example calls the production People draw method. Its
fictional Ada/Bob roster is confined to the fixture. Comparisons cover a selected
person moving and then departing, in light, dark with 125% text, and high contrast.
PNG artifacts live in the ready PR's attachments rather than Git. The qualification
receipt records source revisions, image hashes and focused test results.

Native regression fixtures reproduce a click following the wrong person after
a live reorder and a stale keyboard activation following the replacement after
departure. Desktop regressions exercise identity focus, replacement, unavailable
targets, accessibility and explicit navigation. The native repeated-action
journey uses actual keyboard/mouse events, Escape, close/reopen and Follow/Stop.

The follow transport, authority checks, permissions, status-sharing settings and
invite/join behavior are unchanged. PR3300 changes downstream follow focus
installation, not this roster lookup. The shared overlay_host file also appears
in Messages, Dictation and display work; this change does not edit their routes.

Existing Follow-label elision at 125% text remains visible in the inspected
comparisons. Installed-device acceptance and broader People text/viewport/status
controls remain separate inventory work.

Verification: all seven desktop and five native cases pass. The five original
regressions failed at their intended assertions on the unchanged production
base. Pixel comparisons confine every changed pixel in all six before/after
pairs to Follow control bounds; the surrounding panel and typography are
unchanged. Strict all-target desktop/shell Clippy and shell tests compilation,
staged precommit and commit-policy checks are required before the ready PR.
