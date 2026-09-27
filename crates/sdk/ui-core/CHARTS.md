# Charts and plots

Tilo charts are retained widgets, not a separate canvas runtime. They use the
same layout, theme, pointer, accessibility and automation paths as every other
UI control. Import the focused facade:

```rust
# use tilo_ui as ui;
use ui::charts::{CartesianPlot, PlotDatum, PlotSeries, SeriesKind};
```

## Choosing a chart

| Question | Widget |
|---|---|
| Trend or relationship on numeric axes | `CartesianPlot` |
| Tiny inline trend | `Sparkline` |
| Categories side-by-side, stacked or cumulative | `CategoryChart` |
| Distribution from raw observations | `Histogram` or `DistributionChart` |
| Part-to-whole | `PieChart` |
| Seasonal or cyclical magnitude | `PolarAreaChart` |
| Hierarchical part-to-whole | `SunburstChart` or `Treemap` |
| Matrix intensity | `Heatmap` |
| Several metrics with different scales | `RadarChart` |
| Open/high/low/close data | `CandlestickChart` |
| Movement between stages | `SankeyChart` |
| Dense weighted relationships | `ChordChart` |
| Schedule, interval, reservation, or milestone | `RangeChart` |
| Sequential conversion | `FunnelChart` |
| Compact progress or threshold value | `RingGauge` / `BarGauge` |

`CartesianPlot` series may be mixed. It supports line, smooth line, step,
straight/smooth/stepped area, scatter, bubble, bar and lollipop marks. `CategoryChart` supports grouped,
stacked, normalized-percent and waterfall layouts. `DistributionChart` supports
box, violin and combined plots. `FunnelChart` also renders pyramids.
`RangeChart` covers timelines, Gantt plans, numeric intervals and milestones,
with an optional per-range progress fill and caller-defined value formatter.
`PolarAreaChart` (rose/radial bars) keeps equal angular categories while radius
encodes magnitude; its `radius_exponent` supports perceptual area scaling.
`ChordChart` automatically allocates weighted node arcs and connection ribbons;
both are independently hoverable, clickable, and accessible.

## Interactive gallery

The catalogue is executable example code rather than a static mockup. It
contains populated examples of every chart family, reflows from two columns to
one, and retains each widget's hover, click, focus, and accessibility behavior:

```text
cargo run -p ui-examples --example ui-lab -- chart_gallery
```

Deterministic wide/narrow, light/dark renders use the same scene:

```text
cargo run -p ui-examples --example ui-shots -- chart_gallery_shots
```

## A mixed Cartesian plot

```rust
# use tilo_ui as ui;
use ui::charts::*;
# use tilo_ui::vello::peniko::Color;
# let accent = Color::from_rgb8(0x3B, 0x82, 0xF6);
# let warning = Color::from_rgb8(0x3B, 0x82, 0xF6);

let observations = vec![
    PlotDatum::new("jan", 1.0, 18.0)
        .labelled("January")
        .with_detail("18 ms median"),
    PlotDatum::new("feb", 2.0, 12.0)
        .labelled("February")
        .with_detail("12 ms median"),
];

let latency = PlotSeries::new("latency", "Latency", accent, observations)
    .kind(SeriesKind::Smooth)
    .stroke_width(2.5)
    .marker_radius(4.0)
    .marker_shape(MarkerShape::Diamond)
    .dashed([8.0, 4.0]);

let style = CartesianStyle {
    x_axis: PlotAxis::default().with_ticks([
        AxisTick::new(1.0, "Jan"),
        AxisTick::new(2.0, "Feb"),
    ]),
    y_axis: PlotAxis::default()
        .fixed(0.0, 25.0)
        .formatted(AxisLabelFormat::Decimal(1))
        .with_tick_density(7, 64.0)
        .titled("Milliseconds"),
    rendering: CartesianRenderingStyle {
        show_crosshair: true,
        show_markers_on_lines: true,
        ..Default::default()
    },
    zoom: CartesianZoom::X,
    pan: CartesianPan::X,
    brush: CartesianBrush::X,
    ..Default::default()
};

let plot = CartesianPlot::new(vec![latency])
    .with_annotations([
        CartesianAnnotation::y_line("budget", 20.0, warning)
            .labelled("Latency budget"),
        CartesianAnnotation::x_band("deploy", 1.25, 1.5, accent)
            .labelled("Deployment"),
    ])
    .styled(style)
    .titled("Request latency")
    .with_height(320.0);
```

