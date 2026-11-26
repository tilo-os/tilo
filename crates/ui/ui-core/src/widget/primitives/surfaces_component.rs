//! [`ModalDialog`], [`FloatingPanel`] and [`Popover`] as described components.

use vello::kurbo::{Point, Rect};

use tilo_ui_base::{component::Component, component::Described, component::Event};
use tilo_ui_base::widget::DrawChild;
use crate::widget::modal_dialog::DialogAction;
use crate::{widget::Button, widget::Container, widget::FloatingPanel, widget::ModalDialog, widget::PanelAlign, widget::Popover, widget::PopoverSide};
use tilo_ui_base::{widget::Element, widget::Widget};

fn column(views: Vec<Element>) -> Container {
    let mut stack = crate::widget::arrange::Stack::stack()
        .fill_width()
        .gap(tilo_ui_base::tokens::Rhythm::Related);
    for view in views {
        stack = stack.push(view);
    }
    Container::new()
        .with_no_padding()
        .with_child(Box::new(stack))
}

/// A question over the window: a dimmed page, a sheet with a title, the content and up to three
/// answers; Escape or the scrim closes it, and Enter fires the main answer, or the safe one
/// when it destroys something.
#[derive(Component, Default)]
#[component(name = "ModalDialog", role = "Dialog", label = "title")]
pub struct ModalProps {
    /// What the question is, as its title (name the action: "Delete 3 notes?").
    #[component(required)]
    pub title: String,
    /// A glyph before the title naming what asks.
    pub icon: Option<tilo_ui_base::icons::Icon>,
    /// What assistive technology says after the title.
    pub description: Option<String>,
    /// Whether it is open; a closed dialog paints nothing and takes no input.
    pub open: bool,
    /// The main answer's label ("Delete"): the inverted button, and what Enter fires.
    pub primary: Option<String>,
    /// The other answer's label, usually Cancel. Escape fires it.
    pub secondary: Option<String>,
    /// A third answer's label, set apart ("Don't save"); Enter never fires it.
    pub tertiary: Option<String>,
    /// The main answer destroys something: it is a destructive button, and Enter is the safe
    /// answer instead.
    pub destructive: bool,
    /// The main answer cannot be taken yet, so it is disabled.
    pub primary_enabled: bool,
    /// The view's content, inside the sheet.
    #[component(part(default))]
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
    /// It was closed (Escape, the scrim, or Cancel): a dialog with this closes as it is.
    #[component(event)]
    pub dismiss: Event,
}

pub struct DeclaredModal {
    dialog: ModalDialog<Container>,
}

impl DeclaredModal {
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

    fn sync(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) {
        self.dialog.sync(state, path);
    }

    fn update(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) -> bool {
        self.dialog.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.dialog.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.dialog.apply_layout(taffy, node, origin);
    }

    fn draw_overlay(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.dialog.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.dialog.draw_child(scene, transform, text, theme, &env);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        self.dialog.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.dialog.children()
    }

