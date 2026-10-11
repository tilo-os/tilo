# Identical Chromium frames and native redraw admission

Chromium's native helper can deliver another allocation containing the same
pixels. Previously `deliver_frame` replaced the retained allocation and woke
the UI for every receipt. `WebView::needs_animation_frame` treats allocation
identity as new image work, so identical receipts admitted another redraw.

The local delivery path now compares dimensions and RGBA bytes. An identical
receipt keeps the retained allocation and its trace identity and does not wake
the UI. A changed pixel or changed dimensions still replaces the frame and
wakes. Native tracing records an unchanged receipt separately, without claiming
another scene or presentation. The remote forwarding path is unchanged.

## Reproduction

Run the production-path tests with Cargo or a remote Cargo drop-in selected
through `CARGO`:

```sh
"${CARGO:-cargo}" test --locked -p tilo-ui --lib webview::chromium::tests:: -j8
```

`unchanged_frames_do_not_wake_or_replace_the_painted_pixels` failed before the
fix: two separately allocated identical frames produced two wakes instead of
one. The candidate retains the first allocation and trace ID. Its negative
controls check a one-byte change and a dimensions change.

`repeated_full_size_packets_keep_one_frame_but_events_still_wake` uses the
actual routed-packet encoder, decoder and delivery path. Forty identical
868 × 772 packets produce one frame wake, rather than the forty admitted by
the previous unconditional replacement. Every decoded packet has a fresh pixel
allocation. A subsequent title event still wakes and remains in the event
queue. This is 97.5% fewer frame wakes for this synthetic workload; it is not a
measurement of CPU time, display FPS, app startup or OS boot time.

## Qualification limits

A passive software-rendered VM sample motivated this investigation: Browser
used 237.12% of one logical CPU over 20.96 seconds, while Store used 0.19%.
A separate five-second observation saw approximately two Browser surface
submissions per second and none for Store. These observations did not establish
that Browser's page or model was unchanged. The VM app cohort predates current
startup changes, so it is not a candidate-versus-baseline comparison.

In an isolated GPU-only SDK fixture, three validated Vulkan/llvmpipe runs each
created four unnamed workers alongside four named llvmpipe workers. A diagnostic
thread-entry interposer attributed all ten created threads to
`libvulkan_lvp.so`. The matching VM thread-name and birth-time pattern supports
a driver-worker hypothesis; it does not directly identify the VM's hot stacks.
The interposer ran only in owned remote diagnostic processes. No guest profiling
permissions or settings were changed.

A matched Browser/runtime artifact and controlled unchanged-page observation
are required to quantify the real application's CPU and presentation benefit.
The byte comparison itself scans an unchanged frame; receipt decoding and
Chromium rendering still occur. No hardware or full-boot improvement is claimed.