Every datum has a stable `id`. Hover selects the nearest datum, the inspector
uses its label/detail, clicking queues `(series_id, datum_id)`, and assistive
technology receives the same actionable data points. Read activations with
`take_activated()` during the parent widget's `update` pass.
Touch starts inspection immediately but only activates on a stationary
release; scrolling, dragging, cancellation, and a second finger suppress the
tap. Every interactive chart family—including legends, dense matrices, radial
hierarchies, flows, and timelines—uses the same tap-versus-scroll semantics, so
narrow layouts remain operable without stealing page navigation.

Series support straight, smooth, and stepped lines as well as straight,
smooth, and stepped areas. Use `dashed()` for arbitrary stroke patterns,
`fill_alpha()` for area density, `MarkerShape::{Circle,Square,Diamond}` for
point identity, and `bar_radius()` for square through pill-like bars. These
remain ordinary `PlotSeries` builders, so changing visual grammar does not
change the data or interaction model.
Represent absent observations with `PlotDatum::missing(id, x)`. Lines and
areas break at missing or scale-invalid samples by default, preventing charts
from implying continuity that the data does not contain. Opt into interpolation
with `.connect_gaps(true)`. Per-series `.show_markers(bool)` overrides the
plot-wide marker preference when visual density differs between series.

Connected series automatically use contiguous min/max sampling when their
point count exceeds the responsive pixel budget. This preserves thin spikes
and troughs while preventing resize, zoom, and animation frames from
tessellating an invisible surplus of points. The original data remains intact
for exact hover, keyboard inspection, activation, domains, and accessibility.
Tune or disable the behavior with
`CartesianStyle::with_adaptive_sampling(enabled, points_per_pixel)`.
Auto/fixed domains are cached and invalidated on data, style, or visibility
changes instead of being rescanned on every frame. For finite ascending-X
series, add `.sorted_by_x()` to use an exact binary-window hover search; the
full scan remains the safe default for unsorted data and during animation.

Use `CartesianAnnotation` for labelled X/Y reference lines, highlighted X/Y
intervals, and point callouts. Their coordinates stay in data space through
resize, logarithmic projection, zoom, and pan. Constructors and fluent
`labelled`, `stroke_width`, `solid`, and `fill_alpha` modifiers cover common
analytical overlays; `set_annotations()` and `annotations_mut()` support live
updates and fully dynamic dashboards. Annotations are hoverable, clickable,
keyboard/assistive-technology activatable, and may provide richer tooltip text
with `with_detail()`. Read their stable id with `take_annotation_activated()`,
focus one from application state with `inspect_annotation()`, or opt decorative
ink out of interaction with `.interactive(false)`.

Cartesian inspectors can follow the pointer or remain anchored to the datum
with `tooltip_anchor`. `tooltip_width`, `tooltip_min_width`, `tooltip_offset`,
and `tooltip_padding` control geometry, while `tooltip_show_coordinates`
appends values formatted by each axis. Auto placement flips and clamps against
all chart edges, including very narrow cards.

`set_series()` smoothly interpolates matching series and datum ids by default,
so live updates preserve visual identity and hit testing follows the painted
positions. Tune `animation_duration` or disable `animate_changes`; the active
theme's reduced-motion preference always snaps immediately, and animation
frame requests stop when the transition completes.
Identical updates are no-ops rather than animation restarts. Use
`set_series_immediate()`, `finish_animation()`, and `is_animating()` for initial
hydration, deterministic capture, or application-controlled motion.

Generated ticks automatically become sparser as a plot narrows. Use
`with_tick_density(maximum, minimum_spacing)` to tune that responsive policy,
and `AxisLabelFormat::{Auto, Decimal, Compact, Percent, Bytes, Scientific,
Duration, Currency}` for common formatting without allocating explicit ticks.
Durations adapt seconds into microseconds through days. Explicit ticks remain
the fully custom/date-label escape hatch and preserve their first and last
values while intermediate labels are sampled to fit.

Call `PlotAxis::logarithmic()` for a base-10 axis. Non-positive values are
excluded consistently from rendering, hit testing, keyboard navigation, and
accessibility. Auto domains use multiplicative padding, generated ticks prefer
decades, and zoom, pan, viewport restoration, and brush selections remain in
the caller's original data coordinates.
Axis titles configured with `PlotAxis::titled()` reserve their own responsive
layout space and render on both axes; long titles are Unicode-safely elided
instead of colliding with ticks or escaping narrow chart bounds.

