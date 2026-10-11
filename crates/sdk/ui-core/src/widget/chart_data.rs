//! Data shared by the chart widgets: valued items, graph nodes and the
//! weighted links between them. `id` survives updates and is what activation
//! reports.

use vello::peniko::Color;

/// One valued item: a funnel stage, a polar wedge, a treemap tile.
#[derive(Clone, Debug)]
pub struct ChartItem {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}

impl ChartItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn colored(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// A node of a flow or relationship graph (sankey, chord).
#[derive(Clone, Debug)]
pub struct ChartNode {
    pub id: String,
    pub label: String,
    pub color: Option<Color>,
    pub detail: String,
}

impl ChartNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn colored(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// A weighted link from one [`ChartNode`] to another, by node id.
#[derive(Clone, Debug)]
pub struct ChartLink {
    pub id: String,
    pub source: String,
    pub target: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}

impl ChartLink {
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        target: impl Into<String>,
        value: f64,
    ) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            target: target.into(),
            value,
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn colored(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}