    /// The dialog's content is laid out in the top layer even while the wrapper's children
    /// hide it: the walk follows the dialog's own.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        tilo_ui_base::widget::Widget::for_each_layout_child(&self.dialog, visitor)
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.dialog.children_mut()
    }

    /// It is the dialog it hands on, shown or not: it asks layout exactly when the dialog does.
    fn is_layout_dirty(&self) -> bool {
        Widget::is_layout_dirty(&self.dialog)
    }

    fn layout_needs_measure(&self) -> bool {
        Widget::layout_needs_measure(&self.dialog)
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

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
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
        dialog.set_primary_check(None);
        dialog.set_primary_enabled(primary_enabled);
        dialog.set_on_dismiss((!dismiss.is_none()).then(|| {
            let close = dismiss.clone();
            Box::new(move || close.fire(None)) as Box<dyn FnMut()>
        }));
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn tilo_ui_base::component::PartViews> {
        if part != "body" {
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

/// A floating panel with a header part, a body and a footer part, which a view rarely places
/// itself: a menu is a `Popover` and a question a `ModalDialog`.
#[derive(Component, Default)]
#[component(name = "FloatingPanel", role = "Group")]
pub struct FloatingPanelProps {
    /// The panel's body.
    #[component(part(default))]
    pub body: Vec<Element>,
    /// Its header, above the body; a field there renders embedded.
    #[component(part)]
    pub header: Option<Element>,
    /// Its footer, under the body.
    #[component(part)]
    pub footer: Option<Element>,
    /// A fixed width, in logical pixels (a utility panel).
    pub width: Option<f64>,
    /// Take the height it is offered rather than the height of its content: a panel a surface
    /// measured a room for, whose body scrolls inside it when it holds more.
    pub fill_height: bool,
    /// Take the width it is offered rather than the width of its content: a card that spans a
    /// narrow display.
    pub fill: bool,
    /// What assistive technology calls it; a panel that names none adds no node of its own.
    pub label: Option<String>,
    /// What assistive technology says after its name.
    pub description: Option<String>,
    /// It holds the screen while it shows: what it covers is dimmed, fading with the panel, and
    /// it reads as a modal dialog.
    pub modal: bool,
    /// It arrives from its trailing edge on the panel slide, as a banner rises in the corner.
    pub slide: bool,
    /// Where it stands, from the top-leading corner of what holds it, in logical pixels: its
    /// left edge. With `y`, the panel stands there rather than where its siblings put it.
    pub x: Option<f64>,
    /// Its top edge, from the top of what holds it, in logical pixels.
    pub y: Option<f64>,
    /// The person dragged it by its header and let it go: carries where it settled, its left
    /// and top edge from the top-leading corner of what holds it, onto an edge it was let go
    /// near and inside what holds it. A panel with this moves when its header is dragged.
    #[component(event(value = Vec<i64>))]
    pub moved: Event,
    /// The person drew its trailing edge out or in and let it go: carries the width it rests at,
    /// in logical pixels. A panel with this resizes when that edge is dragged, following it.
    #[component(event(value = f64))]
    pub resized: Event,
    /// A press landed off the panel, on nothing of its own: a panel with this is dismissed by it.
    #[component(event)]
    pub dismiss: Event,
}

/// The place `x` and `y` say, when they both do.
fn place_of(x: Option<f64>, y: Option<f64>) -> Option<Point> {
    Some(Point::new(x?, y?))
}

pub struct DeclaredPanel {
    panel: FloatingPanel,
    rect: Rect,
    /// The pointer as the panel last heard it: a press off the panel is read against it.
    cursor: Point,
    /// What a press off the panel does (`dismiss`).
    dismiss: Event,
    /// The one-view part being changed ([`Described::part_views`]): the header or the footer.
    editing: PanelPart,
}

#[derive(Clone, Copy)]
enum PanelPart {
    Header,
    Footer,
}

impl DeclaredPanel {
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
            panel = panel.width(tilo_ui_base::num::f32_of_f64(width));
        }
        if props.fill_height {
            panel = panel.fill_height();
        }
        panel = panel
            .named(props.label, props.description)
            .modal(props.modal)
            .slide(props.slide);
        if props.fill {
            panel = panel.fill_width();
        }
        panel = panel.at(place_of(props.x, props.y));
        if !props.moved.is_none() {
            let moved = props.moved.clone();
            panel = panel.movable(move |at| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a rounded place on a display, far inside i64"
                )]
                let edge = |at: f64| tilo_ui_base::component::Value::Int(at.round() as i64);
                moved.fire(Some(tilo_ui_base::component::Value::List(vec![
                    edge(at.x),
                    edge(at.y),
                ])));
            });
        }
        if !props.resized.is_none() {
            let resized = props.resized.clone();
            panel = panel.resizable(move |width| {
                resized.fire(Some(tilo_ui_base::component::Value::Float(width)));
            });
        }
        Self {
            panel,
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            dismiss: props.dismiss,
            editing: PanelPart::Header,
        }
    }

    fn part(&mut self) -> &mut Option<Element> {
        match self.editing {
            PanelPart::Header => self.panel.header_mut(),
            PanelPart::Footer => self.panel.footer_mut(),
        }
    }

    fn put(&mut self, mut view: Element) {
        if matches!(self.editing, PanelPart::Header) {
            FloatingPanel::embed_header(view.as_mut());
        }
        *self.part() = Some(view);
    }

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

    fn sync(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) {
        self.panel.sync(state, path);
    }

    fn update(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) -> bool {
        self.panel.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.panel.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        self.panel.apply_layout(taffy, node, origin);
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.panel.draw_child(scene, transform, text, theme, &env);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = ctx.logical(position);
        }
        self.panel.handle_event(event, ctx);
        // A press the panel and everything in it left alone is off it, on nothing of its own: a
        // panel with a dismiss reports it, and the press reaches nothing behind it.
        if !*ctx.handled
            && !self.dismiss.is_none()
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && !self.rect.contains(self.cursor)
        {
            self.dismiss.fire(None);
            *ctx.handled = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.panel.children()
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        tilo_ui_base::widget::Widget::for_each_layout_child(&self.panel, visitor)
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

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        self.panel.accessibility()
    }
}

