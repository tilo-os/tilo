//! What the session's shell keeps between frames, and the passes that fold what arrived into it:
//! each workspace's shared desktop, the workspace's history, the windows left open, and what the
//! sheets and settings asked for.

use crate::platform::surfaces::WidgetSurface;
use tilo_session_core::presence::{Place, Step, Target};
use tilo_sheets::SheetState as _;

use crate::{Shared, workspace_settings};
use tilo_session_core::layout;

impl Shared {
    /// Keep what this device shows: the workspace in front and the displays' partition, so the next
    /// session starts where this one was. The shell itself is the workspaces' layouts, written as the
    /// windows change.
    pub(crate) fn keep_layout(&self) {
        if crate::termination::asked() {
            return;
        }
        self.activity.keep_shown(Some(self.handle.revision()), || {
            self.handle.read(Clone::clone)
        });
        // What the layout keeps of the kept apps' windows, their keepers keep too: the windows
        // take the same tabs after an in-place restart.
        #[cfg(target_os = "linux")]
        self.restore
            .borrow_mut()
            .tell_placements(&self.desktop, self.launch.as_deref());
    }

    /// [`Self::keep_layout`], and wait for what was written: what ends the session.
    pub(crate) fn keep_layout_now(&self) {
        // A workspace made on this session is founded off the loop, and the shell's own writes need
        // it held: what ends the session waits for it first.
        self.landings.settle(std::time::Duration::from_secs(5));
        if let Some(engine) = &self.engine {
            crate::layout::Layouts::settle_keys(
                engine,
                &self.handle.read(Clone::clone),
                std::time::Duration::from_secs(5),
            );
        }
        // The apps a login's end takes with it leave the workspaces' layouts as they were. An
        // in-place restart keeps them, so the last thing the person moved is written for the desktop
        // that follows instead of being lost with the one that saw it.
        if crate::termination::asked() && !crate::restart::restarting() {
            // The apps a login's end takes with it leave the workspaces' layouts as they were; the
            // person's view of them is theirs to keep, and the next session shows it.
            if let Some(engine) = &self.engine {
                self.desktops.finish_view(
                    engine,
                    &self.handle.read(Clone::clone),
                    std::time::Duration::from_secs(5),
                );
            }
            crate::kept::flush(self.state.as_deref());
            return;
        }
        self.activity
            .keep_shown(None, || self.handle.read(Clone::clone));
        // What this device changed of its workspaces' layouts reaches them before the session ends.
        if let Some(engine) = &self.engine {
            self.desktops.finish_writes(
                engine,
                &self.handle.read(Clone::clone),
                std::time::Duration::from_secs(5),
            );
        }
        // What the layout keeps of the kept apps' windows, their keepers keep too: the windows
        // take the tabs the write just saved in the session that follows.
        #[cfg(target_os = "linux")]
        self.restore
            .borrow_mut()
            .tell_placements(&self.desktop, self.launch.as_deref());
        crate::kept::flush(self.state.as_deref());
    }

