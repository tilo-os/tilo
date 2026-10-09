# People status editor lifecycle

Closing People previously left its inline status editor active. A later key or
IME commit still entered that hidden draft; reopening restored a field the user
had dismissed. The draft could also outlive the transition to Workspace activity.

The native host now ends that draft when a completed People action closes the
surface, when a subsequent input observes an external close, or when hidden
People is painted. The existing shared EditCore-backed text-entry implementation
and status publication behavior remain intact. Ending a draft clears only its
editing flag, buffer and selection; it neither publishes nor changes the current
status. Enter explicitly submits; Escape leaves editing, then dismisses People
on a second press.

Four new journeys fail at their intended assertions on unchanged production
base b0d2ab50db1ac28aad58400417eed1cc8babf773. Six new native journeys exercise
actual keyboard/pointer events, completed outside clicks, external closure
before a keyboard or IME event, closure at paint, reopen, Activity, selection,
Unicode IME replacement, Home/End, repeated submit and Escape. Existing status,
keyboard-modal, completed-click, identity and prompt assertions remain unchanged.

Four representative actual post-input screenshots compare dismissed/reopened
People in light and dark with 125% text. They call the existing production
native draw method and require full-panel containment. Generated PNGs and the
revision/hash/check receipt live in ready PR attachments. Existing #3735/#3727
screenshots retain their original provenance; the portable People component is
unchanged, so their broad render suites are not repeated.

This increment owns only the native status edit lifecycle. Shared overlay_host
ownership was checked against Follow3300, Dictation2974/3374 and Displays3715;
their downstream routes are untouched. Files3009 transfer/broker/undo, devices,
grants, credentials and permission policy are outside this change. Installed
device acceptance, status field/control rendering and broader viewport/paging
remain inventory work. Normal CI, protected landing and exact merged-main
postchecks remain integration gates.