impl Described for DeclaredPanel {
    type Props = FloatingPanelProps;

    fn from_props(_key: &str, props: FloatingPanelProps) -> Self {
        Self::new(props)
    }

    /// The panel's width changes where it stands; its body, header and footer change by key
    /// through [`Self::part_views`], so a field in the header keeps what was typed in it.
    fn set_props(&mut self, props: FloatingPanelProps) -> bool {
        let FloatingPanelProps {
            width,
            fill,
            fill_height: _,
            label,
            description,
            modal,
            slide,
            body: _,
            header: _,
            footer: _,
            x,
            y,
            moved,
            resized,
            dismiss,
        } = props;
        // Whether it moves or resizes is how it was built: a panel that starts or stops is built
        // again.
        if moved.is_none() == self.panel.is_movable()
            || resized.is_none() == self.panel.is_resizable()
        {
            return false;
        }
        let panel = &mut self.panel;
        panel.set_width(width.map(tilo_ui_base::num::f32_of_f64));
        panel.set_fill_width(fill);
        panel.set_named(label, description);
        panel.set_modal(modal);
        panel.set_slide(slide);
        panel.set_place(place_of(x, y));
        self.dismiss = dismiss;
        true
    }

    /// Each part's views change by key in place, so a change of what the body or footer
    /// shows keeps the header's field, and with it focus and what is being typed.
    fn part_views(&mut self, part: &str) -> Option<&mut dyn tilo_ui_base::component::PartViews> {
        match part {
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
                self.editing = PanelPart::Header;
                Some(self)
            }
            "footer" => {
                self.editing = PanelPart::Footer;
                Some(self)
            }
            _ => None,
        }
    }
}

impl tilo_ui_base::component::PartViews for DeclaredPanel {
    fn len(&self) -> usize {
        let part = match self.editing {
            PanelPart::Header => self.panel.has_header(),
            PanelPart::Footer => self.panel.has_footer(),
        };
        usize::from(part)
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index != 0 {
            return None;
        }
        self.part().as_mut()
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.put(view);
    }

    fn remove(&mut self, _index: usize) {
        *self.part() = None;
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.put(view);
    }
}

/// A menu or a small form anchored beside its control, with the view's content inside it.
#[derive(Component, Default)]
#[component(name = "Popover", role = "Group", label = "label")]
pub struct PopoverProps {
    /// Its accessible name (a menu says what it chooses: "Refresh interval").
    pub label: Option<String>,
    /// Whether it is open.
    pub open: bool,
    /// The side of its anchor it prefers: `below` unless said.
    pub side: Option<PopoverSide>,
    /// The point it stands at, `[x, y]` from the top-left of where it is declared: where a press
    /// raised it (a question an action asks first). Its primary button stands under that point,
    /// so pressing again at the same spot takes it.
    pub at: Option<Vec<f64>>,
    /// It was closed (Escape, or a press outside it): a popover with this closes as it is.
    #[component(event)]
    pub dismiss: Event,
    /// Its content.
    #[component(part(default))]
    pub content: Vec<Element>,
}

pub struct DeclaredPopover {
    popover: Popover<Container>,
    rect: Rect,
    /// The point it stands at, from where it is declared ([`PopoverProps::at`]).
    at: Option<Point>,
}

/// The point `[x, y]` a popover is declared at; `None` for anything else.
fn point_of(at: Option<&[f64]>) -> Option<Point> {
    match at? {
        [x, y] => Some(Point::new(*x, *y)),
        _ => None,
    }
}

