#![doc = include_str!("../CHARTS.md")]

pub use crate::chart_palette::ChartPalette;

pub use crate::widget::chart_legend as legend;
pub use crate::widget::function_plane::{format_tick, nice_step, nice_ticks};
pub use crate::widget::{
    AxisLabelFormat, AxisRange, AxisScale, AxisTick, BarGauge, CartesianAnimationStyle,
    CartesianAnnotation, CartesianAnnotationKind, CartesianBrush, CartesianBrushSelection,
    CartesianGesturePolicy, CartesianPan, CartesianPlot, CartesianRenderingStyle, CartesianStyle,
    CartesianTooltipAnchor, CartesianTooltipStyle, CartesianZoom, Category, CategoryChart,
    CategoryChartStyle, CategoryLayout, CategorySeries, CategoryValue, Chart, ChartItem, ChartLink,
    ChartNode, ChartStyle, FunctionPlane, GaugeReading, GaugeStyle, GaugeThresholds,
    GraphSelection, Heatmap, HeatmapCell, HeatmapStyle, MarkerShape, PieChart, PieSlice, PieStyle,
    PieTooltipAnchor, Plot, PlotAxis, PlotDatum, PlotMarks, PlotSeries, RollingSeries, SankeyChart,
    SankeySelection, SankeyStyle, Series, SeriesKind, Sparkline, SunburstChart, SunburstNode,
    SunburstStyle, Treemap, TreemapStyle, TrendCurve, TrendFill, TrendRange, TrendStyle,
};
