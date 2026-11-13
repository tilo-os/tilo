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
        #[cfg(target_os = "linux")]
        let before = tree.clone();
        if self.desktops.follow(engine, &device, &mut tree) {
            // The tree this device drew is the tree it shows: what the record holds is already
            // placed, and swapping it in whole keeps the windows this device shows where its own
            // view left them.
            self.handle.replace_tree(tree.clone());
            self.handle.set_opened_by(self.desktops.opened_by());
            // A window someone else opened in the workspace is one this device shows through its
            // own instance of the app, on the same record: the app is asked for it as a window
            // this device left open is.
            #[cfg(target_os = "linux")]
            self.restore.borrow_mut().arrived(&before, &tree);
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
        // A query the canvas stands asks its app for what it names. When the app's records hold
        // resources of that kind it did not hold when it asked, that is what it found (one
        // becomes the place; several show their rows). When the app's own search has finished —
        // its state left `searching` — and nothing new of the kind came in, it matched nothing.
        // A failure is never read from the app's records: it comes from the search's own
        // invocation result, at `place_found` and Retry (shell.md §8).
        let app_resources = self.services.borrow().resources();
        let running: Vec<(
            String,
            arrangement::ArrangementQueryReading,
            std::collections::BTreeSet<String>,
        )> = arrangement
            .queries_running()
            .into_iter()
            .filter_map(|query| {
                let held = arrangement.held_when_asked(query.key.parse().ok()?)?;
                Some((query.key.clone(), query, held))
            })
            .collect();
        for (key, query, held) in running {
            let fresh: Vec<arrangement::AnswerRef> = app_resources
                .iter()
                .flat_map(leaves)
                .filter(|resource| {
                    resource.kind == query.kind && !held.contains(resource.id.as_str())
                })
                .map(|resource| arrangement::AnswerRef {
                    address: resource.id.as_str().to_owned(),
                    title: resource.label.clone(),
                    kind: resource.kind.clone(),
                })
                .collect();
            if !fresh.is_empty() {
                arrangement.answer_query_of(&key, Ok(fresh), underway);
                continue;
            }
            // The search finished and nothing new of the kind came in: it matched nothing.
            if search_finished(&self.services, &app_resources, &query.app) {
                arrangement.answer_query_of(&key, Ok(Vec::new()), underway);
            }
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
            &self.notes,
            &|app: &str| Some(crate::app_names::app_name(app)),
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
        let engine = self.engine.clone();
        let awaited = move |workspace: &str| {
            let engine = engine.as_ref()?;
            engine.awaited_host(tilo_desktop_model::WorkspaceId::parse(workspace)?)
        };
        self.restore
            .borrow_mut()
            .follow(&self.desktop, self.launch.as_deref(), &awaited);
    }

    /// Stand each place a service's `open` asked for since the last pass: a resource's own `open`
    /// and the arrangement's `open(resource)` are one act — the system's — so the requests go
    /// through the arrangement door's `open`, that same code, beside the place the asking turn is
    /// in and attributed to the actor that asked (`docs/designs/app-language/one-app-model.md`
    /// §3; shell.md §8, "The canvas composes views; it opens nothing"). A resource already shown
    /// is brought forward instead of stood a second time.
    pub(crate) fn open_asked_by_services(&self) {
        let opens = self.services.borrow_mut().take_open();
        for (invoker, open) in opens {
            let arguments = {
                let mut arguments = serde_json::Map::new();
                arguments.insert("resource".into(), open.resource.clone().into());
                if !open.view.is_empty() {
                    arguments.insert("view".into(), open.view.clone().into());
                }
                if !open.beside.is_empty() {
                    arguments.insert("beside".into(), open.beside.clone().into());
                }
                arguments
            };
            let address = tilo_actions::ActionAddress::new(crate::arrangement::RESOURCE, "open");
            let run = self.doors.invoke_as(
                &invoker.actor,
                &address,
                &arguments,
                &mut tilo_session_core::door::Look::none(),
            );
            if let Err(error) = run {
                tracing::warn!(
                    %error,
                    resource = %open.resource,
                    "a service's open was not placed"
                );
            }
        }
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
        // What a place can show, as a find's answer holds it: the borrow ends here, since an
        // answer is heard on a later pass, off the pass that asked.
        let shows: std::rc::Rc<dyn Fn(&str) -> bool> = {
            let catalog = std::rc::Rc::clone(&catalog);
            std::rc::Rc::new(move |resource| catalog.borrow().get(resource).is_some())
        };
        self.session_doors
            .answer(&self.doors, &self.services, &self.search, &shows, look);
        // A service's `open` changes nothing of its record: opening is the session's arranging
        // step, and the places the services were asked for are stood through the arrangement's
        // own `open`, as an agent and a view asking the shell are one act
        // (`docs/designs/app-language/one-app-model.md` §3; shell.md §8, "The canvas composes
        // views; it opens nothing").
        self.open_asked_by_services();
        // The finds the door asked on an earlier pass whose sources answered since: answered here,
        // where the catalogue a place reads is, and never by the pass that asked them.
        self.search.poll_finds();
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

/// Every resource of `resource` and its children, deepest last: what an app publishes of a kind
/// is found among these, as a search's answer is read off the app's own records.
fn leaves(resource: &tilo_actions::ResourceSpec) -> Vec<&tilo_actions::ResourceSpec> {
    let mut out = vec![resource];
    for child in &resource.children {
        out.extend(leaves(child));
    }
    out
}

/// Whether the search the app `app` ran has finished: its own search state left `searching`. The
/// state a search passes through is the app's words, not the system's, so this reads only whether
/// a state word is still `searching` (the client that ships keeps `search_state`, but the fields a
/// type names are its own, and the app is asked through its action for its result, never its
/// records, when it fails). `None` while it still searches, or when the app keeps no such state.
fn search_finished(
    services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
    resources: &[tilo_actions::ResourceSpec],
    app: &str,
) -> bool {
    let belongs = |resource: &tilo_actions::ResourceSpec| {
        resource.id.as_str().starts_with(&format!("tl:{app}:"))
    };
    // The app's root: its one resource of the type a query naming the app means.
    let Some(root) = resources.iter().flat_map(leaves).find(|resource| {
        belongs(resource) && resource.resource_type.as_ref().is_some_and(|ty| ty.root)
    }) else {
        return false;
    };
    let Some(reading) = services.borrow().read(root.id.as_str()) else {
        return false;
    };
    // A search has finished when no text of the root's reading is still the word it uses for
    // searching. That is the one state every search keeps while it runs.
    !reading
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, value)| value.as_str())
        .any(|text| text == "searching")
}
