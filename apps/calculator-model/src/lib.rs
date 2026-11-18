//! The `calculator.document` resource: the evaluated history, newest first, and the saved
//! graphs, each row with its stable id.

use tilo_resource::{self, ActionCx, Collection, Id, Model, Path, Refusal, resource};

/// How many calculations the history keeps.
pub const HISTORY_CAP: usize = 64;

/// Which saved-graph list a row is in; both are collections of [`PlotFn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphKind {
    Curve2d,
    Surface3d,
}

impl GraphKind {
    #[must_use]
    pub fn field(self) -> &'static str {
        match self {
            GraphKind::Curve2d => "funcs",
            GraphKind::Surface3d => "surfaces",
        }
    }

    /// `y` for a curve, `z` for a surface.
    #[must_use]
    pub fn variable(self) -> &'static str {
        match self {
            GraphKind::Curve2d => "y",
            GraphKind::Surface3d => "z",
        }
    }

    /// Stable: row ids embed it.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            GraphKind::Curve2d => "curve2d",
            GraphKind::Surface3d => "surface3d",
        }
    }

    #[must_use]
    pub fn rows(self) -> Path<Calculator, Collection<PlotFn>> {
        let paths = Calculator::paths();
        match self {
            GraphKind::Curve2d => paths.funcs,
            GraphKind::Surface3d => paths.surfaces,
        }
    }
}

#[derive(Model, Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// Calculation
    #[model(label)]
    pub expr: String,
    /// Result
    pub result: String,
}

#[derive(Model, Clone, Debug, PartialEq)]
pub struct PlotFn {
    /// Formula
    #[model(label)]
    pub expr: String,
    /// Shown
    pub visible: bool,
}

/// The evaluated history, newest first, and the saved graphs, each row with its
/// stable id. A formula merges per character; visibility is written whole.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Calculator {
    /// History
    ///
    /// Only evaluating a calculation adds to it.
    #[model(readonly)]
    pub history: Collection<HistoryEntry>,
    /// Functions
    pub funcs: Collection<PlotFn>,
    /// Surfaces
    pub surfaces: Collection<PlotFn>,
}

impl Calculator {
    #[must_use]
    pub fn graphs(&self, kind: GraphKind) -> &Collection<PlotFn> {
        match kind {
            GraphKind::Curve2d => &self.funcs,
            GraphKind::Surface3d => &self.surfaces,
        }
    }

    fn graphs_mut(&mut self, kind: GraphKind) -> &mut Collection<PlotFn> {
        match kind {
            GraphKind::Curve2d => &mut self.funcs,
            GraphKind::Surface3d => &mut self.surfaces,
        }
    }

    #[must_use]
    pub fn graph_id(&self, kind: GraphKind, index: usize) -> Option<&Id> {
        self.graphs(kind).ids().nth(index)
    }

    fn add(&mut self, kind: GraphKind, id: Id, formula: &str) {
        self.graphs_mut(kind).push(
            id,
            PlotFn {
                expr: formula.trim().to_owned(),
                visible: true,
            },
        );
    }

    fn remove(&mut self, kind: GraphKind, row: &Id) -> Result<(), Refusal> {
        self.graphs_mut(kind)
            .remove(row)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such formula"))
    }

    /// Answers through [`fields`](tilo_resource::read::fields), so a caller with
    /// no window open reads the resource as it reads any app's.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<tilo_resource::__private::serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "calculator.document",
    read_content = Calculator::read_document
)]
impl Calculator {
    /// Keeps the newest [`HISTORY_CAP`].
    #[action]
    pub fn commit_history(
        &mut self,
        cx: &mut ActionCx,
        #[param("Calculation")] expr: String,
        #[param("Result")] result: String,
    ) {
        self.history
            .insert(0, cx.id(), HistoryEntry { expr, result });
        let overflow: Vec<Id> = self.history.ids().skip(HISTORY_CAP).cloned().collect();
        for id in overflow {
            self.history.remove(&id);
        }
    }

    #[action(destructive, item = history)]
    pub fn delete_history_entry(&mut self, entry: &Id) -> Result<(), Refusal> {
        self.history
            .remove(entry)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such calculation"))
    }

    #[action(destructive)]
    pub fn clear_history(&mut self) -> Result<(), Refusal> {
        if self.history.is_empty() {
            return Err(Refusal::new("the history is empty"));
        }
        self.history = Collection::default();
        Ok(())
    }

    /// Add a 2D y = f(x) curve.
    #[action]
    pub fn add_function(&mut self, cx: &mut ActionCx, #[param("Formula")] formula: String) {
        self.add(GraphKind::Curve2d, cx.id(), &formula);
    }

    /// Add a 3D z = f(x, y) surface.
    #[action]
    pub fn add_surface(&mut self, cx: &mut ActionCx, #[param("Formula")] formula: String) {
        self.add(GraphKind::Surface3d, cx.id(), &formula);
    }

    /// Remove a function from the graph.
    #[action(destructive, item = funcs)]
    pub fn delete_function(&mut self, function: &Id) -> Result<(), Refusal> {
        self.remove(GraphKind::Curve2d, function)
    }

    /// Remove a surface from the graph.
    #[action(destructive, item = surfaces)]
    pub fn delete_surface(&mut self, surface: &Id) -> Result<(), Refusal> {
        self.remove(GraphKind::Surface3d, surface)
    }
}
