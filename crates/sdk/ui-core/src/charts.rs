#![doc = include_str!("../CHARTS.md")]

//! This module is a convenience facade. Chart widgets remain available from
//! [`crate::widget`] for consistency with the rest of the toolkit.

pub use crate::chart_palette::ChartPalette;

pub use crate::widget::function_plane::{format_tick, nice_step};
pub use crate::widget::{
    AxisLabelFormat, AxisRange, AxisScale, AxisTick, BarGauge, Candle, CandlestickChart,
    CandlestickStyle, CartesianAnimationStyle, CartesianAnnotation, CartesianAnnotationKind,
    CartesianBrush, CartesianBrushSelection, CartesianGesturePolicy, CartesianPan, CartesianPlot,
    CartesianRenderingStyle, CartesianStyle, CartesianTooltipAnchor, CartesianTooltipStyle,
    CartesianZoom, Category, CategoryChart, CategoryChartStyle, CategoryLayout, CategorySeries,
    CategoryValue, Chart, ChartLabels, ChartStyle, ChordChart, ChordLink, ChordNode,
    ChordSelection, ChordStyle, DistributionChart, DistributionKind, DistributionSeries,
    DistributionStyle, FunctionPlane, FunnelChart, FunnelDirection, FunnelStage, FunnelStyle,
    GaugeReading, GaugeStyle, GaugeThresholds, Heatmap, HeatmapCell, HeatmapStyle, Histogram,
    HistogramNormalization, LegendPosition, MarkerShape, PieChart, PieSlice, PieStyle,
    PieTooltipAnchor, PlotAxis, PlotDatum, PlotSeries, PolarAreaChart, PolarAreaDatum,
    PolarAreaStyle, RadarAxis, RadarChart, RadarLegendStyle, RadarSeries, RadarStyle, RangeBar,
    RangeChart, RangeStyle, RollingSeries, SankeyChart, SankeyLink, SankeyNode, SankeySelection,
    SankeyStyle, Series, SeriesKind, Sparkline, SunburstChart, SunburstNode, SunburstStyle,
    Treemap, TreemapItem, TreemapStyle, TrendCurve, TrendFill, TrendRange, TrendStyle,
};