Set `zoom` to `CartesianZoom::X` for timelines or `CartesianZoom::XY` for
exploratory plots. Wheel zoom is anchored under the pointer and requires
Ctrl/Command by default, so charts embedded in scrollable pages do not trap
navigation. Native trackpad pinch zoom is also cursor-anchored and needs no
modifier because the gesture is unambiguous. Dedicated chart surfaces can set
`zoom_modifier_required: false`.
Use `viewport()`, `take_view_changed()`, and `reset_view()` to synchronize
plots, persist navigation, or provide a visible reset control. Restore or link
views with `set_viewport()`, move them using `pan_view_by()` or the
size-independent `pan_view_fraction()`, and turn a brush directly into a detail
view with `zoom_to_selection()`.

Set `pan` to `CartesianPan::X` or `CartesianPan::XY` for direct navigation.
Alt-drag pans by default, and native trackpad pan gestures use the same
scale-aware behavior when Alt is held. Dedicated chart surfaces may set
`pan_modifier_required: false`. Logarithmic axes pan multiplicatively while
`viewport()` and change events continue to expose ordinary data coordinates.

Set `brush` to `CartesianBrush::X` for interval selection or
`CartesianBrush::XY` for rectangular selection. Shift-drag starts a brush by
default, preserving ordinary scrolling and inspection; set
`brush_modifier_required: false` on a dedicated analysis surface. Read the
domain-space result with `take_brush_selection()`—the result is independent of
the chart's pixel size and ready for filtering, aggregation, or linked views.

Visible legends are interactive by default. Clicking or assistive activation
toggles a series, automatically recomputes auto-ranges, and removes hidden data
from hit testing. Applications can use `set_series_visible()`,
`is_series_visible()`, and `take_visibility_changed()` to restore or synchronize
that state. Set `interactive_legend: false` for a purely descriptive legend.

## Part-to-whole

`PieChart::set_slices()` interpolates values by stable slice id, including
growing newly introduced slices from zero. Painted arcs and hit testing share
the same animated sectors. Configure `animate_changes` and
`animation_duration` in `PieStyle`; reduced-motion themes always snap and stop
requesting animation frames.
`PieStyle` also exposes tooltip enablement, pointer-versus-slice anchoring,
fixed or content width, minimum width, offset, padding, optional percentages,
and a right inset for adjacent legends. Placement flips and clamps at all
edges while protecting donut center labels. This lets compact dashboards,
touch-first drill-downs, and externally labelled pies share the same widget
without accepting one hard-coded overlay.
Identical slice updates do not restart motion; `set_slices_immediate()`,
`finish_animation()`, and `is_animating()` provide explicit lifecycle control.

```rust
# use tilo_ui as ui;
# use ui::charts::*;
# use tilo_ui::vello::peniko::Color;
# let blue = Color::from_rgb8(0x3B, 0x82, 0xF6);
# let amber = Color::from_rgb8(0x3B, 0x82, 0xF6);
# let gray = Color::from_rgb8(0x3B, 0x82, 0xF6);
let chart = PieChart::new(vec![
    PieSlice::new("src", "Source", 48.0, blue).with_detail("48 GB"),
    PieSlice::new("target", "Build output", 21.0, amber).with_detail("21 GB"),
    // Visible and inspectable, but does not pretend there is an action.
    PieSlice::new("reserved", "Reserved", 4.0, gray).non_interactive(),
])
.with_style(PieStyle {
    inner_radius: 0.64, // zero makes a conventional pie
    gap: 0.018,
    hover_offset: 7.0,
    ..Default::default()
})
.with_center("69 GB", "total size");
```

Use `SunburstChart` when the data has more than one hierarchy level. Its node
values may be explicit or `<= 0`, in which case they are derived recursively
from children. Use `Treemap` when label density matters more than radial form.

## Raw distributions

Callers do not need to calculate bins, quartiles, outliers or density curves:

```rust
# use tilo_ui as ui;
# use ui::charts::*;
# use tilo_ui::vello::peniko::Color;
# let accent = Color::from_rgb8(0x3B, 0x82, 0xF6);
# let blue = Color::from_rgb8(0x3B, 0x82, 0xF6);
# let green = Color::from_rgb8(0x3B, 0x82, 0xF6);
# let samples = vec![1.0, 2.0, 2.5, 3.0];
# let before = samples.clone();
# let after = vec![0.5, 1.5, 2.0];
let histogram = Histogram::new(samples.clone(), accent)
    .bins(24)
    .normalize(HistogramNormalization::Density)
    .titled("Response distribution")
    .build();

let comparison = DistributionChart::new(vec![
    DistributionSeries::new("before", "Before", blue, before),
    DistributionSeries::new("after", "After", green, after),
])
.kind(DistributionKind::BoxAndViolin);
```

## Updating live data

