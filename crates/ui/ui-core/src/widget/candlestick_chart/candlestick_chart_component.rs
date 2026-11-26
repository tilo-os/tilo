//! [`CandlestickChart`] as a described component: a price room per period, and
//! [`CandleObservation`] as one period's open, high, low and close.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::component::{
    Component, Described, PropType, Type, Value, case_field, case_field_description,
};

use super::{Candle, CandlestickChart};

/// One period of a price: what it opened and closed at, how high and low it went, and how much
/// was traded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CandleObservation {
    pub label: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl PropType for CandleObservation {
    fn ty() -> Type {
        Type::Cases {
            name: "Candle".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        if case != "candle" {
            return Err(format!("a period of a price is a `candle`, not `{case}`"));
        }
        Ok(Self {
            label: case_field(fields, "label")?.ok_or("`candle` needs `label`, the period")?,
            open: case_field(fields, "open")?
                .ok_or("`candle` needs `open`, the price it opened at")?,
            high: case_field(fields, "high")?.ok_or("`candle` needs `high`, the highest price")?,
            low: case_field(fields, "low")?.ok_or("`candle` needs `low`, the lowest price")?,
            close: case_field(fields, "close")?
                .ok_or("`candle` needs `close`, the price it closed at")?,
            volume: case_field(fields, "volume")?.unwrap_or(0.0),
        })
    }
}

/// The description of [`CandleObservation`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn candle_description() -> EnumDescription {
    EnumDescription {
        name: "Candle".to_owned(),
        doc: "One period of a price: its open, high, low and close.".to_owned(),
        cases: vec![CaseDescription {
            name: "candle".to_owned(),
            doc: "A period named `label`, from `open` through `high` and `low` to `close`."
                .to_owned(),
            fields: vec![
                case_field_description(
                    "label",
                    Type::Text,
                    None,
                    "Which period it is: a day, a week, a session.",
                ),
                case_field_description("open", Type::Float, None, "The price it opened at."),
                case_field_description(
                    "high",
                    Type::Float,
                    None,
                    "The highest price in it; never below the open or the close.",
                ),
                case_field_description(
                    "low",
                    Type::Float,
                    None,
                    "The lowest price in it; never above the open or the close.",
                ),
                case_field_description("close", Type::Float, None, "The price it closed at."),
                case_field_description(
                    "volume",
                    Type::Float,
                    Some(Value::Float(0.0)),
                    "How much was traded in it, drawn under the prices when the chart shows volume.",
                ),
            ],
        }],
    }
}

/// A price per period as candles: what rose and fell when. Each period is a candle from its open
/// to its close, its wick reaching the period's high and low; a period that closed above its open
/// is drawn in the theme's success ink, one that closed below it in its danger ink, and a flat one
/// dim. The traded volume stands under the prices when shown.
#[derive(Component, Default)]
#[component(name = "CandlestickChart", role = "Image", label = "title")]
pub struct CandlestickChartProps {
    /// The periods, oldest first: `.candle(label: "Monday", open: 82.0, high: 88.0, low: 80.0, close: 86.0)`.
    #[component(required, positional)]
    pub candles: Vec<CandleObservation>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
    /// The traded volume under the prices; off unless said.
    pub volume: bool,
}

/// The candles `periods` are drawn as.
fn candles(periods: &[CandleObservation]) -> Vec<Candle> {
    periods
        .iter()
        .enumerate()
        .map(|(at, period)| {
            let id = if period.label.is_empty() {
                at.to_string()
            } else {
                period.label.clone()
            };
            Candle::new(
                id,
                period.label.clone(),
                period.open,
                period.high,
                period.low,
                period.close,
            )
            .with_volume(period.volume)
        })
        .collect()
}

impl CandlestickChart {
    /// Everything of `props` but its periods.
    fn wear(&mut self, props: CandlestickChartProps) {
        self.marks.frame.title = props.title.unwrap_or_default();
        self.marks.frame.empty = props.empty.unwrap_or_default();
        self.marks.style.show_volume = props.volume;
        self.mark_dirty();
    }
}

impl Described for CandlestickChart {
    type Props = CandlestickChartProps;

    fn from_props(_key: &str, props: CandlestickChartProps) -> Self {
        let mut chart = Self::new(Vec::new());
        chart.set_props(props);
        chart
    }

    /// Changed periods stand where the old ones stood: the widget stays, with what the person is
    /// inspecting.
    fn set_props(&mut self, props: CandlestickChartProps) -> bool {
        self.marks.candles = candles(&props.candles);
        self.wear(props);
        true
    }
}
