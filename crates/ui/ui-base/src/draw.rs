//! What one draw pass hands the widget tree: the surface it paints on, where
//! it paints, and the plain services paint needs. A widget never sees the
//! workspace: the host paints members' carets, selections and other marks over
//! the widgets through [`WidgetOverlay`].

use crate::permissions::WidgetAuthority;
use crate::presence::PresenceKey;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use std::collections::HashMap;
use tilo_member_types::MemberId;
use vello::Scene;
use vello::kurbo::{Affine, Rect};

/// A member as a plain host-drawn face: who they are, the colour they go by,
/// whether they are an agent, and whether they are away. A widget that lays
/// out people (tab pills, list rows) reads these, never the presence behind
/// them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemberFace {
    pub name: String,
    pub colour: (u8, u8, u8),
    pub agent: bool,
    pub dimmed: bool,
}

/// What the host paints over a widget once the widget has drawn its own
/// content: a member's caret or selection in text, the focus ring of whoever
/// else is on it. The widget calls [`crate::widget::draw_over`] where its own
/// marks belong, so the overlay lands in the widget's clip and transform;
/// everything it needs of the widget it reads through the widget's published
/// geometry, never through the widget's own draw.
pub trait WidgetOverlay {
    fn paint_over(
        &self,
        widget: &dyn crate::widget::Widget,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    );
}

/// The services a draw pass threads to the widgets it descends into. Plain
/// values only: a host fills them for one frame, and they say nothing of the
/// services they were read from.
#[derive(Clone, Default)]
pub struct DrawEnv<'a> {
    /// The visibility window in scene pixels, for spatial culling.
    pub viewport: Option<Rect>,
    /// The authority the subtree being drawn answers to; `None` checks nothing.
    pub permissions: Option<std::rc::Rc<dyn WidgetAuthority>>,
    /// [`crate::permissions::shared_revision`] when that authority was lent: a
    /// retained drawing made under another revision is stale.
    pub paint_permissions: u64,
    /// Whether a host-drawn mark on text is changing now, so a widget that
    /// keeps the drawing it made of its subtree must make it again.
    pub marks_changing: bool,
    /// Counts changes to what marks a host draws into text; a retained
    /// drawing made at another count may miss one, or show one that went.
    pub marks_generation: u64,
    /// What the host paints over the widgets it draws.
    pub overlay: Option<&'a dyn WidgetOverlay>,
    /// Who is at each site the frame shows, as plain faces, for the widgets
    /// that lay people out.
    pub sites: std::rc::Rc<HashMap<PresenceKey, Vec<MemberFace>>>,
    /// Which member this session is, for a widget that marks the person's own
    /// face among others.
    pub self_member: Option<MemberId>,
    /// The workspace's presence, for an app's own painter that draws what a
    /// plain widget cannot publish: cells and rows an app lays out itself.
    /// No ui-core widget reads it.
    pub presence: Option<&'a crate::presence::PresenceCtx>,
}

impl DrawEnv<'_> {
    /// True when `rect` falls entirely outside the active viewport, so a
    /// widget can skip its draw. Off-viewport widgets MUST NOT clear their
    /// `dirty` flag, or `mark_drawn` (which only visits widgets whose draw
    /// ran) leaves them stuck dirty when scrolled back in.
    pub fn should_cull(&self, rect: Rect) -> bool {
        self.viewport
            .is_some_and(|viewport| rect.intersect(viewport).is_zero_area())
    }

    /// Replace the active viewport, returning the previous value so the caller
    /// can restore it.
    pub fn push_viewport(&mut self, viewport: Rect) -> Option<Rect> {
        self.viewport.replace(viewport)
    }

    /// Restore the viewport a [`Self::push_viewport`] replaced.
    pub fn pop_viewport(&mut self, previous: Option<Rect>) {
        self.viewport = previous;
    }

    /// Whether the authority of the subtree being drawn lets the person act on what `widget`
    /// guards. Outside any authority everything is allowed.
    pub fn permits_widget(&self, widget: &dyn crate::widget::Widget) -> bool {
        self.permissions
            .as_ref()
            .is_none_or(|authority| authority.permits_widget(widget))
    }
}

/// What a widget's draw receives: the scene, transform, text renderer and
/// theme it paints with, and the plain [`DrawEnv`] of the pass.
pub struct DrawCtx<'a> {
    pub scene: &'a mut Scene,
    pub transform: Affine,
    pub text_renderer: &'a mut TextRenderer,
    pub theme: &'a Theme,
    pub env: DrawEnv<'a>,
}

impl<'a> DrawCtx<'a> {
    pub fn new(
        scene: &'a mut Scene,
        transform: Affine,
        text_renderer: &'a mut TextRenderer,
        theme: &'a Theme,
        env: DrawEnv<'a>,
    ) -> Self {
        Self {
            scene,
            transform,
            text_renderer,
            theme,
            env,
        }
    }

    /// Borrow the parts a widget body draws with, for the body to bind to the
    /// names it uses. The environment comes back by value so a container can
    /// scope a tighter viewport for its children.
    #[allow(unused_variables, reason = "a body binds only the parts it draws with")]
    pub fn parts(
        &mut self,
    ) -> (
        &mut Scene,
        Affine,
        &mut TextRenderer,
        &'a Theme,
        DrawEnv<'a>,
    ) {
        (
            &mut *self.scene,
            self.transform,
            &mut *self.text_renderer,
            self.theme,
            self.env.clone(),
        )
    }
}