Use `set_series`, `set_data`, `set_cells`, `set_items`, `set_stages`, or the
type-specific setter. These preserve the retained widget and avoid rebuilding
the surrounding tree. Stable item IDs preserve semantic identity. Charts do no
work while idle and request no animation frames by themselves.

Large connected series are adaptively reduced to spike-preserving min/max
buckets based on the current plot width. Full-resolution data remains in the
widget for domains, exact inspection, activation, accessibility, and export;
use `.sorted_by_x()` for binary-window hover lookup. Tune or disable rendering
reduction through `CartesianStyle::with_adaptive_sampling(...)`. Applications
may still pre-aggregate when domain-specific semantics require it.

A dashboard that samples on a timer can keep each history in a
`RollingSeries`: a fixed-capacity window whose `as_slice()` reads oldest to
newest and feeds `Series`, `Sparkline` or a `Chart` directly.

## Painting inside a custom-drawn surface

An app that lays out its own canvas (a dashboard card grid, a scrolling strip)
still uses the toolkit's charts, painting them at a known rect instead of
hand-rolling paths:

| Painter | Draws |
|---|---|
| `Chart::draw_styled` | Rolling line/area series with grid, header/legend and axis labels |
| `Sparkline::paint` | One trend of `f32` or `f64` samples; `Sparkline::points` returns where each sample lands |
| `RingGauge::draw_styled` / `BarGauge::draw_styled` | A fraction as a ring or labelled bar |
| `BarGauge::draw_span` | A `from..=to` span on a track, e.g. a day's low–high on the week's range |
| `FunctionPlane::paint_grid` | The pan/zoom grid, axes and 1-2-5 tick labels of a function plot |

`TrendStyle` chooses line width, a flat or gradient fill, straight or
Catmull-Rom joins, per-sample markers and a leading dot on the newest sample;
`TrendRange` maps values onto the plot (`up_to(max)`, or `fit(&data)` for the
data's own span). `ChartStyle` and `GaugeStyle` carry the grid, header and
track choices. Colours still resolve from the `Theme` at paint time.

## Customization and theming

Use `ChartPalette::from_theme(theme)` for a visually balanced categorical
palette plus sequential and diverging ramps that follow the active light,
dark, or high-contrast design tokens. `categorical(index)` wraps stably for
any series count; `sequential(0.0..=1.0)` and `diverging(-1.0..=1.0)` clamp
safely. `ChartPalette::new(primary, additional)` and the `with_*` methods make
fully custom, non-empty palettes concise.

Each family has a public `*Style` structure whose fields can be set with struct
update syntax. Geometry, labels, legends, grids, inspectors, value annotations,
colors and chart-specific behavior are independent. Supplying `None` for an
optional color delegates it to the OS theme. Explicit data colors are useful
when color has domain meaning; never copy surface or text colors out of a
particular light/dark theme.

Every chart accepts either `.styled(style)` or `.with_style(style)`, so both
the chart-focused and toolkit-wide builder vocabularies are predictable. Most
fixed-canvas charts expose `.with_height(...)`; `RangeChart` intentionally
derives its height from row count and `RangeStyle::row_height`, keeping Gantt
rows readable as data changes.

All chart widgets expose both `style()` and `style_mut()`. This makes live
theme editors, compact-mode policies, and user preferences cheap to apply
without replacing the retained widget or losing hover/selection identity.

Tooltips, focus outlines, dim text, grids and surfaces always resolve from the
current `Theme` during drawing. This keeps the same chart intentional in light,
dark and high-contrast modes.

## Interaction and accessibility

- Every chart family supports arrows, Home, End, Enter, Space, and Escape using
  the same inspection and activation state as pointer input. `inspect(...)`
  and `clear_inspection()` expose that state to apps.
- Comparative Cartesian, category, and radar legends are controls by default. Hidden
  series are dimmed and excluded from pointer, keyboard, and accessibility
  inspection. Visibility can be restored, persisted, or linked with
  `set_series_visible()`, `toggle_series()`, `is_series_visible()`, and
  `take_visibility_changed()`; set `interactive_legend: false` for a purely
  descriptive legend. Legends wrap at narrow widths and reserve matching plot
  space, including long-label truncation and full-row hit targets.
- Hover and click geometry comes from the exact same projection as painting.
- Inspectors avoid window edges and contain the stable label plus formatted
  detail supplied by the caller.
- Every actionable slice, stage, node, link, candle, cell and data point is a
  synthetic accessibility button with stable identity.
- Accessibility activation queues the same result as pointer activation.
- Empty, invalid and non-finite values are ignored safely instead of poisoning
  domains or geometry.

When a click changes application state, expose the same capability as a
resource action. Chart activation is a presentation input, not a replacement
for the application's semantic action surface.