    /// Show each workspace's shared desktop as the workspace holds it, and write back what this
    /// device changed there: what every device in a workspace sees of its strips and windows
    /// ([`layout::Layouts`]).
    pub(crate) fn follow_layouts(&self) {
        // The first look begins this session's restore: a desktop that has drawn is usable once
        // the windows this device left open are being asked for, or there are none to ask for.
        self.restore.borrow_mut().begin();
        let Some(engine) = &self.engine else {
            self.restore.borrow_mut().without_layouts();
            return;
        };
        self.desktops
            .attach(engine, &self.document_reads, &self.document_writes);
        // The apps a login's end takes with it leave the workspaces' sessions as they were.
        if crate::termination::asked() {
            return;
        }
        // What each window shows is the workspace's layout's, not this device's reading of it: a
        // window carried by its bar names it, and a window let go over here finds the place that
        // shows it.
        self.handle
            .set_window_resources(self.layout.borrow().window_resources());
        // Every workspace this device holds has had its record read, and
        // nothing of the workspaces moved since: there is nothing to read,
        // lay out or write.
        if self.desktops.settled(
            crate::layout::EngineVersion::of(engine),
            &self.handle.read(Clone::clone),
        ) {
            return;
        }
        // What the records hold, folded into a copy of the shell: what this device opened or moved
        // since was written first, so the fold comes back with this device's own change in it and
        // nothing is undone.
        // A step of a layout the person asked for while a write of it was under way is taken now
        // that the write is done, before this look starts another.
        if let Some((room, redo)) = self.desktops.take_pending_step() {
            self.desktops.step(room, &self.person, redo, &self.handle);
        }
        let mut tree = self.handle.read(Clone::clone);
        self.desktops.capture(engine, &tree);
        // What the workspace did not take of this session is on the shell the person used: they hear it
        // once, naming the workspace and why.
        if let Some(refusal) = self.desktops.take_refusal() {
            crate::notices::tell_shell_not_taken(&self.notifications, &tree, refusal);
        }
        let device = self
            .sync
            .as_ref()
            .map_or_else(String::new, |sync| sync.publisher().to_string());
        #[cfg(target_os = "linux")]
        let before = tree.clone();
        if self.desktops.follow(engine, &device, &mut tree) {
            // The tree this device drew is the tree it shows: what the layout holds is already
            // placed, and swapping it in whole keeps the windows this device shows where its own
            // view left them.
            #[cfg(target_os = "linux")]
            {
                let mut restore = self.restore.borrow_mut();
                // The first layout this session lays out is the shell it reopens: a place no window
                // came for is not a window. After that, a window someone else opened in the
                // workspace is one this device shows through its own instance of the app, on the
                // same record, and its app is asked for it as a window this device left open is.
                let under_way = |tab| self.desktop.with_policy(|policy| policy.on_its_way(tab));
                if !restore.start(&before, &mut tree, &under_way) {
                    restore.arrived(&before, &tree);
                }
            }
            self.handle.replace_tree(tree.clone());
            self.handle.set_opened_by(self.desktops.opened_by());
        }
        if self.desktops.all_read(&tree) {
            self.restore.borrow_mut().layouts_read();
        }
    }

    /// Run what waited for the person to finish their gesture or their typing: a layout action
    /// that arrived then, in the order it came, after the windows the person floated or put back on
    /// the shell, which are their own and never wait.
    pub(crate) fn follow_layout(&self, underway: layout::Busy) {
        let mut layout = self.layout.borrow_mut();
        if let Err(why) = layout.take_asked_floats() {
            tracing::warn!(why, "a window could not be floated or put back");
        }
        if !underway.holds()
            && layout.waiting() > 0
            && let Err(why) = layout.release()
        {
            tracing::warn!(why, "a layout action that waited could not be made");
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
            layout::LayoutQueryReading,
            std::collections::BTreeSet<String>,
        )> = layout
            .queries_running()
            .into_iter()
            .filter_map(|query| {
                let held = layout.held_when_asked(query.key.parse().ok()?)?;
                Some((query.key.clone(), query, held))
            })
            .collect();
        for (key, query, held) in running {
            let fresh: Vec<layout::AnswerRef> = app_resources
                .iter()
                .flat_map(leaves)
                .filter(|resource| {
                    resource.kind == query.kind && !held.contains(resource.id.as_str())
                })
                .map(|resource| layout::AnswerRef {
                    address: resource.id.as_str().to_owned(),
                    title: resource.label.clone(),
                    kind: resource.kind.clone(),
                })
                .collect();
            if !fresh.is_empty() {
                layout.answer_query_of(&key, Ok(fresh), underway);
                continue;
            }
            // The search finished and nothing new of the kind came in: it matched nothing.
            if search_finished(&self.services, &app_resources, &query.app) {
                layout.answer_query_of(&key, Ok(Vec::new()), underway);
            }
        }
    }

