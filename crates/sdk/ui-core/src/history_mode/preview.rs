//! Presentation-only inputs. No live widget, document loan or service enters a preview.
use super::HistorySnapshot;
use crate::{text_render::TextRenderer, theme::Theme};
use std::{collections::BTreeMap, sync::Arc};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewView {
    pub scroll: (f64, f64),
    pub zoom: f64,
    pub page: Option<String>,
    /// The live window's own presentation (a mode, the open item), so a
    /// past version opens as the window shows it, drawn by the same view
    /// ([`crate::application_view::ApplicationViewState`]).
    pub application: Option<crate::application_view::ApplicationLocalView>,
}

impl Default for PreviewView {
    fn default() -> Self {
        Self {
            scroll: (0.0, 0.0),
            zoom: 1.0,
            page: None,
            application: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum PreviewNavigation {
    Scroll { dx: f64, dy: f64 },
    Zoom { factor: f64, anchor: Point },
    Reveal(String),
    PreviousPage,
    NextPage,
    SelectPage(String),
}

/// Keys are immutable historical content hashes admitted by the source owner.
/// Absence is an unavailable historical asset, never a request for current bytes.
#[derive(Clone, Debug, Default)]
pub struct FrozenPreviewAssets(BTreeMap<String, Arc<[u8]>>);
impl FrozenPreviewAssets {
    pub fn new(assets: BTreeMap<String, Arc<[u8]>>) -> Self {
        Self(assets)
    }
    pub fn get(&self, hash: &str) -> Option<&[u8]> {
        self.0.get(hash).map(AsRef::as_ref)
    }
}

pub struct PreviewInput {
    pub snapshot: HistorySnapshot,
    pub initial_view: PreviewView,
    pub assets: FrozenPreviewAssets,
}

/// Descriptions and closed page-navigation targets only; never arbitrary actions.
#[derive(Clone, Debug, Default)]
pub struct PreviewAccessibility {
    pub description: String,
    pub pages: Vec<(String, String)>,
}

pub trait HistoryPreview {
    /// # Errors
    /// Returns a description when the preview cannot lay out the frozen content.
    fn layout(
        &mut self,
        bounds: Rect,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Result<(), String>;
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    );
    fn navigate(&mut self, command: PreviewNavigation) -> bool;
    fn view(&self) -> PreviewView;
    fn accessibility(&self) -> PreviewAccessibility;
    /// The live window's bar presents search and History for the app, so
    /// the version is drawn as the live view is: its header without a
    /// button of its own that opens the app's search.
    fn set_window_presents_search(&mut self, presents: bool) {
        let _ = presents;
    }
}

pub type PreviewFactory = fn(PreviewInput) -> Result<Box<dyn HistoryPreview>, String>;
