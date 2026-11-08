use super::*;
use crate::chrome::*;
use std::{
    cell::{Ref, RefCell, RefMut},
    rc::Rc,
};
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "test fixture mirrors independent overlay toggles"
)]
pub struct Overlays {
    pub overview: bool,
    pub collaborators: bool,
    pub workspace_activity: bool,
    pub ws_settings: bool,
}
pub struct State {
    pub tree: DesktopTree,
    pub overlays: Overlays,
    pub glance: ActivityGlance,
    /// The activities the bar's pill names, each with the app that shows it: an activity's id is
    /// only its own app's.
    pub activities: Vec<(String, ui::live_activity::Summary)>,
    pub active_exposures: Vec<ActiveExposure>,
    pub watching: u32,
    pub intents: Vec<ChromeIntent>,
    pub own: Option<OwnChip>,
    pub can_invite: bool,
    pub waved: Option<WaveNotice>,
    pub notification_badge: u32,
    pub notifications_only: bool,
    pub arrived: Option<ArrivalNotice>,
    /// Unread counts per verified application id, as an application's own icon shows them.
    pub app_badges: HashMap<String, u32>,
}
impl State {
    pub fn publish_activity_glance(
        &mut self,
        headline: Option<String>,
        unread: usize,
        pulse: u64,
        focus: Option<ActivityFocus>,
    ) {
        self.glance = ActivityGlance {
            headline,
            unread,
            pulse,
            focus,
        };
    }
    pub fn set_active_exposures(&mut self, mut exposures: Vec<ActiveExposure>) {
        exposures.compact_sort();
        exposures.dedup();
        self.active_exposures = exposures;
    }
}
pub struct FixtureChrome(RefCell<State>);
impl FixtureChrome {
    pub fn new(tree: DesktopTree) -> Rc<Self> {
        Rc::new(Self(RefCell::new(State {
            tree,
            overlays: Overlays::default(),
            glance: ActivityGlance::default(),
            activities: Vec::new(),
            active_exposures: Vec::new(),
            watching: 0,
            intents: Vec::new(),
            own: None,
            can_invite: false,
            waved: None,
            notification_badge: 0,
            notifications_only: false,
            arrived: None,
            app_badges: HashMap::default(),
        })))
    }
    pub fn borrow(&self) -> Ref<'_, State> {
        self.0.borrow()
    }
    pub fn borrow_mut(&self) -> RefMut<'_, State> {
        self.0.borrow_mut()
    }
}
impl DesktopChromeSource for FixtureChrome {
    fn tree(&self) -> DesktopTree {
        self.0.borrow().tree.clone()
    }
    fn snapshot(&self) -> DesktopChromeSnapshot {
        let state = self.0.borrow();
        DesktopChromeSnapshot {
            overlays: ChromeOverlays {
                overview: state.overlays.overview,
                ws_settings: state.overlays.ws_settings,
            },
            activity_glance: state.glance.clone(),
            activities: ui::floating_layer::pill(&state.activities),
            active_exposures: state.active_exposures.clone(),
            watching: state.watching,
            own: state.own.clone(),
            invite: crate::chrome::Invite::when(state.can_invite),
            waved: state.waved.clone(),
            notification_badge: state.notification_badge,
            notifications_only: state.notifications_only,
            arrived: state.arrived.clone(),
            ..Default::default()
        }
    }
    fn app_notification_badge(&self, app_id: &str) -> u32 {
        self.0.borrow().app_badges.get(app_id).copied().unwrap_or(0)
    }
    fn intent(&self, intent: ChromeIntent) -> bool {
        let mut state = self.0.borrow_mut();
        state.intents.push(intent.clone());
        match intent {
            ChromeIntent::ToggleOverview => state.overlays.overview = !state.overlays.overview,
            ChromeIntent::ToggleCollaborators => {
                state.overlays.collaborators = !state.overlays.collaborators;
            }
            // History opens as a tab the host places; no overlay moves.
            ChromeIntent::ShowActivity(_) => state.overlays.collaborators = false,
            ChromeIntent::SwitchWorkspace(delta) => {
                let Some(index) = state
                    .tree
                    .workspaces
                    .iter()
                    .position(|w| w.slot == state.tree.active)
                else {
                    return false;
                };
                let Some(id) = i64::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_add(i64::from(delta)))
                    .and_then(|next| usize::try_from(next).ok())
                    .and_then(|i| state.tree.workspaces.get(i))
                    .map(|w| w.slot)
                else {
                    return false;
                };
                state.tree.active = id;
            }
            _ => {}
        }
        true
    }
}
