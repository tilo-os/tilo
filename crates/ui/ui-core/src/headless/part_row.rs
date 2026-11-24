//! The app's window-bar parts in a row of the frame's own, as a window of its own shows them outside a
//! Tilo session. A browser page that mounts one app is such a window: no shell draws a bar over it,
//! so nothing would show the parts the app declares ([`crate::window_toolbar`]), and an app whose
//! controls are its parts would have none.

use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use super::{FrameHarness, InspectionNode};
use crate::app_core::KeyCore as _;
#[cfg(test)]
use crate::part_row::WindowAction;
use crate::part_row::{PartRow, PartRowHost, RowInput, RowPlace};
use crate::widget::search_request::SearchRequest;
use crate::widget::{HistoryControl, InPlace, Widget};
use crate::window_toolbar::{SearchOffer, SearchOpening, ToolbarEvent};

/// The name the row goes by in the frame's inspection and in its semantic actions: its parts are
/// performed on this owner with their targets.
pub const WINDOW_PARTS: &str = "window-parts";

/// The row the frame hosts, and what it showed when last refreshed.
pub(super) struct HostedParts {
    row: PartRow,
    inset: f64,
    revision: u64,
}

/// The frame lent to its row: the app the row serves.
struct Lent<'a> {
    frame: &'a mut FrameHarness,
    search: Option<SearchRequest>,
}

impl PartRowHost for Lent<'_> {
    fn search_root(&mut self) -> &mut dyn Widget {
        self.frame.root.as_mut()
    }

    fn search_offer(&mut self) -> InPlace {
        SearchOffer::of(self.frame.root.as_mut()).into()
    }

    fn open_app_search(&mut self, request: SearchRequest) {
        self.search = Some(request);
    }

    fn search_drove(&mut self) {
        self.frame.invalidate_local();
    }

    fn takes_search(&mut self, opening: SearchOpening) -> bool {
        let offer = SearchOffer::of(self.frame.root.as_mut());
        match opening {
            SearchOpening::Find => offer != SearchOffer::Nothing,
            SearchOpening::Replace => offer == SearchOffer::Document,
        }
    }

    fn history_control(&self) -> Option<HistoryControl> {
        self.frame.window_history_control()
    }

    fn press_history(&mut self, control: HistoryControl) {
        if let Err(error) = crate::history_mode::host::press_offered(
            self.frame.root.as_mut(),
            self.frame.history.as_deref_mut(),
            control,
        ) {
            tracing::warn!(%error, "window parts: History did not run");
        }
        self.frame.invalidate_local();
    }

    fn deliver(&mut self, event: ToolbarEvent) {
        if crate::window_toolbar::deliver_to_app(event) {
            self.frame.hand_keyboard_to_app();
        }
    }

    fn parts_acted(&mut self) {
        self.frame.invalidate_local();
    }
}

