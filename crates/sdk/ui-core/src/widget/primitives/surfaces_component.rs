//! [`ModalDialog`], [`FloatingPanel`] and [`Popover`] as described components.
//!
//! A modal is how a view asks a question over the window (HIG §4b, §7c); a popover is how it
//! shows a menu or a small form beside its control. Both are the window's one top layer, which
//! the host runs after its chrome, so a view places them in its tree and the toolkit presents
//! them.
//!
//! Each is generic over its content in Rust; here it is given a column of views, and the view
//! is `ui.ModalDialog`, `ui.FloatingPanel` or `ui.Popover`.

use vello::kurbo::{Affine, Point, Rect};

use crate::component::{Component, Described, Event};
use crate::widget::modal_dialog::DialogAction;
use crate::widget::{
    Button, Container, Element, FloatingPanel, ModalDialog, PanelAlign, Popover, PopoverSide,
    Widget,
};

/// The views of a slot in a column, the column filling its width.
fn column(views: Vec<Element>) -> Container {
    let mut stack = crate::widget::arrange::Stack::stack()
        .fill_width()
        .gap(crate::tokens::Rhythm::Related);
    for view in views {
        stack = stack.push(view);
    }
    Container::new()
        .with_no_padding()
        .with_child(Box::new(stack))
}

/// A question over the window: a dimmed page, a sheet with a title, the view's content, and up
/// to three answers. Escape or a click on the scrim closes it; Enter fires the main answer,
/// except in a destructive question, where Enter is the safe one.
#[derive(Component, Default)]
#[component(name = "ModalDialog", role = "Dialog", label = "title")]
pub struct ModalProps {
    /// What the question is, as its title (name the action: "Delete 3 notes?").
    #[component(required)]
    pub title: String,
    /// A glyph before the title naming what asks.
    pub icon: Option<crate::icons::Icon>,
    /// What assistive technology says after the title.
    pub description: Option<String>,
    /// Whether it is open. A closed dialog paints nothing and takes no input.
    pub open: bool,
    /// The main answer's label ("Delete"): the inverted button, and what Enter fires.
    pub primary: Option<String>,
    /// The other answer's label, usually Cancel. Escape fires it.
    pub secondary: Option<String>,
    /// A third answer's label, standing apart ("Don't save"). Enter never fires it.
    pub tertiary: Option<String>,
    /// The main answer destroys something: it is a destructive button, and Enter is the safe
    /// answer instead.
    pub destructive: bool,
    /// The main answer cannot be taken yet. A dialog with this has it disabled.
    pub primary_enabled: bool,
    /// The view's content, inside the sheet.
    #[component(slot(default))]
    pub body: Vec<Element>,
    /// The main answer was taken.
    #[component(event)]
    pub act_primary: Event,
    /// The other answer was taken.
    #[component(event)]
    pub act_secondary: Event,
    /// The third answer was taken.
    #[component(event)]
    pub act_tertiary: Event,
    /// It was closed (Escape, the scrim, or a click on Cancel): a dialog with this closes as
    /// it is.
    #[component(event)]
    pub dismiss: Event,
}

/// A modal driven from a view: its answers and its dismissal reach the view's events.
pub struct DeclaredModal {
    dialog: ModalDialog<Container>,
}

impl DeclaredModal {
    /// The modal the props describe.
    #[must_use]
    pub fn new(mut props: ModalProps) -> Self {
        let mut dialog =
            ModalDialog::new(props.title.clone(), column(std::mem::take(&mut props.body)))
                .open(props.open);
        if let Some(icon) = props.icon {
            dialog = dialog.with_icon(icon);
        }
        if let Some(description) = &props.description {
            dialog = dialog.with_description(description.clone());
        }
        if let Some(label) = &props.primary {
            let event = props.act_primary.clone();
            dialog = dialog.primary_action(label.clone(), move || event.fire(None));
        }
        if let Some(label) = &props.secondary {
            let event = props.act_secondary.clone();
            dialog = dialog.secondary_action(label.clone(), move || event.fire(None));
        }
        if let Some(label) = &props.tertiary {
            let event = props.act_tertiary.clone();
            dialog = dialog.tertiary_action(label.clone(), move || event.fire(None));
        }
        if props.destructive {
            dialog = dialog.with_destructive(true);
        }
        if !props.dismiss.is_none() {
            let close = props.dismiss.clone();
            dialog = dialog.on_dismiss(move || close.fire(None));
        }
        let enabled = props.primary_enabled;
        dialog = dialog.primary_enabled_if(move |_| enabled);
        Self { dialog }
    }

