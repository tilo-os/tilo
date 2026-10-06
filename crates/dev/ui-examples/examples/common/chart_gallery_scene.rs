//! Populated, interactive reference scene for the chart catalogue.

use tilo_ui::TextRole;
use tilo_ui::chart_palette::ChartPalette;
use tilo_ui::num::f64_of_usize;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{
    AxisLabelFormat, Candle, CandlestickChart, Card, CardGrid, CartesianAnnotation, CartesianPan,
    CartesianPlot, CartesianRenderingStyle, CartesianStyle, Category, CategoryChart,
    CategoryLayout, CategorySeries, ChordChart, ChordLink, ChordNode, ChordSelection, Column,
    DistributionChart, DistributionSeries, FunnelChart, FunnelStage, Heatmap, HeatmapCell,
    Histogram, MarkerShape, PieChart, PieSlice, PlotAxis, PlotDatum, PlotSeries, PolarAreaChart,
    PolarAreaDatum, PolarAreaStyle, RadarAxis, RadarChart, RadarSeries, RangeBar, RangeChart,
    SankeyChart, SankeyLink, SankeyNode, SankeySelection, Scrollable, SeriesKind, SunburstChart,
    SunburstNode, Text, Treemap, TreemapItem, Widget,
};

const CHART_HEIGHT: f32 = 270.0;

fn card(title: &str, chart: impl Widget + 'static) -> Box<dyn Widget> {
    Box::new(Card::new(Box::new(chart)).with_title(title))
}

/// Build the same scene used by the live example and offscreen visual checks.
pub fn build(theme: &Theme) -> Box<dyn Widget> {
    build_scene(theme, false)
}

#[allow(
    dead_code,
    reason = "used by the screenshot registry, not the live gallery binary"
)]
pub fn build_inspected(theme: &Theme) -> Box<dyn Widget> {
    build_scene(theme, true)
}

