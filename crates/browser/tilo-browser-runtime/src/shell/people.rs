//! The people overlay and following, in the page.
//!
//! Pinned out of the People popover, the roster floats over the windows
//! (`tilo_desktop::people_overlay`), waiting at an edge as a desk widget
//! does and kept there in the person's settings as the widgets keep
//! theirs. Its peek draws each person's screen from their live layout,
//! the windows on records this page holds pictured live from its own
//! replica. Following someone brings forward, here, the window of what
//! they are in each time it changes, under the same banner the native
//! shell shows; the person's own input takes their view back.
//!
//! The overlay's code may come in its own module the first time the roster
//! is pinned ([`crate::pinned_roster`]); following and the banner are here
//! from the start.

use settings_store::desk_widgets::{PEOPLE_OVERLAY, PeopleOverlayPrefs};
use tilo_desktop::collaborators::RosterRow;
use tilo_desktop::desk_edges::DockEdge;
use tilo_desktop::follow_banner::{BannerFrame, FollowBanner};
use tilo_desktop::here::Person;
use tilo_desktop::people_overlay::{
    PeekPictures, PeerScreen, PeerWindow, PeopleHit, PeopleOverlayModel,
};
use tilo_desktop_model::{DesktopTree, TabId};
use tilo_desktop_surface::DesktopHandle;
use tilo_relay_protocol::live::LiveRecord;
use ui::kurbo::{Affine, Point, Rect};
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::vello::Scene;
use ui::winit::event::{ElementState, Modifiers, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};
use ui::{TextRenderer, Theme};

use crate::pinned_roster::PinnedRoster;

/// What the overlay and the banner need from the page this frame.
pub(super) struct Frame<'a> {
    /// The page's desktop, the bar included.
    pub rect: Rect,
    pub rows: &'a [RosterRow],
    pub people: &'a [Person],
    pub tree: &'a DesktopTree,
    /// The records this page shows, by the window showing each, as the
    /// live layer names them: `(tab, app, record)`.
    pub records: &'a [(TabId, String, String)],
    pub cursor: Option<Point>,
    /// Where the windows' content starts, below their bars: the banner
    /// hangs there, as the page's notices do, clear of the windows' own
    /// controls.
    pub content_top: f64,
}

/// What following last brought forward.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tracked {
    Window(TabId),
    App(String),
}

/// The overlay, following and the banner across frames.
pub(super) struct PagePeople {
    /// The pinned roster, once its code is in and it was first pinned.
    overlay: Option<Box<dyn PinnedRoster>>,
    /// Whether the roster is pinned (shown once its code is in).
    pinned: bool,
    edge: DockEdge,
    banner: FollowBanner,
    /// Whom this page follows, by roster id.
    followed: Option<String>,
    /// What was last brought forward for them: their window here, or
    /// their app where no window here shows their record.
    tracked: Option<Tracked>,
    motion: EffectiveMotion,
}

impl PagePeople {
    /// As the person's settings left it: pinned or not, at its edge.
    pub(super) fn new() -> Self {
        let prefs = PEOPLE_OVERLAY.get();
        Self {
            overlay: None,
            pinned: prefs.pinned,
            edge: prefs.edge,
            banner: FollowBanner::default(),
            followed: None,
            tracked: None,
            motion: EffectiveMotion::default(),
        }
    }

    pub(super) fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// The roster while pinned: made the first time its code is in (asking
    /// for it until then).
    fn roster(&mut self) -> Option<&mut (dyn PinnedRoster + 'static)> {
        if self.overlay.is_none() && self.pinned {
            let make = crate::pinned_roster::code()?;
            let mut overlay = make(self.edge);
            overlay.show(true);
            self.overlay = Some(overlay);
        }
        self.overlay.as_deref_mut()
    }

    /// The roster as it is now, if made.
    fn made(&self) -> Option<&dyn PinnedRoster> {
        self.overlay.as_deref()
    }