    /// The dialog, for a host to place.
    #[must_use]
    pub fn dialog(&mut self) -> &mut ModalDialog<Container> {
        &mut self.dialog
    }
}

impl Widget for DeclaredModal {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.dialog.sync(state, path);
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        self.dialog.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.dialog.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.dialog.apply_layout(taffy, node, origin);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.dialog
            .draw_overlay(scene, transform, text, theme, presence);
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.dialog.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.dialog.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.dialog.children()
    }

    /// The dialog's content is laid out in the top layer even while the wrapper's children
    /// hide it: the walk follows the dialog's own.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        crate::widget::Widget::for_each_layout_child(&self.dialog, visitor)
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.dialog.children_mut()
    }

    fn get_rect(&self) -> Rect {
        self.dialog.get_rect()
    }

    fn traps_focus(&self) -> bool {
        Widget::traps_focus(&self.dialog)
    }

    fn name(&self) -> &'static str {
        "ModalDialog"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.dialog.accessibility()
    }
}

impl Described for DeclaredModal {
    type Props = ModalProps;

    fn from_props(_key: &str, props: ModalProps) -> Self {
        Self::new(props)
    }

    /// What it asks, how it is answered and whether it is open change where the dialog stands,
    /// so a field in its sheet keeps what was typed in it.
    fn set_props(&mut self, props: ModalProps) -> bool {
        let ModalProps {
            title,
            icon,
            description,
            open,
            primary,
            secondary,
            tertiary,
            destructive,
            primary_enabled,
            body: _,
            act_primary,
            act_secondary,
            act_tertiary,
            dismiss,
        } = props;
        let dialog = &mut self.dialog;
        dialog.set_title(title);
        dialog.set_icon(icon);
        dialog.set_description(description);
        dialog.set_open(open);
        let answer = |label: Option<String>, event: &Event| {
            label.map(|label| {
                let event = event.clone();
                DialogAction::new(label, move || event.fire(None))
            })
        };
        dialog.set_primary_action(answer(primary, &act_primary));
        dialog.set_secondary_action(answer(secondary, &act_secondary));
        dialog.set_tertiary_action(answer(tertiary, &act_tertiary));
        dialog.set_destructive(destructive);
        dialog.set_primary_gate(None);
        dialog.set_primary_enabled(primary_enabled);
        dialog.set_on_dismiss((!dismiss.is_none()).then(|| {
            let close = dismiss.clone();
            Box::new(move || close.fire(None)) as Box<dyn FnMut()>
        }));
        true
    }

    /// The sheet's content changes by key in place.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn crate::component::SlotViews> {
        if slot != "body" {
            return None;
        }
        let column = self
            .dialog
            .content_mut()
            .child_mut()?
            .as_any_mut()?
            .downcast_mut::<crate::widget::arrange::Arranged<crate::widget::arrange::FlexLayout>>(
        )?;
        Some(column)
    }
}

/// A floating panel a view places itself: a header slot, a body and a footer slot, on the
/// floating material.
///
/// A view rarely places one: a menu is a `Popover` beside its control, a question a
/// `ModalDialog`, and the window's chrome belongs to the shell (§7c). It is for the surface a
/// canvas's own inspector or a document's navigator stands on.
#[derive(Component, Default)]
#[component(name = "FloatingPanel", role = "Group")]
pub struct FloatingPanelProps {
    /// The panel's body.
    #[component(slot(default))]
    pub body: Vec<Element>,
    /// Its header, above the body; a field there renders embedded.
    #[component(slot)]
    pub header: Option<Element>,
    /// Its footer, under the body.
    #[component(slot)]
    pub footer: Option<Element>,
    /// A fixed width, in logical pixels (a utility panel).
    pub width: Option<f64>,
}