#[allow(
    clippy::too_many_lines,
    reason = "this is one declarative visual fixture catalogue; splitting its chart definitions across helpers would obscure the rendered ordering without isolating reusable behavior"
)]
fn build_scene(theme: &Theme, inspected: bool) -> Box<dyn Widget> {
    let palette = ChartPalette::from_theme(theme);
    let c = [
        palette.categorical(0),
        palette.categorical(1),
        palette.categorical(2),
        palette.categorical(3),
        palette.categorical(4),
        palette.categorical(5),
    ];
    let mut pie = PieChart::new(vec![
        PieSlice::new("design", "Design", 38.0, c[0]).with_detail("152 hours"),
        PieSlice::new("engineering", "Engineering", 31.0, c[1]).with_detail("124 hours"),
        PieSlice::new("research", "Research", 19.0, c[2]).with_detail("76 hours"),
        PieSlice::new("operations", "Operations", 12.0, c[3]).with_detail("48 hours"),
    ])
    .with_center("400 h", "this month")
    .with_height(CHART_HEIGHT);

    let points = |values: &[f64]| {
        values
            .iter()
            .enumerate()
            .map(|(i, value)| PlotDatum::new(format!("p{i}"), f64_of_usize(i), *value))
            .collect()
    };
    let mut cartesian = CartesianPlot::new(vec![
        PlotSeries::new(
            "actual",
            "Actual",
            c[0],
            points(&[18.0, 25.0, 22.0, 35.0, 42.0, 48.0]),
        )
        .kind(SeriesKind::Smooth)
        .marker_shape(MarkerShape::Diamond)
        .sorted_by_x(),
        PlotSeries::new(
            "forecast",
            "Forecast",
            c[3],
            [14.0, 19.0, 0.0, 31.0, 38.0, 44.0]
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    if index == 2 {
                        PlotDatum::missing("forecast-gap", f64_of_usize(index))
                    } else {
                        PlotDatum::new(format!("f{index}"), f64_of_usize(index), value)
                    }
                })
                .collect(),
        )
        .kind(SeriesKind::SmoothArea)
        .fill_alpha(38)
        .dashed([7.0, 4.0])
        .sorted_by_x(),
    ])
    .with_annotations([
        CartesianAnnotation::x_band("launch", 2.7, 3.5, c[1])
            .labelled("Launch window")
            .with_detail("Click to open the deployment report")
            .fill_alpha(24),
        CartesianAnnotation::y_line("target", 40.0, c[2])
            .labelled("Target 40")
            .with_detail("Forecast performance objective"),
    ])
    .styled(CartesianStyle {
        rendering: CartesianRenderingStyle {
            show_markers_on_lines: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .with_height(CHART_HEIGHT);

    let mut logarithmic = CartesianPlot::new(vec![
        PlotSeries::new(
            "latency",
            "Tail latency",
            c[2],
            [
                ("l1", 1.0, 0.001),
                ("l2", 10.0, 0.006),
                ("l3", 100.0, 0.08),
                ("l4", 1000.0, 1.0),
            ]
            .into_iter()
            .map(|(id, x, y)| {
                PlotDatum::new(id, x, y)
                    .labelled(format!("{x:.0} req/s"))
                    .with_detail(format!("{y:.3} s tail latency"))
            })
            .collect(),
        )
        .kind(SeriesKind::Line)
        .marker_radius(5.0)
        .sorted_by_x(),
    ])
    .styled(CartesianStyle {
        x_axis: PlotAxis::default()
            .logarithmic()
            .formatted(AxisLabelFormat::Compact)
            .titled("Requests / second"),
        y_axis: PlotAxis::default()
            .logarithmic()
            .formatted(AxisLabelFormat::Duration(0))
            .titled("Tail latency"),
        rendering: CartesianRenderingStyle {
            show_markers_on_lines: true,
            ..Default::default()
        },
        pan: CartesianPan::XY,
        ..Default::default()
    })
    .with_height(CHART_HEIGHT);

    let mut histogram = Histogram::new(
        [
            2.1, 2.4, 2.5, 2.7, 2.8, 2.8, 2.9, 3.0, 3.0, 3.1, 3.2, 3.4, 3.7, 4.1,
        ],
        c[5],
    )
    .bins(7)
    .build()
    .with_height(CHART_HEIGHT);

    let categories = ["Mon", "Tue", "Wed", "Thu", "Fri"]
        .into_iter()
        .map(|label| Category::new(label, label))
        .collect();
    let mut bars = CategoryChart::new(
        categories,
        vec![
            CategorySeries::new("desktop", "Desktop", c[0], [14.0, 18.0, 16.0, 22.0, 24.0]),
            CategorySeries::new("mobile", "Mobile", c[1], [9.0, 12.0, 15.0, 17.0, 19.0]),
        ],
    )
    .layout(CategoryLayout::Stacked)
    .with_height(CHART_HEIGHT);

    let heat_cells = (0_usize..5)
        .flat_map(|row| {
            (0_usize..7).map(move |column| {
                let value = row
                    .saturating_mul(11)
                    .saturating_add(column.saturating_mul(7))
                    .saturating_add(3)
                    .checked_rem(18)
                    .map_or(0.0, f64_of_usize);
                HeatmapCell::new(format!("{row}-{column}"), row, column, value)
                    .with_detail(format!("{value:.0} sessions"))
            })
        })
        .collect();
    let mut heatmap = Heatmap::new(
        ["09", "12", "15", "18", "21"].map(String::from).to_vec(),
        ["M", "T", "W", "T", "F", "S", "S"]
            .map(String::from)
            .to_vec(),
        heat_cells,
    )
    .with_height(CHART_HEIGHT);

    let mut treemap = Treemap::new(vec![
        TreemapItem::new("src", "Source", 42.0).colored(c[0]),
        TreemapItem::new("assets", "Assets", 25.0).colored(c[1]),
        TreemapItem::new("tests", "Tests", 16.0).colored(c[2]),
        TreemapItem::new("docs", "Docs", 10.0).colored(c[3]),
        TreemapItem::new("config", "Config", 7.0).colored(c[5]),
    ])
    .with_height(CHART_HEIGHT);

    let mut radar = RadarChart::new(
        ["Speed", "Quality", "Reach", "Trust", "Ease"]
            .into_iter()
            .map(|label| RadarAxis::new(label, label, 100.0))
            .collect(),
        vec![
            RadarSeries::new(
                "current",
                "Current",
                c[0],
                vec![82.0, 91.0, 65.0, 88.0, 76.0],
            ),
            RadarSeries::new(
                "previous",
                "Previous",
                c[3],
                vec![70.0, 80.0, 72.0, 75.0, 68.0],
            ),
        ],
    )
    .with_height(CHART_HEIGHT);

    let mut distributions = DistributionChart::new(vec![
        DistributionSeries::new(
            "alpha",
            "Alpha",
            c[0],
            [12.0, 14.0, 14.5, 15.0, 16.0, 18.0, 23.0],
        ),
        DistributionSeries::new(
            "beta",
            "Beta",
            c[1],
            [8.0, 11.0, 13.0, 17.0, 17.5, 19.0, 20.0],
        ),
        DistributionSeries::new(
            "gamma",
            "Gamma",
            c[2],
            [10.0, 12.0, 16.0, 18.0, 22.0, 25.0, 27.0],
        ),
    ])
    .with_height(CHART_HEIGHT);

    let mut candles = CandlestickChart::new(
        [
            (42.0, 48.0, 40.0, 46.0),
            (46.0, 49.0, 43.0, 44.0),
            (44.0, 52.0, 43.0, 50.0),
            (50.0, 54.0, 47.0, 53.0),
            (53.0, 56.0, 49.0, 51.0),
            (51.0, 59.0, 50.0, 57.0),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (open, high, low, close))| {
            Candle::new(
                format!("d{i}"),
                format!("Day {}", i.saturating_add(1)),
                open,
                high,
                low,
                close,
            )
            .with_volume(20.0 + f64_of_usize(i) * 8.0)
        })
        .collect(),
    )
    .with_height(CHART_HEIGHT);

    let mut sankey = SankeyChart::new(
        ["Visits", "Signup", "Trial", "Paid", "Churn"]
            .into_iter()
            .zip(c.iter().copied().cycle())
            .map(|(label, color)| SankeyNode::new(label, label).colored(color))
            .collect(),
        vec![
            SankeyLink::new("v-s", "Visits", "Signup", 72.0),
            SankeyLink::new("v-t", "Visits", "Trial", 28.0),
            SankeyLink::new("s-t", "Signup", "Trial", 54.0),
            SankeyLink::new("t-p", "Trial", "Paid", 61.0),
            SankeyLink::new("t-c", "Trial", "Churn", 21.0),
        ],
    )
    .with_height(CHART_HEIGHT);

    let mut chord = ChordChart::new(
        ["Design", "Engineering", "Research", "Support", "Sales"]
            .into_iter()
            .zip(c.iter().copied().cycle())
            .map(|(label, color)| ChordNode::new(label, label).colored(color))
            .collect(),
        vec![
            ChordLink::new("d-e", "Design", "Engineering", 32.0),
            ChordLink::new("d-r", "Design", "Research", 14.0),
            ChordLink::new("e-r", "Engineering", "Research", 25.0),
            ChordLink::new("e-s", "Engineering", "Support", 18.0),
            ChordLink::new("support-sales", "Support", "Sales", 27.0),
            ChordLink::new("sales-design", "Sales", "Design", 21.0),
        ],
    )
    .with_height(CHART_HEIGHT);

    let mut sunburst = SunburstChart::new(SunburstNode::new("all", "All work", 0.0).with_children(
        vec![
            SunburstNode::new("product", "Product", 0.0).colored(c[0]).with_children(vec![
                SunburstNode::new("design", "Design", 32.0),
                SunburstNode::new("build", "Build", 48.0),
            ]),
            SunburstNode::new("market", "Market", 0.0).colored(c[1]).with_children(vec![
                SunburstNode::new("sales", "Sales", 27.0),
                SunburstNode::new("support", "Support", 19.0),
            ]),
        ],
    ))
    .with_center("126", "projects")
    .with_height(CHART_HEIGHT);

    let mut funnel = FunnelChart::new(vec![
        FunnelStage::new("visit", "Visited", 12_400.0).colored(c[0]),
        FunnelStage::new("signup", "Signed up", 7_850.0).colored(c[3]),
        FunnelStage::new("trial", "Started trial", 4_920.0).colored(c[1]),
        FunnelStage::new("paid", "Subscribed", 2_640.0).colored(c[2]),
    ])
    .with_height(CHART_HEIGHT);

    let mut timeline = RangeChart::new(vec![
        RangeBar::new("discover", "Discover", 0.0, 3.5)
            .with_progress(1.0)
            .colored(c[0])
            .with_detail("Complete"),
        RangeBar::new("design", "Design", 2.0, 6.5)
            .with_progress(0.82)
            .colored(c[3]),
        RangeBar::new("build", "Build", 5.0, 11.0)
            .with_progress(0.46)
            .colored(c[1]),
        RangeBar::new("launch", "Launch", 12.0, 12.0)
            .colored(c[2])
            .with_detail("Milestone"),
    ])
    .axis_titled("Week");

    let mut rose = PolarAreaChart::new(
        [
            ("Jan", 34.0),
            ("Feb", 51.0),
            ("Mar", 72.0),
            ("Apr", 58.0),
            ("May", 88.0),
            ("Jun", 65.0),
            ("Jul", 79.0),
            ("Aug", 46.0),
        ]
        .into_iter()
        .zip(c.iter().copied().cycle())
        .map(|((label, value), color)| {
            PolarAreaDatum::new(label, label, value)
                .colored(color)
                .with_detail("active users")
        })
        .collect(),
    )
    .styled(PolarAreaStyle {
        inner_radius: 0.3,
        ..Default::default()
    })
    .with_center("88K", "peak")
    .with_height(CHART_HEIGHT);

    if inspected {
        let _ = pie.inspect("engineering");
        let _ = cartesian.inspect_annotation("launch");
        let _ = logarithmic.inspect("latency", "l3");
        let _ = histogram.inspect("histogram", "bin-3");
        let _ = bars.set_series_visible("desktop", false);
        let _ = bars.inspect("mobile", "Wed");
        let _ = heatmap.inspect("2-3");
        let _ = treemap.inspect("assets");
        let _ = radar.set_series_visible("previous", false);
        let _ = radar.inspect("current", "Quality");
        let _ = distributions.inspect("beta");
        let _ = candles.inspect("d3");
        let _ = sankey.inspect(&SankeySelection::Link("t-p".into()));
        let _ = chord.inspect(&ChordSelection::Node("Engineering".into()));
        let _ = sunburst.inspect("build");
        let _ = timeline.inspect("build");
        let _ = rose.inspect("Apr");
        let _ = funnel.inspect("trial");
    }

    let cards = vec![
        card("Donut · selectable slices", pie),
        card("Mixed Cartesian · smooth + gapped area", cartesian),
        card("Log axes · orders of magnitude", logarithmic),
        card("Histogram · raw samples", histogram),
        card("Categories · stacked bars", bars),
        card("Heatmap · dense matrix", heatmap),
        card("Treemap · weighted hierarchy", treemap),
        card("Radar · multivariate comparison", radar),
        card("Distribution · violin + box", distributions),
        card("Candlestick · OHLC + volume", candles),
        card("Sankey · weighted flow", sankey),
        card("Chord · weighted relationships", chord),
        card("Sunburst · recursive hierarchy", sunburst),
        card("Timeline · Gantt ranges + milestones", timeline),
        card("Polar area · radial magnitude", rose),
        card("Funnel · conversion stages", funnel),
    ];
    let content = Column::new()
        .with_intrinsic_height()
        .with_gap(18.0)
        .push(Box::new(Text::new("Interactive chart gallery").with_role(TextRole::Display)))
        .push(Box::new(Text::new(
            "Resize to reflow. Hover points, cells, links, nodes, and segments; click to activate accessible data targets.",
        )))
        .push(Box::new(
            CardGrid::new(cards).with_card_width(tilo_ui::widget::CardWidth::Wide),
        ));
    Box::new(Scrollable::new_vertical().with_child(Box::new(content)))
}