    /// Pin the roster to the desk, or put it back in the bar; kept.
    pub(super) fn pin(&mut self, pinned: bool) {
        self.pinned = pinned;
        if let Some(overlay) = self.overlay.as_deref_mut() {
            overlay.show(pinned);
        }
        if let Err(error) = PEOPLE_OVERLAY.update(|prefs| PeopleOverlayPrefs { pinned, ..prefs }) {
            web_sys::console::warn_1(&format!("keeping the people overlay's pin: {error}").into());
        }
    }

    pub(super) fn followed(&self) -> Option<&str> {
        self.followed.as_deref()
    }

    /// Follow `id`, or stop following them. The banner finds their row in
    /// the next frame's roster.
    pub(super) fn toggle_follow(&mut self, id: &str) {
        if self.followed.as_deref() == Some(id) {
            self.stop_following(false);
            return;
        }
        self.followed = Some(id.to_owned());
        self.tracked = None;
        self.banner
            .started(None, self.motion, MotionSource::Pointer, ui::time::now());
    }

    /// Stop following; `by_own_input` when the person took their view back
    /// by doing something of their own (the banner says so a moment).
    pub(super) fn stop_following(&mut self, by_own_input: bool) {
        if self.followed.take().is_none() {
            return;
        }
        self.tracked = None;
        self.banner.ended(
            by_own_input,
            self.motion,
            MotionSource::Pointer,
            ui::time::now(),
        );
    }

    /// While following: bring forward the window of what they are in, each
    /// time that changes; where no window here shows it (a room whose
    /// layout is each person's own), `open_app` shows their app here.
    /// Returns whether anything was brought forward.
    pub(super) fn track(
        &mut self,
        people: &[Person],
        handle: &DesktopHandle,
        open_app: &dyn Fn(&str) -> bool,
    ) -> bool {
        let Some(id) = self.followed.as_deref() else {
            return false;
        };
        let Some(person) = people.iter().find(|person| person.id == id) else {
            // They left: following them ends with them.
            self.stop_following(false);
            return false;
        };
        let focused_app = person
            .screen
            .as_ref()
            .and_then(|screen| screen.windows.iter().find(|window| window.focused))
            .map(|window| window.app.clone());
        let want = match (person.tab, focused_app) {
            (Some(tab), _) => Tracked::Window(tab),
            (None, Some(app)) => Tracked::App(app),
            (None, None) => return false,
        };
        if self.tracked.as_ref() == Some(&want) {
            return false;
        }
        let brought = match &want {
            Tracked::Window(tab) => handle.focus_tab(*tab),
            Tracked::App(app) => open_app(app),
        };
        self.tracked = Some(want);
        brought
    }

    pub(super) fn needs_animation_frame(&self) -> bool {
        self.made().is_some_and(PinnedRoster::needs_animation_frame)
            || self.banner.is_moving(ui::time::now())
    }

