# Maps visual captures

The `showcase` example renders the real hosted app at 1440×810 with
`Theme::dark().with_ui_scale(1.5)`. It freezes the clock and uses the bundled
schematic Munich fixture; it does not contact mapping providers or read user data.
The fixture includes varied courtyard footprints, secondary streets, the Isar,
Theresienwiese and Alter Südfriedhof. It is illustrative cartography, not surveyed
OpenStreetMap data or evidence of live-provider coverage.

```sh
mkdir -p target/debug/examples
ln -s "$(pwd)/crates/sdk/ui-core/assets/fonts" target/debug/examples/fonts
cargo run --locked -p maps --example showcase -- "$PWD/target/maps-showcase.png" transit overview
cargo run --locked -p maps --example showcase -- "$PWD/target/maps-frames" demo
ffmpeg -framerate 60 -i target/maps-frames/%03d.png -c:v libx264 -crf 18 \
  -pix_fmt yuv420p -movflags +faststart target/maps-demo.mp4
```

Other still scenarios include `directions`, `pois`, `search`, `explore`, and
`navigation`. Add `overview` after the scenario to zoom out once using the map's
own control and pan east to include the river. The demo uses this same overview
framing after mode changes. The 210-frame demo switches to Cycle and back to Transit through
real pointer clicks, scrolls the itinerary, selects another departure, and pans
the map. Its arrow follows minimum-jerk quadratic curves. Planner results are
seeded through the same resource actions that accept live responses; none of
this data enters the live app.

For review, check complete route geometry beside the enlarged panels, whole
rows at the scroll boundary, reserved Live badges, marker hit regions, and
collision-free names. Check the named Lindwurmstraße / Sendlinger Straße corridor,
Sonnenstraße, Isar, SENDLING / ALTSTADT and U / U/S station badges; their measured
plates must stay crisp at enlarged interface scales and clear of the panel.
History belongs in the map control column, never in a
second bar above the surface. Existing search remains the platform palette.
