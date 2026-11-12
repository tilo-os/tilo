//! What the session's desk keeps between frames, and the passes that fold what arrived into it:
//! each workspace's shared desktop, the workspace's history, the windows left open, and what the
//! sheets and settings asked for.

use crate::platform::surfaces::WidgetHost;
use tilo_session_core::presence::{Place, Step, Target};
use tilo_sheets::Seat as _;

use crate::{Shared, arrangement, workspace_settings};

impl Shared {
    /// Show each workspace's shared desktop as the workspace holds it, and write back what this
    /// device changed there: what every device in a workspace sees of its strips and windows
    /// ([`shared_desktop::SharedDesktops`]).
    pub(crate) fn follow_shared_desktops(&self) {
        let Some(engine) = &self.engine else {
            return;
        };
        // The apps a login's end takes with it leave the workspaces' desks as they were.
        if crate::termination::asked() {
            return;
        }
        // Every workspace this device holds has had its record read, and
        // nothing of the workspaces moved since: there is nothing to read,
        // lay out or write.
        if self
            .desktops
            .settled(engine.owner().revision(), &self.handle.read(Clone::clone))
        {
            return;
        }
        // What the records hold, folded into a copy of the desk: what this device opened or moved
        // since was written first, so the fold comes back with this device's own change in it and
        // nothing is undone.
        let mut tree = self.handle.read(Clone::clone);
        self.desktops.capture(engine, self.sync.as_deref(), &tree);
        // What the workspace did not take of this desk is on the desk the person used: they hear it
        // once, naming the workspace and why.
        if let Some(refusal) = self.desktops.take_refusal() {
            crate::notices::tell_desk_not_taken(&self.notifications, &tree, refusal);
        }
        let device = self
            .sync
            .as_ref()
            .map_or_else(String::new, |sync| sync.publisher().to_string());
        if self.desktops.follow(engine, &device, &mut tree) {
            // The tree this device drew is the tree it shows: what the record holds is already
            // placed, and swapping it in whole keeps the windows this device shows where its own
            // view left them.
            self.handle.replace_tree(tree.clone());
            self.handle.set_opened_by(self.desktops.opened_by());
        }
    }

    /// Run what waited for the person to finish their gesture or their typing: an arranging action
    /// that arrived then, in the order it came. The arrangement is the desk, so what the desk did
    /// meanwhile needs no taking in.
    pub(crate) fn follow_arrangement(&self, underway: arrangement::Busy) {
        let mut arrangement = self.arrangement.borrow_mut();
        if !underway.holds()
            && arrangement.waiting() > 0
            && let Err(why) = arrangement.release()
        {
            tracing::warn!(why, "an arranging action that waited could not be made");
        }
    }

    /// Whom this device follows, as its own presence word says: the window of theirs shown here comes forward each
    /// time the place they work in changes, and their app where no window here shows their record. Doing something of
    /// their own takes their view back, and the workspace hears that they follow nobody.
    pub(crate) fn follow_people(&self) {
        let Some(sync) = &self.sync else {
            return;
        };
        let followed = sync.own().following;
        let mut follow = self.follow.borrow_mut();
        if followed.is_none() && follow.followed().is_none() {
            return;
        }
        // Their own gesture or typing is them, not the person they followed: the view is theirs
        // again, and the workspace hears they follow nobody. The press that asked to follow is not
        // them working, so it does not end what it began.
        if self.underway.get().holds() {
            if sync.follow_asked() {
                return;
            }
            follow.follow(None);
            drop(follow);
            self.say_following_nobody();
            return;
        }
        sync.forget_follow_asked();
        follow.follow(followed);
        let people = sync.people(
            &self.handle.read(Clone::clone),
            &self.notes.records(),
            &|_: &str| None,
        );
        let places: Vec<Place<'_>> = people
            .iter()
            .map(|person| Place {
                id: &person.id,
                tab: person.tab,
                app: person.followed_app(),
            })
            .collect();
        let step = follow.step(&places);
        drop(follow);
        match step {
            Step::Rest => {}
            Step::Bring(Target::Window(tab)) => {
                self.handle.focus_tab(tab);
            }
            Step::Bring(Target::App(app)) => {
                self.handle.focus_app(&app);
            }
            Step::Gone => self.say_following_nobody(),
        }
    }

    /// This device follows nobody from now on, and the workspace hears it once.
    fn say_following_nobody(&self) {
        let Some(sync) = &self.sync else {
            return;
        };
        let mut own = sync.own();
        if own.following.take().is_some() {
            sync.set_own(own);
        }
    }

