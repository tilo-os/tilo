//! [`ImageCropEditor`] as a described component: crop a picture by its corners.

use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::component::{Component, Described, Event, Value};
use crate::pictures::{Pictures, Shown};
use crate::widget::{ImageCropEditor, TaffyTreeExt, Widget};

use super::CropSelection;
use crate::widget::DrawChild;
use crate::widget::PictureSource;

/// A picture with a crop its four corners set (a scanned page, a photo to trim): drag a corner
/// to move it, or an edge's arrow keys, and it reports the crop each time it changes. The
/// picture is letterboxed in its box; the crop is a share of the picture, never of the box.
#[derive(Component, Default)]
#[component(name = "ImageCropEditor", role = "Group")]
pub struct ImageCropEditorProps {
    /// The picture to crop.
    #[component(required, positional)]
    pub image: PictureSource,
    /// The left crop edge, from 0 (the picture's left) to 1.
    #[component(required)]
    pub left: f64,
    /// The top crop edge, from 0 (the picture's top) to 1.
    #[component(required)]
    pub top: f64,
    /// The right crop edge, to 1 at the picture's right.
    #[component(required)]
    pub right: f64,
    /// The bottom crop edge, to 1 at the picture's bottom.
    #[component(required)]
    pub bottom: f64,
    /// The crop changed: carries its edges, left, top, right, bottom, each `0..=1`.
    #[component(event(value = Vec<f64>))]
    pub change: Event,
    /// The window's pictures, through which the picture loads.
    #[component(skip)]
    pub pictures: Option<Pictures>,
    /// The box's shape before the picture is here (its width over its height).
    pub aspect: Option<f64>,
}

/// The crop editor of a picture once its bytes arrive, reporting the crop the person sets.
pub struct CropEditor {
    shown: Shown,
    editor: Option<ImageCropEditor>,
    selection: CropSelection,
    change: Event,
    /// The box's shape before the picture is here (its width over its height).
    aspect: Option<f64>,
    rect: Rect,
}

impl CropEditor {
    /// The editor of `picture` starting at `selection`, reporting each change to `change`.
    #[must_use]
    pub fn new(pictures: Pictures, picture: PictureSource, selection: CropSelection) -> Self {
        let mut shown = Shown::default();
        shown.set(pictures, picture);
        Self {
            shown,
            editor: None,
            selection,
            change: Event::None,
            aspect: None,
            rect: Rect::ZERO,
        }
    }

    /// Report each crop change to `event`.
    #[must_use]
    pub fn reporting(mut self, event: Event) -> Self {
        self.change = event;
        self
    }

    /// The box stands at `width / height` before the picture's shape is known.
    #[must_use]
    pub fn with_aspect(mut self, aspect: f64) -> Self {
        self.aspect = Some(aspect);
        self
    }

    /// Take the picture's bytes once they are here, and build the editor around it.
    fn take_picture(&mut self) {
        if !self.shown.resolve() {
            return;
        }
        let Some(image) = self.shown.image() else {
            return;
        };
        let rgba: std::sync::Arc<[u8]> = std::sync::Arc::from(image.data.data().to_vec());
        let selection = crate::LocalState::new(self.selection);
        self.editor = Some(ImageCropEditor::new(
            rgba,
            image.width,
            image.height,
            selection,
        ));
    }

    /// Report the crop the editor shows now, when it changed since the last frame.
    fn report_crop(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        let selection = editor.selection();
        if selection == self.selection {
            return;
        }
        self.selection = selection;
        self.change.fire(Some(Value::List(vec![
            Value::Float(selection.left),
            Value::Float(selection.top),
            Value::Float(selection.right),
            Value::Float(selection.bottom),
        ])));
    }
}

impl Widget for CropEditor {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.take_picture();
        if let Some(editor) = &mut self.editor {
            editor.sync(state, path);
        }
        self.report_crop();
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let arrived = self.shown.waiting();
        self.take_picture();
        if let Some(editor) = &mut self.editor {
            editor.update(state, path);
        }
        self.report_crop();
        arrived
    }

    fn needs_redraw(&self) -> bool {
        self.shown.waiting()
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // One leaf of its own: the editor paints into the rect it is given, so it never takes
        // a percentage height (which, inside a scrolling page whose height is its content's,
        // never resolves). It takes the width it is given and stands at its aspect.
        let _ = theme;
        taffy.widget_leaf(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::auto(),
                },
                // A box with no shape still stands at 4:3, so a leaf that is not yet sized
                // does not collapse to nothing.
                aspect_ratio: Some(crate::num::f32_of_f64(self.aspect.unwrap_or(4.0 / 3.0))),
                flex_shrink: 0.0,
                ..taffy::Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(editor) = &mut self.editor {
            editor.apply_layout(taffy, node, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        if let Some(editor) = &mut self.editor {
            editor.draw_child(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let Some(editor) = &mut self.editor {
            editor.handle_event(event, ctx);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "ImageCropEditor"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.editor.as_ref().and_then(Widget::accessibility)
    }
}

impl Described for CropEditor {
    type Props = ImageCropEditorProps;

    fn from_props(_key: &str, props: ImageCropEditorProps) -> Self {
        let ImageCropEditorProps {
            image,
            left,
            top,
            right,
            bottom,
            change,
            pictures,
            aspect,
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        let selection = CropSelection {
            left,
            top,
            right,
            bottom,
        };
        let mut editor = Self::new(pictures, image, selection).reporting(change);
        editor.aspect = aspect;
        editor
    }

    fn set_props(&mut self, props: ImageCropEditorProps) -> bool {
        let ImageCropEditorProps {
            image,
            left,
            top,
            right,
            bottom,
            change,
            pictures,
            aspect,
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        self.change = change;
        self.aspect = aspect;
        self.selection = CropSelection {
            left,
            top,
            right,
            bottom,
        };
        // A changed picture loads again; a changed crop from the model is the view's word, not
        // reported back (the person's own drag already was).
        let swapped = self.shown.set(pictures, image);
        if swapped {
            self.editor = None;
        }
        true
    }
}