    pub(super) fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        [
            self.made().and_then(PinnedRoster::next_wake_deadline),
            self.banner.next_wake_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Whether `point` is on the overlay, its peek, or the banner.
    pub(super) fn contains(&self, point: Point) -> bool {
        self.made().is_some_and(|overlay| overlay.contains(point))
            || self.banner.chip_rect().contains(point)
    }

    pub(super) fn wants_pointer_capture(&self) -> bool {
        self.made().is_some_and(PinnedRoster::wants_pointer_capture)
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        frame: &Frame<'_>,
    ) {
        self.motion = theme.motion();
        let bar = f64::from(tilo_desktop::topbar::topbar_height(theme));
        let below_bar = Rect::new(
            frame.rect.x0,
            frame.rect.y0 + bar,
            frame.rect.x1,
            frame.rect.y1,
        );
        let following = self
            .followed
            .as_deref()
            .map(|id| frame.rows.iter().find(|row| row.id == id).cloned());
        let kept = self.banner.draw(
            scene,
            transform,
            text,
            theme,
            BannerFrame {
                work_area: below_bar,
                viewport: Rect::new(
                    below_bar.x0,
                    below_bar.y0.max(frame.content_top),
                    below_bar.x1,
                    below_bar.y1,
                ),
                window_bars: &[],
                cursor: frame.cursor,
                motion: self.motion,
            },
            following,
        );
        if !kept {
            self.stop_following(false);
        }
        let followed = self.followed.clone();
        let Some(overlay) = self.roster().filter(|overlay| overlay.is_visible()) else {
            ui::window_pictures::want(&[]);
            return;
        };
        let screens = screens(frame);
        // Live pictures only of the windows the open peek shows.
        let wanted: Vec<u64> = overlay
            .peeked()
            .and_then(|id| screens.iter().find(|screen| screen.id == id))
            .into_iter()
            .flat_map(|screen| &screen.windows)
            .filter_map(|window| window.picture)
            .collect();
        ui::window_pictures::want(&wanted);
        let unavailable = tilo_desktop::i18n::t("people-overlay-messages-unavailable");
        overlay.draw(
            scene,
            transform,
            text,
            theme,
            &PeopleOverlayModel {
                host: below_bar,
                occupied: &[],
                rows: frame.rows,
                screens: &screens,
                messages: &[],
                followed_id: followed.as_deref(),
                messaging_unavailable: Some(&unavailable),
            },
            &mut LivePictures,
        );
    }

    /// Route `event`; marks it handled when it was the overlay's or the
    /// banner's. Returns what the host applies.
    pub(super) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        cursor: Option<Point>,
        handled: &mut bool,
    ) -> Option<PeopleHit> {
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
            && let Some(point) = cursor
            && self.banner.stop_rect().contains(point)
        {
            self.stop_following(false);
            *handled = true;
            return None;
        }
        let hit = self
            .overlay
            .as_deref_mut()
            .and_then(|overlay| overlay.handle_event(event, scale, handled));
        if let Some(PeopleHit::Moved(edge)) = &hit {
            self.edge = *edge;
        }
        if let Some(PeopleHit::Moved(edge)) = &hit
            && let Err(error) = PEOPLE_OVERLAY.update(|prefs| PeopleOverlayPrefs {
                edge: *edge,
                ..prefs
            })
        {
            web_sys::console::warn_1(&format!("keeping the people overlay's edge: {error}").into());
        }
        hit
    }

    /// A key, before the desktop's: the message box's while it holds the
    /// keyboard; while following, Escape stops and any other key takes the
    /// view back. Returns whether the key was taken.
    pub(super) fn key(
        &mut self,
        event: &WindowEvent,
        modifiers: &Modifiers,
    ) -> (bool, Option<PeopleHit>) {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return (false, None);
        };
        if key.state != ElementState::Pressed {
            return (self.made().is_some_and(PinnedRoster::composing), None);
        }
        if let Some(hit) = self
            .overlay
            .as_deref_mut()
            .and_then(|overlay| overlay.key(&key.logical_key, modifiers.state()))
        {
            return (true, hit);
        }
        if self.followed.is_some() {
            let escape = matches!(key.logical_key, Key::Named(NamedKey::Escape));
            self.stop_following(!escape);
            return (escape, None);
        }
        (false, None)
    }

    /// A press on the desktop, not on the overlay or the banner: the
    /// person going back to their own view.
    pub(super) fn pressed_elsewhere(&mut self) {
        self.stop_following(true);
    }
}