impl DeclaredPopover {
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
            at: point_of(props.at.as_deref()),
        }
    }

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

    fn sync(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) {
        self.popover.sync(state, path);
    }

    fn update(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) -> bool {
        self.popover.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.popover.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        // Declared at a point of where it stands: that point in the window.
        let aim = self.at.map(|at| self.rect.origin() + at.to_vec2());
        self.popover.set_aim(aim);
        self.popover.apply_layout(taffy, node, origin);
    }

    fn is_layout_dirty(&self) -> bool {
        self.popover.is_layout_dirty()
    }

    fn draw_overlay(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.popover.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.popover.draw_child(scene, transform, text, theme, &env);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        self.popover.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.popover.children()
    }

    /// A popover's content is laid out open or closed (so reopening finds its nodes): the
    /// walk follows the popover's own.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        tilo_ui_base::widget::Widget::for_each_layout_child(&self.popover, visitor)
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

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
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
            at,
            dismiss,
            content: _,
        } = props;
        self.at = point_of(at.as_deref());
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

    fn part_views(&mut self, part: &str) -> Option<&mut dyn tilo_ui_base::component::PartViews> {
        if part != "content" {
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

/// A button that opens a popover anchored to itself (a filter's options, a share sheet); a
/// second press, Escape or a press outside closes it.
#[derive(Component, Default)]
#[component(name = "PopoverButton", role = "Button", label = "label")]
pub struct PopoverButtonProps {
    /// What the button is called, as it is announced.
    #[component(required, positional)]
    pub label: String,
    /// A glyph before the label, or instead of it when `icon_only`.
    pub icon: Option<tilo_ui_base::icons::Icon>,
    /// The glyph alone, with the label as its tooltip and accessible name.
    pub icon_only: bool,
    /// The side of the button it opens on: `below` unless said.
    pub side: Option<PopoverSide>,
    /// Whether it opens at the button's trailing edge, not its leading edge.
    pub trailing: bool,
    /// The popover's content.
    #[component(part(default))]
    pub content: Vec<Element>,
}

/// The button a popover opens from: its label after its glyph, or the glyph alone.
fn face(key: &str, label: &str, icon: Option<tilo_ui_base::icons::Icon>, icon_only: bool) -> Button {
    match icon {
        Some(icon) if icon_only => Button::icon_only(key, icon, label),
        Some(icon) => Button::new(key, label).with_icon(icon),
        None => Button::new(key, label),
    }
}

pub struct DeclaredPopoverButton {
    pair: crate::widget::PopoverButton<Container>,
}

impl DeclaredPopoverButton {
    #[must_use]
    pub fn new(key: &str, mut props: PopoverButtonProps) -> Self {
        let content = column(std::mem::take(&mut props.content));
        let mut pair = face(key, &props.label, props.icon, props.icon_only).with_popover(content);
        if let Some(side) = props.side {
            pair = pair.side(side);
        }
        if props.trailing {
            pair = pair.aligned(crate::widget::PanelAlign::Trailing);
        }
        Self { pair }
    }

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

    fn sync(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) {
        self.pair.sync(state, path);
    }

    fn update(&mut self, state: &tilo_ui_base::widget::AppModel, path: &tilo_ui_base::widget::ModelPath) -> bool {
        self.pair.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &tilo_ui_base::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.pair.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.pair.apply_layout(taffy, node, origin);
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.pair.draw_child(scene, transform, text, theme, &env);
    }

    fn draw_overlay(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.pair.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
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
        tilo_ui_base::widget::Widget::for_each_layout_child(&self.pair, visitor)
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

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
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
            icon_only,
            side,
            trailing,
            content: _,
        } = props;
        let words = face("", &label, icon, icon_only);
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

    fn part_views(&mut self, part: &str) -> Option<&mut dyn tilo_ui_base::component::PartViews> {
        if part != "content" {
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

    /// A press off a panel that reports one is a dismissal; a press on the panel, even on its own
    /// empty ground, is none.
    #[test]
    fn a_press_off_a_panel_with_a_dismiss_reports_it_and_one_on_it_does_not() {
        use tilo_ui_base::component::Component;
        use tilo_ui_base::{component::testing::Node, component::testing::fired};
        use crate::testing::harness;
        let queue = tilo_ui_base::Intents::default();
        let mut node = Node::new("FloatingPanel", "panel", &queue)
            .prop("width", tilo_ui_base::component::Value::Float(200.0))
            .on("dismiss");
        let props = FloatingPanelProps::from_source(&mut node).expect("the panel reads");
        let mut host = harness(DeclaredPanel::from_props("panel", props));
        host.relayout();
        let rect = host.root::<DeclaredPanel>().get_rect();

        host.hover_at(rect.x1 + 60.0, rect.y1 + 60.0);
        host.click_at(rect.x1 + 60.0, rect.y1 + 60.0);
        assert_eq!(queue.take(), [fired("panel", "dismiss", None)]);

        let (x, y) = (
            f64::midpoint(rect.x0, rect.x1),
            f64::midpoint(rect.y0, rect.y1),
        );
        host.hover_at(x, y);
        host.click_at(x, y);
        assert!(
            queue.take().is_empty(),
            "a press on the panel dismisses nothing"
        );
    }

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
