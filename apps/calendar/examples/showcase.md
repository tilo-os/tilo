# Calendar at desktop accessibility scale

The capture uses the real `CalendarShell` resource fixture, native widgets,
rich-text notes and action dispatcher. It is not a mockup or an enlarged bitmap.
The sample has a pinned Tuesday morning and synthetic calendar accounts; it
is not evidence of a live CalDAV session.

## Reproduce

Make the bundled font payload available at `crates/sdk/ui-core/assets/fonts`
(link missing font files from a populated checkout). The example also links
that catalog beside the executable before constructing any text renderer.
Run from the repository root, with an absolute output path:

```sh
out="$PWD/target/calendar-captures"
cargo run --locked -p calendar --example showcase -- "$out" video
ffmpeg -y -framerate 60 -i "$out/frames/%03d.png" \
  -c:v libx264 -preset fast -crf 18 -pix_fmt yuv420p \
  -movflags +faststart "$out/demo.mp4"
```

Use the normal Cargo entry point on remote-build workstations. Encoding is a
separate step so the builder does not need ffmpeg. Omitting `video` renders
only the review stills. The example generates no network traffic.

- `showcase.png`: 1440×810, `Theme::dark().with_ui_scale(1.5)`, agenda with
  real event rows, task checkboxes, calendar visibility and mini-month. The
  mini-month leads the unscrolled sidebar and Design review is selected,
  showing its right-hand Event details and Attached notes inspector.
- `week.png`, `month.png`, `agenda.png`, `tasks.png`, `event_editor.png`,
  `task_detail.png`: the principal views at the same native size and scale.
- `light.png`: light-theme week view; `narrow.png`: 720×900 scaled editor.
- `frames/`: 210 independently rendered frames for 60fps, 3.5 seconds.
  A high-contrast arrow follows quadratic curves parameterized by `min_jerk`.
  Design review and its rich attached notes stay open throughout. Real
  pointer events complete Prepare quarterly report without dismissing the
  inspector. The capture fails unless the event remains selected and the
  task is durably marked done. `gesture.txt`
  records those assertions. No frame interpolation is used.

## Review findings and fixes

The original scale change preserved a pixel scroll offset rather than the
visible wall time, opening the week around 04:00 instead of the workday.
The timeline now retains wall time across scale and column-density changes.
An event starting above the viewport keeps its title visible on the remaining
block. Short blocks and month chips elide intentionally within their bounds.

The navigation body formerly clipped later accounts and conditionally omitted
the mini-month. It now scrolls below fixed chrome, keeps every section and the
month in its flow, and clips pointer hits to the viewport. Assistive technology
retains offscreen controls and can bring them into view. Overlay thumbs remain
quiet at 6px instead of growing into broad rails.

One unified window header hosts period navigation, all five view buttons,
platform search, New event and the shared History opener. It spans the window
rather than stopping at the sidebar edge. Only genuinely narrow windows move
the switcher into the drawer. The mini-month leads the sidebar, followed by
Calendars; task lists and projects remain available in Tasks view. Overflow
still scrolls on short windows or with many calendars.

Selecting an existing event on desktop docks a compact inspector to the right,
with its attached rich notes immediately visible. The agenda and task checkboxes
remain live beside it. Edit event opens every field of the full editor; narrow
windows and new drafts retain that modal editor.

Hand-positioned inputs and attached notes adopt scaled text sizes, including
empty-field prompts. Wide editors place notes beside the form; narrow editors
retain the stacked flow. A scrolling form ends between complete rows, and its
modal semantics exclude the background. Notes actions route to their editor.

Verification: `cargo test --locked -p calendar`,
`cargo clippy --locked -p calendar`, and all-target Clippy. Stills and decoded
video frames at 0, 1, 2 and 3 seconds were opened for visual inspection.