    /// Whom this device follows, as its own presence word says: the window of theirs shown here comes forward each
    /// time the place they work in changes, and their app where no window here shows their record. Doing something of
    /// their own takes their view back, and the workspace hears that they follow nobody.
    ///
    /// Returns what the workspace's apps are to hear of it where that changed: the workspace,
    /// and whom this person follows there (`None`: nobody).
    pub(crate) fn follow_people(&self) -> Option<(String, Option<tilo_actions::MemberId>)> {
        let Some(sync) = &self.sync else {
            return None;
        };
        let followed = sync.own().following;
        if self.follow.borrow().idle(followed.as_deref()) {
            return None;
        }
        let people = sync.people(
            &self.handle.read(Clone::clone),
            &self.notes,
            &|app: &str| Some(crate::app_names::app_name(app)),
        );
        let places: Vec<Place<'_>> = people
            .iter()
            .map(tilo_desktop::here::Person::place)
            .collect();
        let member = followed
            .as_deref()
            .and_then(|id| tilo_desktop::here::followed_member(&people, id));
        let owed = self.follow.borrow_mut().owe(
            followed,
            self.underway.get().holds(),
            sync.follow_asked(),
            &places,
            member,
            &self.active_workspace_id(),
        )?;
        sync.forget_follow_asked();
        if owed.ended {
            sync.stop_following();
        }
        match owed.step {
            Step::Rest | Step::Gone => {}
            Step::Bring(Target::Window(tab)) => {
                self.handle.focus_tab(tab);
            }
            Step::Bring(Target::App(app)) => {
                self.handle.focus_app(&app);
            }
        }
        owed.tell
    }

    /// Go to where `who` (a roster id) works, once: the window of theirs shown here, else a window
    /// of the app they are in. Nothing moves when neither stands here.
    pub(crate) fn go_to_person(&self, who: &str) {
        let Some(sync) = &self.sync else {
            return;
        };
        let people = sync.people(
            &self.handle.read(Clone::clone),
            &self.notes,
            &|app: &str| Some(crate::app_names::app_name(app)),
        );
        tilo_desktop::here::go_to(&self.handle, &people, who);
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
    /// reads, asks the shell for the frame that serves it.
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
        // into the shell's document as models answer; the shell draws a frame to read them.
        let described = self.activity.describe(&self.handle.read(Clone::clone));
        if self.activity.take_woken() || described {
            // A window asked the activity model something, or what the history reads moved: the
            // shell draws a frame, which serves it.
            self.desktop.with_widgets(WidgetSurface::relayout);
        }
    }