/// A floating panel whose body, header and footer may change by key.
pub struct DeclaredPanel {
    panel: FloatingPanel,
    rect: Rect,
    /// The one-view slot being changed ([`Described::slot_views`]): the header or the footer.
    editing: PanelSlot,
}

/// A floating panel's slot that holds one view, or none.
#[derive(Clone, Copy)]
enum PanelSlot {
    Header,
    Footer,
}

impl DeclaredPanel {
    /// The panel the props describe.
    #[must_use]
    pub fn new(mut props: FloatingPanelProps) -> Self {
        let mut panel = FloatingPanel::new(Box::new(column(std::mem::take(&mut props.body))));
        if let Some(header) = props.header.take() {
            panel = panel.header(header);
        }
        if let Some(footer) = props.footer.take() {
            panel = panel.footer(footer);
        }
        if let Some(width) = props.width {
            panel = panel.width(crate::num::f32_of_f64(width));
        }
        Self {
            panel,
            rect: Rect::ZERO,
            editing: PanelSlot::Header,
        }
    }

    fn slot(&mut self) -> &mut Option<Element> {
        match self.editing {
            PanelSlot::Header => self.panel.header_mut(),
            PanelSlot::Footer => self.panel.footer_mut(),
        }
    }

    fn put(&mut self, mut view: Element) {
        if matches!(self.editing, PanelSlot::Header) {
            FloatingPanel::embed_header(view.as_mut());
        }
        *self.slot() = Some(view);
    }

    /// The panel, for a host to place.
    #[must_use]
    pub fn panel(&mut self) -> &mut FloatingPanel {
        &mut self.panel
    }
}

impl Widget for DeclaredPanel {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.panel.sync(state, path);
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        self.panel.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.panel.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.panel.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.panel.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.panel.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.panel.children()
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        crate::widget::Widget::for_each_layout_child(&self.panel, visitor)
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.panel.children_mut()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "FloatingPanel"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.panel.accessibility()
    }
}

impl Described for DeclaredPanel {
    type Props = FloatingPanelProps;

    fn from_props(_key: &str, props: FloatingPanelProps) -> Self {
        Self::new(props)
    }

    /// The panel's width changes where it stands; its body, header and footer change by key
    /// through [`Self::slot_views`], so a field in the header keeps what was typed in it.
    fn set_props(&mut self, props: FloatingPanelProps) -> bool {
        self.panel
            .set_width(props.width.map(crate::num::f32_of_f64));
        true
    }

    /// Each slot's views change by key in place, so a change of what the body or footer
    /// shows keeps the header's field, and with it focus and what is being typed.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn crate::component::SlotViews> {
        match slot {
            "body" => {
                let column = self
                    .panel
                    .body_mut()
                    .as_any_mut()?
                    .downcast_mut::<Container>()?
                    .child_mut()?
                    .as_any_mut()?
                    .downcast_mut::<crate::widget::arrange::Arranged<
                        crate::widget::arrange::FlexLayout,
                    >>()?;
                Some(column)
            }
            "header" => {
                self.editing = PanelSlot::Header;
                Some(self)
            }
            "footer" => {
                self.editing = PanelSlot::Footer;
                Some(self)
            }
            _ => None,
        }
    }
}

/// The view of the header or footer being changed: one, or none.
impl crate::component::SlotViews for DeclaredPanel {
    fn len(&self) -> usize {
        let slot = match self.editing {
            PanelSlot::Header => self.panel.has_header(),
            PanelSlot::Footer => self.panel.has_footer(),
        };
        usize::from(slot)
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index != 0 {
            return None;
        }
        self.slot().as_mut()
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.put(view);
    }

    fn remove(&mut self, _index: usize) {
        *self.slot() = None;
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.put(view);
    }
}

