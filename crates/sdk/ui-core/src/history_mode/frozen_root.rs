//! The default History preview: the app's own root, synchronised, laid out and
//! drawn over the document as it was at one version. The root is a separate
//! read-only instance; no event, focus or dispatch path ever reaches it.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash};
use tilo_automerge::ApplyIncoming;
use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};

use super::{HistoryPreview, PreviewAccessibility, PreviewNavigation, PreviewView};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// The committed live document frozen at `heads`. Forked and isolated rather
/// than `fork_at`, whose Automerge 0.8 collector can walk a merged graph out
/// of order and panic.
pub(crate) fn document_at(live: &AutoCommit, heads: &[ChangeHash]) -> AutoCommit {
    let mut doc = live.clone();
    if doc.pending_ops() > 0 {
        doc.rollback();
    }
    let mut frozen = doc.fork();
    frozen.isolate_at(heads);
    frozen
}

/// A past version drawn by a read-only root of the app over its frozen
/// document.
pub struct FrozenRootPreview {
    root: Box<dyn Widget>,
    doc: Option<AutoCommit>,
    label: String,
    bounds: Rect,
    view: PreviewView,
}

impl FrozenRootPreview {
    /// `label` names what is shown ("Calendar", "Document") for assistive
    /// technology; `doc` is the frozen version the preview owns.
    pub fn new(root: Box<dyn Widget>, doc: AutoCommit, label: impl Into<String>) -> Self {
        Self {
            root,
            doc: Some(doc),
            label: label.into(),
            bounds: Rect::ZERO,
            view: PreviewView::default(),
        }
    }

    /// Open scrolled to `view`, where the person left the previous version.
    #[must_use]
    pub fn with_view(mut self, view: PreviewView) -> Self {
        self.view = view;
        self
    }

    /// The root drawing the version.
    pub fn root(&self) -> &dyn Widget {
        self.root.as_ref()
    }
}

impl HistoryPreview for FrozenRootPreview {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a preview pane is a few thousand logical pixels"
    )]
    fn layout(
        &mut self,
        bounds: Rect,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Result<(), String> {
        self.bounds = bounds;
        let size = (
            bounds.width().max(1.0) as u32,
            bounds.height().max(1.0) as u32,
        );
        let mut frame = crate::launch::ShotFrame::new(size, theme.clone());
        if let Some(doc) = self.doc.take() {
            frame = frame.with_state(crate::State::from_doc(doc));
        }
        // The view is measured by the renderer that draws it — the live
        // window's, with its fonts and text scale — or a label laid out by
        // another renderer can come out too narrow and collapse to "…".
        std::mem::swap(&mut frame.text, text);
        frame.sync(self.root.as_mut());
        let laid_out = frame.layout(self.root.as_mut());
        std::mem::swap(&mut frame.text, text);
        self.doc = Some(std::mem::take(&mut frame.state.doc));
        laid_out.map_err(|error| error.to_string())
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let offset = Vec2::new(self.bounds.x0, self.bounds.y0 - self.view.scroll.1);
        let transform = transform.pre_translate(offset);
        self.root
            .draw(scene, transform, text, theme, &PresenceCtx::empty());
    }

    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        let PreviewNavigation::Scroll { dy, .. } = command else {
            return false;
        };
        let limit = (self.root.get_rect().height() - self.bounds.height()).max(0.0);
        let next = (self.view.scroll.1 + dy).clamp(0.0, limit);
        let changed = (next - self.view.scroll.1).abs() > f64::EPSILON;
        self.view.scroll.1 = next;
        changed
    }

    fn view(&self) -> PreviewView {
        self.view.clone()
    }

    fn accessibility(&self) -> PreviewAccessibility {
        let heading = crate::i18n::I18nManager::format(
            "history-preview-description",
            &[("resource", self.label.as_str().into())],
        );
        PreviewAccessibility {
            description: std::iter::once(heading)
                .chain(crate::a11y::texts(self.root.as_ref()))
                .collect::<Vec<_>>()
                .join("\n"),
            pages: Vec::new(),
        }
    }
}

const OUTLINE_LINES: usize = 400;

/// A faithful fallback when the app's own root cannot be built a second
/// time: the version's document itself, one `path: value` line per field.
pub(crate) fn outline_root(doc: &AutoCommit) -> Box<dyn Widget> {
    use automerge::{ObjType, ReadDoc, Value};
    fn walk(doc: &AutoCommit, obj: &automerge::ObjId, path: &str, out: &mut Vec<String>) {
        let props: Vec<automerge::Prop> = match doc.object_type(obj) {
            Ok(ObjType::Map | ObjType::Table) => doc.keys(obj).map(automerge::Prop::Map).collect(),
            Ok(ObjType::List) => (0..doc.length(obj)).map(automerge::Prop::Seq).collect(),
            _ => Vec::new(),
        };
        for prop in props {
            if out.len() >= OUTLINE_LINES {
                return;
            }
            let Ok(Some((value, id))) = doc.get(obj, prop.clone()) else {
                continue;
            };
            let here = if path.is_empty() {
                prop.to_string()
            } else {
                format!("{path}.{prop}")
            };
            match value {
                Value::Object(ObjType::Text) => {
                    out.push(format!("{here}: {}", doc.text(&id).unwrap_or_default()));
                }
                Value::Object(_) => walk(doc, &id, &here, out),
                Value::Scalar(scalar) => out.push(format!("{here}: {scalar}")),
            }
        }
    }
    let mut lines = Vec::new();
    walk(doc, &automerge::ROOT, "", &mut lines);
    let outline = lines.join("\n");
    Box::new(crate::widget::Text::dynamic(move || outline.clone()).with_id("history-outline"))
}
