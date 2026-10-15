# Calculator

Calculator mode and both graph editors use the same structured mathfield:
fractions, powers and radicals are two-dimensional, with structural caret
navigation, selection, clipboard input and bounded scrolling. The calculation
tape uses that layout too. Expressions retain the arithmetic/scientific syntax
of the evaluator; this is not a symbolic algebra or integral solver.

The keypad first drops the previous-answer status line when height is tight,
before moving the display beside the keys. Display and memory limits follow the
active theme rather than unscaled constants. Graph traces snap to nearby visible
curve crossings, excluding poles. Surface samples survive camera movement, while
the projection and depth ordering update during an orbit.

## Review captures

From the repository root:

```sh
cargo run --locked -p calculator --example calculator_capture -- target/calculator-review --video
```

The example links the production font catalog beside its binary before creating
any app or renderer. It captures all three modes in dark and light themes at
1440×810 with `Theme::with_ui_scale(1.5)`, plus narrow layouts. The optional video
requires `gst-launch-1.0` with `rawvideoparse`, `openh264enc`, `h264parse` and
`mp4mux`. It records 240 actual app frames, including pointer-driven mode changes,
a curve trace and a surface orbit, using curved minimum-jerk cursor trajectories.
It does not capture the desktop or seed a live profile.

```sh
cargo test --locked -p calculator
cargo clippy --locked -p calculator --all-targets
```

When running test binaries directly, link `crates/sdk/ui-core/assets/fonts` as
`fonts` in their directory as well, so geometry is measured with Tilo's fonts.