/// A menu or a small form beside its control: the one anchored floating panel. The view's
/// content is inside it.
#[derive(Component, Default)]
#[component(name = "Popover", role = "Group", label = "label")]
pub struct PopoverProps {
    /// Its accessible name (a menu says what it chooses: "Refresh interval").
    pub label: Option<String>,
    /// Whether it is open.
    pub open: bool,
    /// The side of its anchor it prefers: `below` unless said.
    pub side: Option<PopoverSide>,
    /// It was closed (Escape, or a press outside it): a popover with this closes as it is.
    #[component(event)]
    pub dismiss: Event,
    /// Its content.
    #[component(slot(default))]
    pub content: Vec<Element>,
}

/// A popover whose content may change by key.
pub struct DeclaredPopover {
    popover: Popover<Container>,
    rect: Rect,
}

impl DeclaredPopover {
    /// The popover the props describe.
    #[must_use]
    pub fn new(mut props: PopoverProps) -> Self {
        let mut popover =
            Popover::with_content(column(std::mem::take(&mut props.content))).open(props.open);
        if let Some(label) = &props.label {
            popover = popover.with_label(label.clone());
        }
        if let Some(side) = props.side {
            popover = popover.side(side);
        }
        if !props.dismiss.is_none() {
            let close = props.dismiss.clone();
            popover = popover.on_dismiss(move || close.fire(None));
        }
        Self {
            popover,
            rect: Rect::ZERO,
        }
    }

    /// The popover, for a host to anchor.
    #[must_use]
    pub fn popover(&mut self) -> &mut Popover<Container> {
        &mut self.popover
    }
}

impl Widget for DeclaredPopover {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.popover.sync(state, path);
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        self.popover.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.popover.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.popover.apply_layout(taffy, node, origin);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.popover
            .draw_overlay(scene, transform, text, theme, presence);
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.popover.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.popover.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.popover.children()
    }

    /// A popover's content is laid out open or closed (so reopening finds its nodes): the
    /// walk follows the popover's own.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        crate::widget::Widget::for_each_layout_child(&self.popover, visitor)
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.popover.children_mut()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "Popover"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.popover.accessibility()
    }
}

impl Described for DeclaredPopover {
    type Props = PopoverProps;

    fn from_props(_key: &str, props: PopoverProps) -> Self {
        Self::new(props)
    }

    /// Whether it is open, what it is called and where it opens change where it stands: a
    /// popover the person opened stays open, and a field in it keeps what was typed in it.
    fn set_props(&mut self, props: PopoverProps) -> bool {
        let PopoverProps {
            label,
            open,
            side,
            dismiss,
            content: _,
        } = props;
        let popover = &mut self.popover;
        popover.set_label_option(label);
        popover.set_side(side.unwrap_or_default());
        popover.set_open(open);
        popover.set_on_dismiss((!dismiss.is_none()).then(|| {
            let close = dismiss.clone();
            Box::new(move || close.fire(None)) as Box<dyn FnMut()>
        }));
        true
    }

    /// Its content changes by key in place.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn crate::component::SlotViews> {
        if slot != "content" {
            return None;
        }
        let column = self
            .popover
            .content_mut()
            .child_mut()?
            .as_any_mut()?
            .downcast_mut::<crate::widget::arrange::Arranged<crate::widget::arrange::FlexLayout>>(
        )?;
        Some(column)
    }
}

/// A button that opens a popover anchored to itself: a filter's options, a share sheet, a
/// picker of values. A press opens it, and it closes by a second press, Escape, or a press
/// outside it.
#[derive(Component, Default)]
#[component(name = "PopoverButton", role = "Button", label = "label")]
pub struct PopoverButtonProps {
    /// What the button is called, as it is announced.
    #[component(required, positional)]
    pub label: String,
    /// A glyph instead of the label, on an icon-only button.
    pub icon: Option<crate::icons::Icon>,
    /// The side of the button it opens on: `below` unless said.
    pub side: Option<PopoverSide>,
    /// Whether it opens at the button's trailing edge instead of its leading edge.
    pub trailing: bool,
    /// The popover's content.
    #[component(slot(default))]
    pub content: Vec<Element>,
}

