#![doc = include_str!("../CHARTS.md")]

//! This module is a convenience facade. Chart widgets remain available from
//! [`crate::widget`] for consistency with the rest of the toolkit.

pub use crate::chart_palette::ChartPalette;

pub use crate::widget::{
    AxisLabelFormat, AxisRange, AxisScale, AxisTick, BarGauge, Candle, CandlestickChart,
    CandlestickStyle, CartesianAnnotation, CartesianAnnotationKind, CartesianBrush,
    CartesianBrushSelection, CartesianPan, CartesianPlot, CartesianStyle, CartesianTooltipAnchor,
    CartesianZoom, Category, CategoryChart, CategoryChartStyle, CategoryLayout, CategorySeries,
    CategoryValue, Chart, ChordChart, ChordLink, ChordNode, ChordSelection, ChordStyle,
    DistributionChart, DistributionKind, DistributionSeries, DistributionStyle, FunnelChart,
    FunnelDirection, FunnelStage, FunnelStyle, GaugeThresholds, Heatmap, HeatmapCell, HeatmapStyle,
    Histogram, HistogramNormalization, LegendPosition, MarkerShape, PieChart, PieSlice, PieStyle,
    PieTooltipAnchor, PlotAxis, PlotDatum, PlotSeries, PolarAreaChart, PolarAreaDatum,
    PolarAreaStyle, RadarAxis, RadarChart, RadarSeries, RadarStyle, RangeBar, RangeChart,
    RangeStyle, RingGauge, SankeyChart, SankeyLink, SankeyNode, SankeySelection, SankeyStyle,
    Series, SeriesKind, Sparkline, SunburstChart, SunburstNode, SunburstStyle, Treemap,
    TreemapItem, TreemapStyle,
};
