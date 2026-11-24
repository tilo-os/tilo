#![doc = include_str!("../CHARTS.md")]

pub use crate::chart_palette::ChartPalette;

pub use crate::widget::chart_legend as legend;
pub use crate::widget::function_plane::{format_tick, nice_step, nice_ticks};
pub use crate::widget::{
    AxisLabelFormat, AxisRange, AxisScale, AxisTick, BarGauge, Candle, CandleObservation,
    CandlestickChart, CandlestickChartProps, CandlestickStyle, CartesianAnimationStyle,
    CartesianAnnotation, CartesianAnnotationKind, CartesianBrush, CartesianBrushSelection,
    CartesianGesturePolicy, CartesianPan, CartesianPlot, CartesianPlotProps,
    CartesianRenderingStyle, CartesianStyle, CartesianTooltipAnchor, CartesianTooltipStyle,
    CartesianZoom, Category, CategoryChart, CategoryChartStyle, CategoryLayout, CategorySeries,
    CategoryValue, Chart, ChartItem, ChartLink, ChartNode, ChartStyle, ChordChart, ChordSelection,
    ChordStyle, Curve, DistributionChart, DistributionKind, DistributionSeries, DistributionStyle,
    FunctionPlane, FunnelChart, FunnelDirection, FunnelStyle, GaugeReading, GaugeStyle,
    GaugeThresholds, GraphSelection, Heatmap, HeatmapCell, HeatmapStyle, Histogram, HistogramChart,
    HistogramNormalization, HistogramProps, HistogramScale, Marker, MarkerShape, PieChart,
    PieSlice, PieStyle, PieTooltipAnchor, Plot, PlotAxis, PlotDatum, PlotMarks, PlotPoints,
    PlotSeries, PolarAreaChart, PolarAreaStyle, RadarAxis, RadarChart, RadarLegendStyle,
    RadarSeries, RadarStyle, RangeBar, RangeChart, RangeChartProps, RangeKind, RangeSpan,
    RangeStyle, RollingSeries, SankeyChart, SankeySelection, SankeyStyle, Series, SeriesKind,
    Sparkline, SunburstChart, SunburstNode, SunburstStyle, Treemap, TreemapStyle, TrendCurve,
    TrendFill, TrendRange, TrendStyle,
};