impl FrameHarness {
    /// Host the app's window-bar parts in a row across the top of the frame, as a window of its own
    /// outside a Tilo session does (a browser page that mounts one app): the app is told its parts,
    /// search and History are hosted, its frame is laid out under the row, and the row is painted,
    /// routed and inspected with it.
    pub fn host_window_parts(&mut self) {
        self.invalidate_local();
        if self.window_bar || self.part_row.is_some() {
            return;
        }
        // The row's parts are this window's, as the shell bar's are
        // ([`Self::present_in_window_bar`]): a process laying out several windows keeps each
        // row to its own window.
        self.host_bar_window();
        let _running = self.running();
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            history.set_open_button(false);
            history.set_window_presents(true);
        }
        // A window shows one bar: the app's own header row is offered the window's bar, and a row
        // that draws it is the window's top, with no row of the frame's over it. A page draws no
        // window furniture, so the bar it offers has none: the row keeps its actions rather than
        // hiding one behind an overflow nothing else reaches.
        let bar = crate::widget::WindowBar {
            controls_width: 0.0,
            lists_actions: false,
        };
        crate::widget::window_header::offer_root_header_bar(self.root.as_mut(), Some(bar));
        let frame = self.content_viewport(self.viewport);
        if let Err(error) = self.resize(frame) {
            tracing::warn!(%error, "window parts: the frame under the bar did not lay out");
        }
        self.refresh_row_holds();
        // A window shows one bar: an app whose own header row draws it — its shell's — is the
        // window's top. So the app keeps its parts in that row, and the window's notice stands on
        // a card over the frame.
        if self.row_holds_bar()
            || crate::widget::window_header::draws_a_header_row(self.root.as_ref())
        {
            if let Some(window) = self.window.as_ref() {
                window.set_app_draws_bar(true);
            }
            crate::widget::window_header::offer_window_search(self.root.as_mut(), false);
            self.part_row = Some(Box::new(HostedParts {
                row: PartRow::notice_card(),
                inset: 0.0,
                revision: crate::window_toolbar::revision().wrapping_sub(1),
            }));
            self.invalidate_layout_tree();
            self.relayout();
            return;
        }
        // A frame whose row does not draw the bar keeps the app's parts in a row of its own.
        self.part_row = Some(Box::new(HostedParts {
            row: PartRow::new(),
            inset: 0.0,
            revision: crate::window_toolbar::revision().wrapping_sub(1),
        }));
        self.invalidate_layout_tree();
        self.relayout();
    }

    /// Whether the frame hosts the app's window-bar parts in a row of its own
    /// ([`Self::host_window_parts`]).
    #[must_use]
    pub fn hosts_window_parts(&self) -> bool {
        self.parts_row_shown()
    }

    /// Whether a row of the app's parts stands over its frame, rather than only a notice card over
    /// an app whose own header row is the window's bar: only such a row presents search.
    pub(super) fn parts_row_shown(&self) -> bool {
        self.part_row
            .as_ref()
            .is_some_and(|parts| !parts.row.is_notice_card())
    }

    /// The frame is a window of its own on a shell that leaves the frame to the app
    /// ([`Self::host_window_parts`]): its row carries the window's minimize, maximize and close in
    /// their pill at its trailing end, as the native runner's row does, and a press on the row's empty
    /// ground moves the window.
    pub fn host_window_controls(&mut self) {
        self.invalidate_local();
        let Some(hosted) = self.part_row.as_mut() else {
            return;
        };
        hosted.row.set_window_controls(true);
        self.relayout();
    }

    /// Whether the frame's row carries the window's controls ([`Self::host_window_controls`]).
    #[must_use]
    pub fn hosts_window_controls(&self) -> bool {
        self.part_row
            .as_ref()
            .is_some_and(|hosted| hosted.row.shows_window_controls())
    }

    /// How far below the window's top the app's frame starts: the row's height while the frame hosts
    /// the parts.
    pub(super) fn part_row_inset(&self) -> f64 {
        self.part_row.as_ref().map_or(0.0, |hosted| hosted.inset)
    }

    /// Run `serve` with the row and the frame lent to it, then open the search of the app's own it
    /// asked for.
    fn with_part_row<R>(
        &mut self,
        serve: impl FnOnce(&mut PartRow, &mut Lent<'_>) -> R,
    ) -> Option<R> {
        let mut hosted = self.part_row.take()?;
        let mut lent = Lent {
            frame: self,
            search: None,
        };
        let result = serve(&mut hosted.row, &mut lent);
        let search = lent.search.take();
        self.part_row = Some(hosted);
        if let Some(request) = search
            && let Err(error) = self.open_search(&request)
        {
            tracing::warn!(%error, "window parts: the app's search did not open");
        }
        Some(result)
    }

    /// Keep the row current before a frame lays out: what the app declares, the window's History and
    /// width. Returns whether the row changed, so a frame already drawn must be drawn again.
    pub(super) fn refresh_part_row(&mut self) -> bool {
        if self.part_row.is_none() {
            return false;
        }
        self.with_part_row(|row, lent| row.serve_app_requests(lent));
        let title = self.window_tab().title;
        let history = self.window_history_control();
        let window = (f64::from(self.viewport.0), f64::from(self.viewport.1));
        let revision = crate::window_toolbar::revision();
        let Some(hosted) = self.part_row.as_mut() else {
            return false;
        };
        hosted.revision = revision;
        let height = hosted.row.refresh(
            &self.theme,
            Some(RowPlace {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                window,
                app_name: &title,
            }),
            history,
        );
        let mut changed = hosted.row.take_repaint();
        if (hosted.inset - height).abs() > f64::EPSILON {
            hosted.inset = height;
            changed = true;
            self.invalidate_layout_tree();
        }
        self.with_part_row(|row, lent| row.drive_search(lent));
        if changed {
            self.invalidate_local();
        }
        changed
    }

    /// Whether the row asks for a frame: the app declared other parts since it last showed them, or a
    /// menu or the search's caret moves.
    pub(super) fn part_row_wants_frame(&self) -> (bool, bool) {
        self.part_row.as_ref().map_or((false, false), |hosted| {
            (
                hosted.revision != crate::window_toolbar::revision(),
                hosted.row.needs_frame(),
            )
        })
    }

    /// Paint the row over the app's frame, its menus and the search's options over the app.
    pub(super) fn draw_part_row(&mut self, transform: Affine) {
        let Some(hosted) = self.part_row.as_mut() else {
            return;
        };
        if !hosted.row.is_shown() {
            return;
        }
        let window = Rect::new(
            0.0,
            0.0,
            f64::from(self.viewport.0),
            f64::from(self.viewport.1),
        );
        let bounds = hosted.row.paint_bounds(0.0, &self.theme, window);
        let (scene, text, theme, presence) =
            (&mut self.scene, &mut self.text, &self.theme, &self.presence);
        crate::scene_parts::part(
            scene,
            crate::scene_parts::PartKey::new("window-parts", 0),
            vec![bounds],
            |scene| {
                hosted.row.draw(
                    &mut crate::draw::DrawCtx::new(
                        scene,
                        transform,
                        text,
                        theme,
                        presence.draw_env(),
                    ),
                    0.0,
                )
            },
        );
    }

    /// Hand `event` to the row when it is the row's, before the app sees it. Returns whether the row
    /// took it.
    pub(super) fn part_row_event(&mut self, event: &WindowEvent) -> bool {
        if self.part_row.is_none() {
            return false;
        }
        let modifiers = self.modifiers;
        let input = RowInput {
            scale: self.input_scale,
            modifiers: &modifiers,
            platform: self.platform,
        };
        let Some((took, redraw)) = self.with_part_row(|row, lent| {
            let routed = row.route(lent, event, &input);
            let _ = row.take_window_actions();
            routed
        }) else {
            return false;
        };
        if took || redraw {
            self.invalidate_local();
            if self.coalesce_input_frames {
                self.input_frame_pending = true;
            } else {
                self.relayout();
            }
        }
        took
    }

    /// The row as the frame's inspection shows it: one toolbar of the app's parts and the window's
    /// search, or the notice's card alone where the app's own row is the bar.
    pub(super) fn part_row_inspection(&self) -> Option<InspectionNode> {
        let hosted = self.part_row.as_ref()?;
        if !hosted.row.is_shown() {
            return None;
        }
        if hosted.row.is_notice_card() {
            return self.notice_card_inspection(hosted);
        }
        let window = Rect::new(0.0, 0.0, f64::from(self.viewport.0), hosted.inset);
        Some(InspectionNode {
            path: WINDOW_PARTS.into(),
            widget: "WindowParts".into(),
            test_id: None,
            focusable: false,
            rect: [window.x0, window.y0, window.width(), window.height()],
            child_clip: None,
            page_title: None,
            layout: None,
            paint_key: None,
            semantics: Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Toolbar)
                    .with_name(self.window_tab().title)
                    .with_bounds(window)
                    .with_children(hosted.row.accessibility()),
            ),
            children: Vec::new(),
        })
    }

    fn notice_card_inspection(&self, hosted: &HostedParts) -> Option<InspectionNode> {
        let card = hosted.row.strip_card(&self.theme)?;
        let nodes = hosted.row.accessibility();
        let buttons = nodes
            .iter()
            .filter(|node| node.action_target.is_some() && node.role == crate::a11y::Role::Button)
            .map(|node| {
                let rect = node.bounds.map_or(Rect::ZERO, |bounds| bounds.to_rect());
                InspectionNode {
                    path: format!(
                        "{WINDOW_PARTS}#{}",
                        node.action_target.as_deref().unwrap_or_default()
                    ),
                    widget: "WindowPart".into(),
                    test_id: None,
                    focusable: node.state.focusable,
                    rect: [rect.x0, rect.y0, rect.width(), rect.height()],
                    child_clip: None,
                    page_title: None,
                    layout: None,
                    paint_key: None,
                    semantics: Some(node.clone()),
                    children: Vec::new(),
                }
            })
            .collect();
        Some(InspectionNode {
            path: WINDOW_PARTS.into(),
            widget: "WindowNoticeStrip".into(),
            test_id: None,
            focusable: false,
            rect: [card.x0, card.y0, card.width(), card.height()],
            child_clip: None,
            page_title: None,
            layout: None,
            paint_key: None,
            semantics: nodes.into_iter().find(|node| node.live.is_some()),
            children: buttons,
        })
    }

    /// The row as a page shows it: a toolbar across the top of the window, its paint with each
    /// part it names acting where it stands — or the notice's card alone where the app's own row
    /// is the bar.
    pub(super) fn part_row_composition(&self) -> Option<crate::composition::Composed> {
        use crate::composition::{Composed, Picture};
        let hosted = self.part_row.as_ref()?;
        if !hosted.row.is_shown() {
            return None;
        }
        let nodes = hosted.row.accessibility();
        if hosted.row.is_notice_card() {
            let card = hosted.row.strip_card(&self.theme)?;
            let mut strip = Composed::new(
                format!("{WINDOW_PARTS}#strip"),
                "WindowNoticeStrip",
                [card.x0, card.y0, card.width(), card.height()],
            );
            strip.semantics = nodes
                .into_iter()
                .find(|node| node.live.is_some() && node.role == crate::a11y::Role::StaticText);
            return Some(strip);
        }
        let window = Rect::new(0.0, 0.0, f64::from(self.viewport.0), hosted.inset);
        let mut row = Composed::new(
            WINDOW_PARTS.into(),
            "WindowParts",
            [window.x0, window.y0, window.width(), window.height()],
        );
        row.semantics = Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Toolbar)
                .with_name(self.window_tab().title)
                .with_bounds(window)
                .with_children(nodes),
        );
        row.picture = Some(Picture::default());
        Some(row)
    }

    /// The row's band alone, at `scale`, for its picture.
    pub(super) fn paint_part_row(&mut self, scale: f64) -> Option<(vello::Scene, [f64; 4])> {
        let hosted = self.part_row.as_mut()?;
        let width = f64::from(self.viewport.0);
        let mut scene = vello::Scene::new();
        hosted.row.draw(
            &mut crate::draw::DrawCtx::new(
                &mut scene,
                Affine::scale(scale),
                &mut self.text,
                &self.theme,
                self.presence.draw_env(),
            ),
            0.0,
        );
        Some((scene, [0.0, 0.0, width, hosted.inset]))
    }

    /// The window's own actions pressed through the row's semantics since the last call.
    #[cfg(test)]
    pub(super) fn take_part_row_window_actions(&mut self) -> Vec<WindowAction> {
        self.with_part_row(|row, _| row.take_window_actions())
            .unwrap_or_default()
    }

    /// Perform `action` on the row's part `target` (a `window-toolbar-*` part or a `window-search-*`
    /// part), as assistive technology and automation do, and show what the app made of it, as a press
    /// on the part does.
    pub(super) fn perform_part_row_semantic(
        &mut self,
        target: Option<&str>,
        action: &crate::a11y::Action,
    ) -> Result<(), String> {
        let Some(target) = target else {
            return Ok(());
        };
        let ran = self
            .with_part_row(|row, lent| row.perform(lent, target, action))
            .ok_or("this frame hosts no window parts")?;
        if !ran {
            return Err(format!("the window part {target} took no {action:?} now"));
        }
        self.relayout();
        self.host_tick();
        self.invalidate_local();
        self.relayout();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
    use crate::window_toolbar::{ToolbarAction, WindowToolbar};
    use std::cell::RefCell;
    use std::rc::Rc;

    const MUTE: &str = "window-toolbar-action:call-mute";

    /// An app whose one control is a part in the window's bar: a call's microphone, named for what a
    /// press does, as Chat declares it.
    struct CallApp {
        content: crate::widget::Column,
        muted: bool,
        heard: Rc<RefCell<Vec<ToolbarEvent>>>,
    }

    impl crate::widget::Decorator for CallApp {
        type Inner = crate::widget::Column;

        fn inner(&self) -> &Self::Inner {
            &self.content
        }

        fn inner_mut(&mut self) -> &mut Self::Inner {
            &mut self.content
        }

        fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            for event in crate::window_toolbar::take_events() {
                if event
                    == (ToolbarEvent::Action {
                        id: "call-mute".into(),
                    })
                {
                    self.muted = !self.muted;
                }
                self.heard.borrow_mut().push(event);
            }
            let name = if self.muted { "Unmute" } else { "Mute" };
            crate::window_toolbar::declare(Some(WindowToolbar {
                actions: vec![ToolbarAction::new("call-mute", "mic", name)],
                location: Some(crate::window_toolbar::ToolbarLocation {
                    label: "Call".into(),
                    ..crate::window_toolbar::ToolbarLocation::default()
                }),
                ..WindowToolbar::default()
            }));
            self.content.sync(state, path);
        }
    }

    fn call_app() -> (FrameHarness, Rc<RefCell<Vec<ToolbarEvent>>>) {
        let heard = Rc::new(RefCell::new(Vec::new()));
        let app = CallApp {
            content: crate::widget::Column::new()
                .push(Box::new(crate::widget::Button::new("send", "Send"))),
            muted: true,
            heard: Rc::clone(&heard),
        };
        let mut harness = FrameHarness::for_root(
            crate::State::new(),
            app,
            (900, 600),
            crate::theme::Theme::dark().with_reduce_motion(true),
        );
        harness.host_window_parts();
        harness.relayout();
        (harness, heard)
    }

    /// The app whose own header row draws the window's top takes no row of parts above it, so
    /// the window shows one bar.
    #[test]
    fn a_header_row_that_holds_the_bar_takes_the_row_of_parts_with_it() {
        use crate::widget::NavShell;
        let shell = NavShell::new()
            .content_kind(crate::widget::ContentKind::Page)
            .sidebar(Box::new(crate::widget::Button::new("one", "One")))
            .content(Box::new(crate::widget::Button::new("two", "Two")))
            .sidebar_header("Places", Vec::new())
            .content_header("A page", Vec::new());
        let mut harness = FrameHarness::for_root(
            crate::State::new(),
            shell,
            (900, 600),
            crate::theme::Theme::dark().with_reduce_motion(true),
        );
        harness.host_window_parts();
        harness.relayout();
        assert!(
            !harness.hosts_window_parts(),
            "no row of parts stands over the header row the shell draws"
        );
    }

    /// A page's app whose own header row is the window's bar keeps its search in that row, at
    /// every rebuild: nothing else on the page presents one.
    #[test]
    fn an_apps_own_bar_keeps_its_search() {
        use crate::widget::NavShell;
        use crate::widget::search_request::{SearchRequest, search_button};
        let shell = NavShell::new()
            .content_kind(crate::widget::ContentKind::Page)
            .sidebar(Box::new(crate::widget::Button::new("one", "One")))
            .content(Box::new(crate::widget::Button::new("two", "Two")))
            .sidebar_header("Places", Vec::new())
            .content_header(
                "A page",
                vec![Box::new(search_button(SearchRequest::this_app()))],
            );
        let mut harness = FrameHarness::for_root(
            crate::State::new(),
            shell,
            (1280, 800),
            crate::theme::Theme::dark().with_reduce_motion(true),
        );
        harness.host_window_parts();
        harness.relayout();
        harness.invalidate_layout_tree();
        harness.relayout();
        fn shown(node: &crate::composition::Composed, name: &str) -> bool {
            let named = node
                .semantics
                .as_ref()
                .is_some_and(|semantics| semantics.name.as_deref() == Some(name));
            (named && node.rect[2] > 0.0 && node.rect[3] > 0.0)
                || node.children.iter().any(|child| shown(child, name))
        }
        let name = crate::i18n::I18nManager::lookup_plain("standalone-search-title");
        assert!(
            shown(&harness.composition().root, &name),
            "the row keeps the app's search"
        );
    }

    fn part(harness: &FrameHarness, target: &str) -> Option<crate::a11y::AccessNode> {
        fn find(node: &crate::a11y::AccessNode, target: &str) -> Option<crate::a11y::AccessNode> {
            if node.action_target.as_deref() == Some(target) {
                return Some(node.clone());
            }
            node.children.iter().find_map(|child| find(child, target))
        }
        let inspection = harness.inspection();
        let row = inspection
            .root
            .children
            .iter()
            .find(|node| node.path == WINDOW_PARTS)?;
        find(row.semantics.as_ref()?, target)
    }

    #[test]
    fn the_frame_hosts_the_apps_parts_in_a_row_above_its_frame() {
        let (harness, _) = call_app();
        assert!(harness.hosts_window_parts());
        assert!(
            crate::window_toolbar::is_hosted(),
            "the app hears its parts are hosted"
        );
        let frame = Widget::get_rect(harness.root::<CallApp>());
        assert!(
            (frame.y0 - TOOLBAR_ROW_HEIGHT).abs() < f64::EPSILON,
            "the app's frame starts under the row: {frame:?}"
        );
        assert!((frame.height() - (600.0 - TOOLBAR_ROW_HEIGHT)).abs() < f64::EPSILON);
        let mute = part(&harness, MUTE).expect("the microphone stands in the row");
        assert_eq!(mute.name.as_deref(), Some("Unmute"));
        let bounds = mute.bounds.expect("the part says where it stands");
        assert!(bounds.y1 <= TOOLBAR_ROW_HEIGHT && bounds.x1 > bounds.x0);
        assert!(
            part(&harness, "window-search-open").is_some(),
            "the row ends with the window's search"
        );
    }

    #[test]
    fn a_window_of_its_own_has_one_bar_with_the_windows_controls() {
        let (mut harness, _) = call_app();
        assert!(!harness.hosts_window_controls(), "a page hosts no controls");
        harness.host_window_controls();
        assert!(harness.hosts_window_controls());
        let frame = Widget::get_rect(harness.root::<CallApp>());
        assert!(
            (frame.y0 - TOOLBAR_ROW_HEIGHT).abs() < f64::EPSILON,
            "the window has one bar: the app starts under it, not under two: {frame:?}"
        );
        let row = harness
            .inspection()
            .root
            .children
            .into_iter()
            .find(|node| node.path == WINDOW_PARTS)
            .expect("the row stands in the frame");
        assert!(
            (row.rect[3] - TOOLBAR_ROW_HEIGHT).abs() < 0.5,
            "the row is one bar tall: {:?}",
            row.rect
        );
    }

    #[test]
    fn activating_a_part_reaches_the_app_and_the_row_shows_its_outcome() {
        let (mut harness, heard) = call_app();
        harness
            .perform_semantic_named(WINDOW_PARTS, Some(MUTE), &crate::a11y::Action::Activate)
            .expect("the part runs");
        assert_eq!(
            heard.borrow().as_slice(),
            [ToolbarEvent::Action {
                id: "call-mute".into()
            }]
        );
        let mute = part(&harness, MUTE).expect("the microphone stays in the row");
        assert_eq!(
            mute.name.as_deref(),
            Some("Mute"),
            "the row shows the app's new state"
        );
        assert!(
            harness
                .perform_semantic_named(
                    WINDOW_PARTS,
                    Some("window-toolbar-action:missing"),
                    &crate::a11y::Action::Activate
                )
                .is_err(),
            "a part the app does not declare runs nothing"
        );
    }

    // A person's press on the part, where the row paints it, is the same
    // press, and never reaches the app's own controls under the row.
    #[test]
    fn a_pointer_press_on_a_part_reaches_the_app() {
        let (mut harness, heard) = call_app();
        let bounds = part(&harness, MUTE)
            .and_then(|mute| mute.bounds)
            .expect("the part stands in the row");
        let (x, y) = ((bounds.x0 + bounds.x1) / 2.0, (bounds.y0 + bounds.y1) / 2.0);
        harness.send_host_event(&crate::synthetic_input::cursor_moved(x, y));
        harness.send_host_event(&crate::synthetic_input::mouse_press());
        harness.send_host_event(&crate::synthetic_input::mouse_release());
        harness.relayout();
        assert_eq!(
            heard.borrow().as_slice(),
            [ToolbarEvent::Action {
                id: "call-mute".into()
            }]
        );
        assert_eq!(
            part(&harness, MUTE).and_then(|mute| mute.name).as_deref(),
            Some("Mute")
        );
    }

    #[test]
    fn the_windows_controls_are_named_and_run_for_a_reader() {
        let (mut harness, _) = call_app();
        harness.host_window_controls();
        for (target, name) in [
            ("window-control-minimize", "Minimize"),
            ("window-control-maximize", "Maximize"),
            ("window-control-close", "Close"),
        ] {
            let control = part(&harness, target).expect("the control is reachable");
            assert_eq!(control.name.as_deref(), Some(name), "{target}");
            assert!(control.state.focusable, "{target} takes focus");
            let bounds = control.bounds.expect("the control says where it stands");
            assert!(
                bounds.y1 <= TOOLBAR_ROW_HEIGHT && bounds.x1 > bounds.x0,
                "{target} stands in the bar: {bounds:?}"
            );
        }
        harness
            .perform_semantic_named(
                WINDOW_PARTS,
                Some("window-control-close"),
                &crate::a11y::Action::Activate,
            )
            .expect("the control runs");
        assert_eq!(
            harness.take_part_row_window_actions(),
            [WindowAction::Close],
            "the press reaches the window"
        );
    }

    // The bar names what the window is on: a label at the row's location
    // that names no target, because a press on it runs nothing that a press
    // on the row's own background does not. A reader that presses it is
    // told nothing was refused, and no part runs.
    #[test]
    fn a_press_on_the_rows_own_label_runs_nothing_and_is_not_refused() {
        let (mut harness, heard) = call_app();
        let named = |harness: &FrameHarness, name: &str| {
            fn find(node: &crate::a11y::AccessNode, name: &str) -> bool {
                node.name.as_deref() == Some(name)
                    || node.children.iter().any(|child| find(child, name))
            }
            let inspection = harness.inspection();
            inspection
                .root
                .children
                .iter()
                .find(|node| node.path == WINDOW_PARTS)
                .and_then(|row| row.semantics.as_ref())
                .is_some_and(|row| find(row, name))
        };
        assert!(
            named(&harness, "Call"),
            "the call is named in the row: {:?}",
            part(&harness, MUTE).and_then(|mute| mute.name)
        );
        harness
            .perform_semantic_named(WINDOW_PARTS, None, &crate::a11y::Action::Activate)
            .expect("a press on the row's own label is not refused");
        assert!(
            heard.borrow().is_empty(),
            "no part ran: {:?}",
            heard.borrow()
        );
        assert!(harness.take_part_row_window_actions().is_empty());
    }

    /// An app whose bar holds a menu, as a list's sort menu does: the row's button says what the menu
    /// is, and the rows it lists run by their id.
    struct SortApp {
        content: crate::widget::Column,
        heard: Rc<RefCell<Vec<ToolbarEvent>>>,
    }

    impl crate::widget::Decorator for SortApp {
        type Inner = crate::widget::Column;

        fn inner(&self) -> &Self::Inner {
            &self.content
        }

        fn inner_mut(&mut self) -> &mut Self::Inner {
            &mut self.content
        }

        fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            for event in crate::window_toolbar::take_events() {
                self.heard.borrow_mut().push(event);
            }
            crate::window_toolbar::declare(Some(WindowToolbar {
                actions: vec![
                    ToolbarAction::new("sort", "arrow-up-down", "Sort")
                        .labelled()
                        .with_items(vec![
                            ToolbarAction::new("sort~row-0", "", "Everything"),
                            ToolbarAction::new("sort~row-1", "", "Title"),
                        ]),
                ],
                ..WindowToolbar::default()
            }));
            self.content.sync(state, path);
        }
    }

    fn sort_app() -> (FrameHarness, Rc<RefCell<Vec<ToolbarEvent>>>) {
        let heard = Rc::new(RefCell::new(Vec::new()));
        let app = SortApp {
            content: crate::widget::Column::new()
                .push(Box::new(crate::widget::Button::new("send", "Send"))),
            heard: Rc::clone(&heard),
        };
        let mut harness = FrameHarness::for_root(
            crate::State::new(),
            app,
            (900, 600),
            crate::theme::Theme::dark().with_reduce_motion(true),
        );
        harness.host_window_parts();
        harness.relayout();
        (harness, heard)
    }

    #[test]
    fn a_menu_in_a_window_of_its_own_opens_on_one_click_and_stays_open() {
        let (mut harness, heard) = sort_app();
        let button = part(&harness, "window-toolbar-action:sort")
            .and_then(|button| button.bounds)
            .expect("the sort menu stands in the row");
        let (x, y) = ((button.x0 + button.x1) / 2.0, (button.y0 + button.y1) / 2.0);
        harness.send_host_event(&crate::synthetic_input::cursor_moved(x, y));
        harness.send_host_event(&crate::synthetic_input::mouse_press());
        harness.send_host_event(&crate::synthetic_input::mouse_release());
        harness.relayout();
        let rows = |harness: &FrameHarness| {
            let inspection = harness.inspection();
            let row = inspection
                .root
                .children
                .iter()
                .find(|node| node.path == WINDOW_PARTS);
            let mut names = Vec::new();
            fn walk(node: &crate::a11y::AccessNode, names: &mut Vec<String>) {
                if let Some(name) = &node.name {
                    names.push(name.clone());
                }
                for child in &node.children {
                    walk(child, names);
                }
            }
            if let Some(semantics) = row.and_then(|row| row.semantics.as_ref()) {
                walk(semantics, &mut names);
            }
            names
        };
        assert!(
            rows(&harness).iter().any(|name| name == "Title"),
            "one click opens the menu: {:?}",
            rows(&harness)
        );
        for _ in 0..4 {
            harness.relayout();
        }
        assert!(
            rows(&harness).iter().any(|name| name == "Title"),
            "the menu stays open: {:?}",
            rows(&harness)
        );
        assert!(
            heard.borrow().is_empty(),
            "opening the menu runs none of its rows"
        );
    }
}