    /// The workspace in front, as its id: where a chosen notification's app opens.
    pub(crate) fn active_workspace_id(&self) -> String {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|held| held.id().to_string())
                .unwrap_or_default()
        })
    }

    /// Take up what the workspace's history wrote back and what the person opened from it, and tell
    /// the apps following it what moved. A window that asked, or a change of what the history
    /// reads, asks the desk for the frame that serves it.
    pub(crate) fn serve_activity(&self) {
        if let Some(tree) = self.activity.take_restored() {
            self.handle.replace_tree(tree);
        }
        for (workspace, target) in self.activity.take_openings() {
            self.open_from_history(&workspace, &target);
        }
        self.activity.keep_unlock();
        self.activity.serve(&self.notifications.borrow());
        // What the history says its changes were, and the names over the runs of them, are written
        // into the desk's document as models answer; the desk draws a frame to read them.
        let described = self.activity.describe();
        if self.activity.take_woken() || described {
            // A window asked the activity model something, or what the history reads moved: the
            // desk draws a frame, which serves it.
            self.desktop.with_widgets(WidgetHost::relayout);
        }
    }

    /// Tell the person when someone mentions them in one of the workspaces' records, and open the
    /// places they chose from those notices: the app's own `open` of the item, as the person's
    /// choice runs it.
    pub(crate) fn serve_mentions(&self) {
        let Some(mentions) = &self.mentions else {
            return;
        };
        mentions.step();
        let chosen = mentions.take_chosen();
        if chosen.is_empty() {
            return;
        }
        let Some(apps) = &self.apps else {
            return;
        };
        let workspace = self.active_workspace_id();
        for place in chosen {
            let target = crate::platform::notice::open_target(&workspace, &place);
            if let Err(error) =
                crate::search::AppActions::invoke(&**apps, target, serde_json::Map::new())
            {
                tracing::warn!(%error, app = %place.app, "a mention was not opened");
            }
        }
    }

    /// The desktop's control hints: the person's record as their settings hold it, the reasons
    /// this session knows (following someone, setup standing over the desktop), and what they
    /// learned since.
    pub(crate) fn follow_hints(&self, setup_up: bool) {
        let settings = settings_store::stamp();
        if settings != self.hints_stamp.get() {
            self.hints_stamp.set(settings);
            self.hints
                .set_record(settings_store::control_hints::LEARNED.get());
        }
        let quiet = if self.follow.borrow().followed().is_some() {
            Some(ui::control_hints::Quiet::Following)
        } else {
            setup_up.then_some(ui::control_hints::Quiet::Busy)
        };
        let search_keys = self.shortcut_guide.search_keys();
        self.hints.set_context(quiet, !setup_up, search_keys);
        if let Some(learned) = self.hints.take_learned() {
            // Not kept, a hint at worst shows once more: nothing to tell.
            settings_store::control_hints::LEARNED.set(learned).ok();
        }
    }

    /// Open what the person chose in the history: the app that holds it,
    /// showing it.
    pub(crate) fn open_from_history(
        &self,
        workspace_id: &tilo_desktop_model::WorkspaceId,
        target: &ui::HistoryResourceTarget,
    ) {
        let Some(apps) = &self.launch else {
            return;
        };
        let Some(app) = target.app_id.as_deref() else {
            return;
        };
        let open = if let Some(path) = target.id.strip_prefix("file:") {
            Some(tilo_app_launch::Open::File(std::path::PathBuf::from(path)))
        } else if target.id.contains("://") {
            Some(tilo_app_launch::Open::Uri(target.id.clone()))
        } else {
            None
        };
        let launch = tilo_app_launch::Launch::Package {
            app: app.to_owned(),
            open,
        };
        let workspace = apps.workspace_named(&workspace_id.to_string());
        if let Err(error) = apps.start(&launch, &workspace) {
            tracing::warn!(%error, %app, "an app is not opened from the history");
            // The change is still in the history, so the person reads it there: what they chose did
            // not happen, and why.
            self.answer_failed(&crate::i18n::t("action-failed-that-change"), &error);
        }
    }

    /// The windows this device left open: ask for the next one once the one before it came, each
    /// into the tab it was in.
    pub(crate) fn follow_restore(&self) {
        self.restore
            .borrow_mut()
            .follow(&self.desktop, self.launch.as_deref());
    }

    /// Carry out what the person chose in the sheets.
    pub(crate) fn follow_sheets(&self, look: &mut tilo_session_core::door::Look<'_>) {
        // What a place can show, as the arrangement published it: an agent's `find` leads with the
        // resource typing the text would place, so it reads the same catalogue the canvas places
        // through.
        let catalog = self.arrangement.borrow().targets();
        self.session_doors.keep_tabs(&self.handle);
        if let Some(engine) = &self.engine {
            self.session_doors.keep_awaiting(&self.handle, engine);
        }
        self.session_doors.answer(
            &self.doors,
            &self.services,
            &self.search,
            &|resource| catalog.borrow().get(resource).is_some(),
            look,
        );
        if let Some(power) = &self.power {
            for effect in power.take_asked() {
                power.presentation().close();
                self.power_effect(effect);
            }
        }

        crate::directory::follow(
            self.directory.as_ref(),
            &self.directory_store,
            &*self.telling(),
            &crate::platform::directory::DeskDirectory(self),
        );
        workspace_settings::follow(
            self.workspace_settings.as_ref(),
            &self.workspace_settings_feed(),
            &self.intents,
        );
        workspace_settings::refresh(
            self.workspace_settings.as_ref(),
            &self.workspace_settings_feed(),
        );
        self.follow_invite();
        self.follow_delete_workspace();
        self.follow_owner_deletions();
        self.follow_boot_join();
        self.follow_hosted_browser();
        // A program of this login asking to work as an agent is a question of the person's: their
        // answer admits it, and the client is served from the next pass on.
        if !self.boot_join.borrow().waiting()
            && let Some(door) = self.agent_door.borrow_mut().as_mut()
        {
            door.follow(&self.handle, &self.person);
        }
        self.tell_agent_bindings_lost();
        self.asker.follow();
        self.shortcut_guide.follow();
        self.app_sheets.follow(self.downloaded_apps().as_ref());
    }
    /// One anchored surface at a time, whichever side opened it.
    /// The desk's own popovers and the shell's views are two stacks that meet here: a popover that
    /// came up over a shell surface takes that surface down — the person is looking at the menu
    /// they just opened — and a shell surface that came up closes the roster, which the roster's
    /// own host draws over everything.
    pub(crate) fn lone_surface(&self) {
        if self.surfaces.beside_up() {
            self.surfaces.clear_shell();
        }
        if !self.people.borrow().is_open() {
            return;
        }
        let shell_up = self.surfaces.overlays.here() != crate::overlays::Overlay::Rest.name();
        if shell_up || self.surfaces.beside_up() {
            self.people.borrow().dismiss();
        }
    }
}