    /// Tell the person when someone mentions them in one of the workspaces' records, and open the
    /// places they chose from those notices: the app's `open` at the item, as a search hit on it
    /// runs.
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
            let open = tilo_desktop_search::InvokeTarget::place(workspace.as_str(), &place);
            if let Err(error) =
                crate::search::AppActions::invoke(&**apps, open.remote(), open.arguments)
            {
                tracing::warn!(%error, app = %place.app, "a mention was not opened");
            }
        }
    }

    /// The desktop's control hints: the person's record as their settings hold it, the reasons
    /// this session knows (following someone, setup over the desktop), and what they
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

    /// The pages account sign-ins asked for: each opened in a window of its own, and closed when
    /// its sign-in ends.
    pub(crate) fn follow_sign_in_pages(&self) {
        if let (Some(accounts), Some(launch)) = (self.table.accounts(), &self.launch) {
            accounts.follow(launch);
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
    /// and the layout's `open(resource)` are one act — the system's — so the requests go
    /// through the layout API's `open`, that same code, beside the place the asking turn is
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
            let address =
                tilo_actions::ActionAddress::new(tilo_session_core::layout::RESOURCE, "open");
            let run = self.published.invoke_as(
                &invoker.acting.by,
                &address,
                &arguments,
                &mut tilo_session_core::surfaces::Look::none(),
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
    pub(crate) fn follow_sheets(&self, look: &mut tilo_session_core::surfaces::Look<'_>) {
        // What a place can show, as the layout published it: an agent's `find` leads with the
        // resource typing the text would place, so it reads the same catalogue the canvas places
        // through.
        let catalog = self.layout.borrow().targets();
        self.session_services.keep_tabs(&self.handle);
        if let Some(engine) = &self.engine {
            self.session_services.keep_awaiting(&self.handle, engine);
        }
        // What a place can show, as a find's answer holds it: the borrow ends here, since an
        // answer is heard on a later pass, off the pass that asked.
        let shows: std::rc::Rc<dyn Fn(&str) -> bool> = {
            let catalog = std::rc::Rc::clone(&catalog);
            std::rc::Rc::new(move |resource| catalog.borrow().get(resource).is_some())
        };
        self.session_services
            .answer(&self.published, &self.services, &self.search, &shows, look);
        // A service's `open` changes nothing of its record: opening is the session's arranging
        // step, and the places the services were asked for are stood through the layout's
        // own `open`, as an agent and a view asking the shell are one act
        // (`docs/designs/app-language/one-app-model.md` §3; shell.md §8, "The canvas composes
        // views; it opens nothing").
        self.open_asked_by_services();
        // The finds the API asked on an earlier pass whose sources answered since: answered here,
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
            &crate::platform::directory::SessionDirectory(self),
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
        self.follow_nearby_shares();
        self.follow_pair_device();
        self.follow_delete_workspace();
        self.follow_owner_deletions();
        self.follow_boot_join();
        self.follow_hosted_browser();
        // A program of this login asking to work as an agent is a question of the person's: their
        // answer admits it as their agent member for its client, which the broker's roster lists
        // before the client is told (`Shared::mcp_member`), and it is served from the next pass on.
        if !self.boot_join.borrow().waiting()
            && let Some(server) = self.mcp_server.borrow_mut().as_mut()
        {
            server.follow(
                &self.handle,
                &self.person,
                &|workspace: &str, client: &str| self.mcp_member(workspace, client),
            );
        }
        self.tell_agent_bindings_lost();
        self.asker.follow(&self.notifications);
        self.follow_reach();
        self.shortcut_guide.follow();
        self.app_sheets.follow(self.downloaded_apps().as_ref());
    }
    /// Lend the apps what decides whether a launch starts now: an app of a workspace that has not
    /// admitted this device yet is not started, and whoever asked hears why at once
    /// (`Apps::spawn`); an app the person installed waits on what they let it reach.
    pub(crate) fn lend_to_apps(&self) {
        let Some(launch) = &self.launch else {
            return;
        };
        if let Some(engine) = &self.engine {
            let (engine, handle) = (std::sync::Arc::clone(engine), self.handle.clone());
            launch.lend_admission(std::rc::Rc::new(move |workspace: &str| {
                crate::mcp_server::awaited_reason(&engine, &handle, workspace)
            }));
        }
        if let Some(questions) = self.asker.questions() {
            launch.lend_questions(questions.clone());
        }
        // Where a workspace's resources open on the web, as its directory entry says: what a
        // tilolang window's host mints its resources' links under. A workspace whose host serves
        // no page lends nothing, and a copy link there is refused with the system's words.
        let directory = std::rc::Rc::clone(&self.directory_store);
        launch.lend_link_home(std::rc::Rc::new(move |workspace: &str| {
            let listed = directory.listing(workspace)?;
            let origin = listed.entry_url.as_ref()?.origin().ascii_serialization();
            let web_entry = crate::platform::directory::community_site()
                .map_or_else(|| format!("{origin}/"), |site| site.to_string());
            let home = tilolang_tilo::share::LinkHome {
                origin,
                workspace: workspace.to_owned(),
                host_device: String::new(),
                web_entry,
            };
            match home.json() {
                Ok(home) => Some(home),
                Err(error) => {
                    // Every field is text, so this cannot fail short of a bug: the window's
                    // links are refused, and the log says why.
                    tracing::warn!(
                        %error,
                        "a workspace's page does not write as JSON: its windows' resources have \
                         no link"
                    );
                    None
                }
            }
        }));
    }

    /// One anchored surface at a time, whichever side opened it.
    /// The shell's own popovers and the shell's views are two stacks that meet here: a popover that
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
        let shell_up = self.surfaces.overlays.here() != tilo_desktop::overlays::Overlay::Rest;
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