/// A popover button whose content may change by key.
pub struct DeclaredPopoverButton {
    pair: crate::widget::PopoverButton<Container>,
}

impl DeclaredPopoverButton {
    /// The button the props describe, its content inside the popover.
    #[must_use]
    pub fn new(key: &str, mut props: PopoverButtonProps) -> Self {
        let content = column(std::mem::take(&mut props.content));
        let button = match props.icon {
            Some(icon) => crate::widget::Button::icon_only(key, icon, &props.label),
            None => crate::widget::Button::new(key, &props.label),
        };
        let mut pair = button.with_popover(content);
        if let Some(side) = props.side {
            pair = pair.side(side);
        }
        if props.trailing {
            pair = pair.aligned(crate::widget::PanelAlign::Trailing);
        }
        Self { pair }
    }

    /// The button and its popover, for a host to place.
    #[must_use]
    pub fn pair(&mut self) -> &mut crate::widget::PopoverButton<Container> {
        &mut self.pair
    }
}

impl Widget for DeclaredPopoverButton {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.pair.sync(state, path);
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        self.pair.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.pair.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.pair.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.pair.draw(scene, transform, text, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.pair
            .draw_overlay(scene, transform, text, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.pair.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.pair.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.pair.children_mut()
    }

    /// The button and its popover, and the popover's content, are laid out by the pair: the
    /// walk follows the pair's own.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        crate::widget::Widget::for_each_layout_child(&self.pair, visitor)
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.pair.for_each_child(visitor);
    }

    fn get_rect(&self) -> Rect {
        self.pair.get_rect()
    }

    fn name(&self) -> &'static str {
        "PopoverButton"
    }

    fn window_slot_barrier(&self) -> bool {
        true
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.pair.accessibility()
    }
}

impl Described for DeclaredPopoverButton {
    type Props = PopoverButtonProps;

    fn from_props(key: &str, props: PopoverButtonProps) -> Self {
        Self::new(key, props)
    }

    /// The button's words and glyph, the side it opens on and its alignment change where the
    /// pair stands, so a popover it opened stays open.
    fn set_props(&mut self, props: PopoverButtonProps) -> bool {
        let PopoverButtonProps {
            label,
            icon,
            side,
            trailing,
            content: _,
        } = props;
        let words = match icon {
            Some(icon) => Button::icon_only("", icon, &label),
            None => Button::new("", &label),
        };
        if !self.pair.button_mut().take_words(&words) {
            return false;
        }
        let popover = self.pair.popover_mut();
        popover.set_side(side.unwrap_or_default());
        popover.set_align(if trailing {
            PanelAlign::Trailing
        } else {
            PanelAlign::Leading
        });
        true
    }

    /// Its content changes by key in place.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn crate::component::SlotViews> {
        if slot != "content" {
            return None;
        }
        let column = self
            .pair
            .popover_mut()
            .content_mut()
            .child_mut()?
            .as_any_mut()?
            .downcast_mut::<crate::widget::arrange::Arranged<crate::widget::arrange::FlexLayout>>(
            )?;
        Some(column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A popover the person opened stays open while its props are given to it again, and closes
    /// when the host says it is closed.
    #[test]
    fn a_popover_takes_its_props_where_it_stands_and_keeps_what_was_opened() {
        let mut popover = DeclaredPopover::new(PopoverProps {
            label: Some("Filters".into()),
            ..PopoverProps::default()
        });
        popover.popover().set_open(true);
        let props = PopoverProps {
            open: true,
            label: Some("Filters".into()),
            side: Some(PopoverSide::Below),
            ..PopoverProps::default()
        };
        assert!(
            popover.set_props(props),
            "the popover takes its props where it stands"
        );
        assert!(
            popover.popover().is_open(),
            "the popover the person opened stayed open"
        );
        let closed = PopoverProps {
            open: false,
            label: Some("Filters".into()),
            ..PopoverProps::default()
        };
        assert!(popover.set_props(closed));
        assert!(
            !popover.popover().is_open(),
            "a popover the host closed shut"
        );
    }
}