/// Each person's screen as their live layout gives it, each window matched
/// against the records this page holds: a record held here is shared with
/// this person, and its window here stands for theirs (its title, its live
/// picture); any other shows as its app alone.
fn screens(frame: &Frame<'_>) -> Vec<PeerScreen> {
    frame
        .people
        .iter()
        .filter_map(|person| {
            let screen = person.screen.as_ref()?;
            let (width, height) = screen.size;
            let windows = screen
                .windows
                .iter()
                .map(|window| {
                    let here = frame.records.iter().find(|(_, app, record)| {
                        *app == window.app
                            && LiveRecord {
                                app: app.clone(),
                                record: record.clone(),
                            }
                            .short_digest()
                                == window.record
                    });
                    let tab = here.map(|(tab, _, _)| *tab);
                    PeerWindow {
                        rect: window.rect,
                        app: window.app.clone(),
                        app_name: crate::desktop::known_application(&window.app)
                            .map_or_else(|| window.app.clone(), str::to_owned),
                        title: tab.and_then(|tab| tab_title(frame.tree, tab)),
                        picture: tab.and_then(|tab| group_of(frame.tree, tab)),
                        focused: window.focused,
                    }
                })
                .collect();
            Some(PeerScreen {
                id: person.id.clone(),
                aspect: if height > 0.0 {
                    width / height
                } else {
                    16.0 / 10.0
                },
                windows,
            })
        })
        .collect()
}

fn tab_title(tree: &DesktopTree, tab: TabId) -> Option<String> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|t| t.id == tab)
        .map(|t| t.title.clone())
        .filter(|title| !title.trim().is_empty())
}

/// The window group `tab` is in, whose live picture stands for it.
fn group_of(tree: &DesktopTree, tab: TabId) -> Option<u64> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|group| group.tabs.iter().any(|t| t.id == tab))
        .map(|group| group.id)
}

/// The live pictures the strip keeps of the windows a peek shows.
struct LivePictures;

impl PeekPictures for LivePictures {
    fn paint(&mut self, window: u64, scene: &mut Scene, transform: Affine, rect: Rect) -> bool {
        ui::window_pictures::with_picture(window, |picture| {
            ui::window_pictures::paint_fitted(picture, scene, transform, rect)
        })
        .is_some_and(|painted| painted.area() > 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop::here::{Screen, ScreenWindow};
    use tilo_desktop_model::IdMinter;

    /// A window on someone's screen showing a record this page shows too
    /// stands for theirs, with its title and live picture; any other shows
    /// as its app and nothing more.
    #[test]
    fn only_what_this_page_holds_is_named_and_pictured() {
        let mut minter = IdMinter::new(3);
        let tab = tilo_desktop_model::Tab {
            id: minter.mint(),
            title: "Plans".into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
        };
        let tab_id = tab.id;
        let tree = tilo_desktop_surface::seed_tree(&mut minter, "Lounge", vec![tab]);
        let digest = |record: &str| {
            LiveRecord {
                app: "so.tilo.notes".into(),
                record: record.into(),
            }
            .short_digest()
        };
        let window = |record: &str, focused| ScreenWindow {
            app: "so.tilo.notes".into(),
            record: digest(record),
            rect: Rect::new(0.0, 0.0, 0.5, 1.0),
            focused,
        };
        let person = Person {
            id: "ada".into(),
            label: "Ada".into(),
            rgb: (1, 2, 3),
            agent: false,
            tab: None,
            place: None,
            status: None,
            recent: None,
            screen: Some(Screen {
                size: (1600.0, 1000.0),
                windows: vec![window("plans", true), window("diary", false)],
            }),
        };
        let records = [(tab_id, "so.tilo.notes".to_owned(), "plans".to_owned())];
        let screens = screens(&Frame {
            rect: Rect::new(0.0, 0.0, 1440.0, 900.0),
            rows: &[],
            people: std::slice::from_ref(&person),
            tree: &tree,
            records: &records,
            cursor: None,
            content_top: 0.0,
        });
        let [screen] = screens.as_slice() else {
            panic!("one screen: {screens:?}");
        };
        assert!((screen.aspect - 1.6).abs() < 1e-9);
        let [shared, private] = screen.windows.as_slice() else {
            panic!("two windows: {:?}", screen.windows);
        };
        assert_eq!(shared.title.as_deref(), Some("Plans"));
        assert_eq!(shared.picture, group_of(&tree, tab_id));
        assert!(shared.picture.is_some() && shared.focused);
        assert!(!private.shared(), "a record this page does not hold");
        assert_eq!((private.title.as_deref(), private.picture), (None, None));
        assert_eq!(private.app, "so.tilo.notes", "its app is all it says");
    }
}
