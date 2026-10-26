//! Tube's shared library as a resource: likes, the shared watch queue,
//! subscriptions, and comment likes and replies. What the catalogue
//! fetched, the screen on show, and the person's watch history and
//! playback preferences are not part of it.
//!
//! Liking, queueing and subscribing are field edits of the library's sets
//! (`Library::paths().likes.insert(&video)`), so concurrent adds and
//! removes merge per video. Replying to a comment is the one verb.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ResourceApp, ResourceSurface, RunMode,
    SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{
    self, ActionCx, Collection, Edit, Id, Invoke, Model, Path, Refusal, SearchEntry, Set, resource,
};
use ui::widget::Element;

use crate::actions::TubeEffect;
use crate::app::TubeApp;

/// One reply to a comment, visible to everyone sharing the library.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    /// Comment
    ///
    /// The comment it answers: its id after `tube.comment:`.
    #[model(readonly)]
    pub comment: String,
    /// Reply
    #[model(label, readonly)]
    pub text: String,
}

/// The library that syncs between everyone who shares it.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Library {
    /// Liked videos
    pub likes: Set<String>,
    /// Shared watch queue
    ///
    /// Membership, not order, so concurrent adds and removes converge
    /// without one person reordering another's queue.
    pub queue: Set<String>,
    /// Subscribed creators
    ///
    /// A channel id, or a channel URL a paired device brought in before
    /// this one fetched the channel.
    pub subscriptions: Set<String>,
    /// Liked comments
    ///
    /// By their id after `tube.comment:`.
    pub comment_likes: Set<String>,
    /// Replies to comments
    pub replies: Collection<Reply>,
}

#[resource(kind = "tube.library", label = "Tube")]
impl Library {
    /// Reply to a comment.
    ///
    /// Visible to everyone sharing this library. The same reply twice to one
    /// comment is kept once, so a retried reply is not posted again.
    #[action]
    pub fn reply_to_comment(
        &mut self,
        cx: &mut ActionCx,
        #[param("The comment's id: its row id after `tube.comment:`")] comment: String,
        #[param("The reply")] text: String,
    ) -> Result<(), Refusal> {
        let text = text.trim();
        if comment.trim().is_empty() || text.is_empty() {
            return Err(Refusal::new("a reply names its comment and says something"));
        }
        if self
            .replies
            .values()
            .any(|reply| reply.comment == comment && reply.text == text)
        {
            return Ok(());
        }
        self.replies.push(
            cx.id(),
            Reply {
                comment,
                text: text.to_owned(),
            },
        );
        Ok(())
    }

    /// Retract a reply.
    #[action]
    pub fn retract(&mut self, reply: &Id) -> Result<(), Refusal> {
        self.replies
            .remove(reply)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such reply"))
    }
}

impl Library {
    /// The edit that likes `video` or takes the like back.
    pub fn like(video: &str, liked: bool) -> Edit<Self> {
        Self::member(&Self::paths().likes, video, liked)
    }

    /// The edit that adds `video` to the shared queue or takes it out.
    pub fn enqueue(video: &str, queued: bool) -> Edit<Self> {
        Self::member(&Self::paths().queue, video, queued)
    }

    /// The edit that subscribes to the creator stored as `key`, or
    /// unsubscribes it.
    pub fn subscribe(key: &str, subscribed: bool) -> Edit<Self> {
        Self::member(&Self::paths().subscriptions, key, subscribed)
    }

    /// The edit that likes the comment `comment` or takes the like back.
    pub fn like_comment(comment: &str, liked: bool) -> Edit<Self> {
        Self::member(&Self::paths().comment_likes, comment, liked)
    }

    /// The edit that puts `value` in the set at `set`, or takes it out.
    fn member(set: &Path<Self, Set<String>>, value: &str, member: bool) -> Edit<Self> {
        if member {
            set.insert(&value.to_owned())
        } else {
            set.remove(&value.to_owned())
        }
    }
}

/// The screenshot scenarios, each over the bundled sample catalogue: Home,
/// its empty and loading states, device sync, a playing watch
/// page, the narrow window with its rail summoned, the sidebar widened
/// into a list, and every screen.
pub(crate) const SCENARIOS: &[&str] = &[
    "home",
    "empty",
    "loading",
    "device-sync",
    "play-demo",
    "narrow",
    "sidebar-list",
    "trending",
    "subscriptions",
    "queue",
    "history",
    "settings",
    "search",
    "watch",
];

/// Framework app: the Tube widget is the surface over [`Library`].
pub struct TubeShell;

/// What the binary runs instead of opening a window.
///
/// `--probe-youtube-engine VALUE` (the value is ignored) runs the `YouTube`
/// client's self-check without a network. It needs `runtime-engine`, the
/// release build, where that code is in `libtilo_youtube_engine.so`; the
/// check proves Tube loads that library and runs it.
const COMMANDS: &[ui::framework::Command] = &[ui::framework::Command {
    option: "--probe-youtube-engine",
    run: probe_youtube_engine,
}];

#[cfg(feature = "runtime-engine")]
fn probe_youtube_engine(_: &str) -> Result<(), String> {
    let report = tilo_youtube_engine::self_check()?;
    println!("tube --probe-youtube-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_youtube_engine(_: &str) -> Result<(), String> {
    Err("the YouTube engine library needs the `runtime-engine` feature".to_string())
}

impl ResourceApp for TubeShell {
    type Resource = Library;
    const ID: &'static str = "so.tilo.tube";
    const TITLE: &'static str = "Tube";
    const ICON: Option<&'static str> = Some("tube");
    /// A launch may name a search (`tube:search?q=cats`).
    const ARGUMENTS: usize = 1;

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }

    /// The narrow scenario draws a phone-sized window, below the width at
    /// which the navigation rail docks.
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario == Some("narrow") {
            (340, 800)
        } else {
            (1280, 800)
        }
    }

    /// The sample library: the creators the sample catalogue subscribes to.
    fn scenario(_name: Option<&str>) -> Vec<Invoke<Library>> {
        crate::sample::model()
            .subscriptions
            .iter()
            .filter_map(|channel_id| Invoke::edit(&Library::subscribe(channel_id, true)).ok())
            .collect()
    }

    fn view(cx: &mut ViewCx<Library>) -> Element {
        // A search hit, or an agent's `open`, opens the video, creator or
        // playlist it names.
        let dispatcher = cx.dispatcher();
        cx.on_open(
            move |anchor: &str| -> Result<(), ui::tilo_resource::Refusal> {
                let effect = crate::search_type::opened(anchor).ok_or_else(|| {
                    ui::tilo_resource::Refusal::new(format!("`{anchor}` is nothing Tube opens"))
                })?;
                dispatcher.effect(&effect);
                Ok(())
            },
        );
        let surface = Self::surface(cx.mode());
        // A live window launched to search ("Search YouTube for cats" in
        // desktop search) opens on those results.
        let surface = match crate::search_type::launch_search(cx.launch_arguments()) {
            Some(query) if cx.mode().is_live() => surface.searching(&query),
            _ => surface,
        };
        cx.surface(surface)
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }
}

impl TubeShell {
    /// The widget the framework mounts: in a live window the online app,
    /// which fetches the real catalogue and runs device sync; in a capture
    /// the bundled sample catalogue, offline; for a past version the
    /// library alone.
    pub(crate) fn surface(mode: RunMode) -> TubeApp {
        match mode {
            RunMode::Live => TubeApp::new().online(),
            RunMode::Capture => TubeApp::new(),
            RunMode::Past => TubeApp::empty(),
        }
    }
}

impl ResourceSurface<Library, TubeEffect> for TubeApp {
    fn present(&mut self, state: &Rc<Library>) {
        self.present_library(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Library>) {
        // Only a live window follows the person's own history: a capture
        // shows the sample's, a past version none.
        let history = cx
            .mode()
            .is_live()
            .then(|| cx.person::<crate::history::WatchHistory>())
            .flatten();
        self.follow_preferences(cx.settings::<crate::model::Settings>());
        // Only a live window pairs and syncs devices, with the app's secret
        // store and the devices paired on this one.
        #[cfg(feature = "grayjay-sync")]
        if cx.mode().is_live() {
            self.start_device_sync(
                &cx.secrets(),
                cx.storage().settings::<crate::sync::PairedDevices>(),
            );
        }
        self.wake_with(cx.waker());
        self.attach_framework(cx.dispatcher(), history);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.stage_scenario(scenario);
        }
    }

    fn effect(&mut self, effect: TubeEffect, _cx: &mut EffectCx<Library, Self>) -> EffectAnswer {
        self.perform(effect).map(|()| Effected::done())
    }

    fn effects(&self) -> Vec<EffectStatus> {
        crate::rows::effect_statuses(self.model_ref(), self.device_sync_view())
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        crate::rows::live_rows(self.model_ref(), self.device_sync_view())
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        crate::rows::live_row(self.model_ref(), self.device_sync_view(), id)
    }

    /// What the platform search is looking for: the catalogue holds only
    /// what has loaded, so the results offer to search `YouTube` for it.
    fn search_query(&mut self, query: &ui::search::SearchQuery) {
        self.follow_palette_query(query.text());
    }

    /// `YouTube` is still answering what the platform search asked.
    fn search_pending(&self) -> bool {
        self.suggestions_pending()
    }

    /// The videos, creators, playlists and watch history shown here, and a
    /// `YouTube` search for what was typed.
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.search_entries_of_model()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::OperationKind;
    use ui::tilo_resource::{Replica, items::item_resource_id};

    fn subscriptions(library: &Library) -> Vec<String> {
        let mut subscribed: Vec<String> = library.subscriptions.iter().cloned().collect();
        subscribed.sort();
        subscribed
    }

    #[test]
    fn every_scenario_captures_over_the_sample_library() {
        let mut sample = crate::sample::model().subscriptions;
        sample.sort();
        for scenario in SCENARIOS {
            let app = TestApp::<TubeShell>::capture(Some(scenario))
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                subscriptions(&app.state()),
                sample,
                "{scenario} stages the sample library"
            );
        }
    }

    fn click(app: &mut TestApp<TubeShell>, id: &str) {
        app.harness()
            .click_named(id)
            .unwrap_or_else(|error| panic!("{error}"));
        app.harness().relayout();
    }

    #[test]
    fn the_watch_page_controls_change_the_shared_library() {
        let mut app = TestApp::<TubeShell>::capture_at(Some("watch"), (1280, 1200)).unwrap();
        let liked = |app: &TestApp<TubeShell>| app.state().likes.contains(&"vid_donut".to_owned());
        click(&mut app, "watch_like");
        assert!(liked(&app));
        click(&mut app, "watch_like");
        assert!(
            !liked(&app),
            "the second click unlikes what the first liked"
        );

        click(&mut app, "watch_queue");
        assert!(app.state().queue.contains(&"vid_donut".to_owned()));

        let channel = crate::sample::model()
            .video("vid_donut")
            .unwrap()
            .channel_id
            .clone();
        let subscribed = |app: &TestApp<TubeShell>| app.state().subscriptions.contains(&channel);
        let before = subscribed(&app);
        click(&mut app, "subscribe_btn");
        assert_eq!(
            subscribed(&app),
            !before,
            "the subscribe pill flips the subscription"
        );

        assert!(app.undo());
        assert_eq!(subscribed(&app), before);
    }

    #[test]
    fn the_settings_controls_change_the_persons_preferences_not_the_library() {
        use crate::model::{Category, Settings, SkipMode};
        // A live window: a capture's controls start no effects.
        let mut app = TestApp::<TubeShell>::for_person(&"ad".repeat(32)).unwrap();
        app.effect(&TubeEffect::ShowScreen {
            screen: "settings".into(),
        })
        .unwrap();
        app.harness().relayout();
        let preferences = app.settings::<Settings>();
        let autoplay = preferences.get().autoplay;
        let library = app.peer_document().save();
        click(&mut app, "switch_autoplay");
        app.settle();
        assert_eq!(preferences.get().autoplay, !autoplay);
        let category = app
            .harness()
            .a11y_node_named(Category::Sponsor.label())
            .unwrap();
        app.harness()
            .perform_a11y_action(
                category,
                &ui::a11y::Action::SetValue(SkipMode::Show.label().into()),
            )
            .unwrap();
        app.settle();
        assert_eq!(
            preferences.get().mode_of(Category::Sponsor),
            SkipMode::Show,
            "the shared mode selector changes Auto-skip to Show"
        );
        assert_eq!(
            preferences.scope(),
            ui::storage::Scope::Personal,
            "they follow the person to their devices"
        );
        assert_eq!(
            app.peer_document().save(),
            library,
            "collaborators on the library never see them"
        );

        app.effect(&TubeEffect::SetPreferredQuality {
            quality: "720p".into(),
        })
        .unwrap();
        assert_eq!(preferences.get().preferred_quality, "720p");
        assert!(
            app.effect(&TubeEffect::SetPreferredQuality {
                quality: "8k".into()
            })
            .is_err(),
            "a quality Tube cannot play is refused"
        );
        app.effect(&TubeEffect::SetPreference {
            preference: "dearrow".into(),
            enabled: false,
        })
        .unwrap();
        assert!(!preferences.get().dearrow_enabled);
        app.effect(&TubeEffect::SetSegmentHandling {
            category: Category::Filler.api_key().into(),
            mode: "skip".into(),
        })
        .unwrap();
        assert_eq!(preferences.get().mode_of(Category::Filler), SkipMode::Skip);
    }

    #[test]
    fn what_the_person_watched_is_kept_in_their_own_record_not_the_library() {
        use crate::history::WatchHistory;
        let person = "ad".repeat(32);
        let mut app = TestApp::<TubeShell>::for_person(&person).unwrap();
        app.effect(&TubeEffect::MarkWatched {
            video: "vid_donut".into(),
        })
        .unwrap();
        app.effect(&TubeEffect::MarkWatched {
            video: "vid_tea".into(),
        })
        .unwrap();
        // Watched in the same millisecond or not, both are kept.
        let watched = |history: &ui::framework::PersonRecord<WatchHistory>| {
            let mut videos = history.with(WatchHistory::videos);
            videos.sort();
            videos
        };
        let history = app.person::<WatchHistory>().unwrap();
        assert_eq!(watched(&history), ["vid_donut", "vid_tea"]);
        assert!(history.syncs(), "it follows the person to their devices");
        let route = history.record().route().unwrap();
        assert_eq!(
            route.cluster_id.to_string(),
            ui::record::person::scope_of(&ui::permissions::UserId(person)),
            "in the person's own room"
        );
        let library = app.peer_document().save();
        assert!(
            !library
                .windows(b"vid_tea".len())
                .any(|window| window == b"vid_tea"),
            "the shared library holds nothing of what the person watched"
        );

        // Forgetting is the person's edit: their undo brings the video back.
        app.effect(&TubeEffect::ForgetWatched {
            video: "vid_donut".into(),
        })
        .unwrap();
        assert_eq!(watched(&history), ["vid_tea"]);
        assert!(history.undo());
        assert_eq!(watched(&history), ["vid_donut", "vid_tea"]);

        // It is the same history after a relaunch.
        drop(history);
        let mut app = app.reopen().unwrap();
        let history = app.person::<WatchHistory>().unwrap();
        assert_eq!(watched(&history), ["vid_donut", "vid_tea"]);
        app.effect_confirmed(&TubeEffect::ClearHistory).unwrap();
        assert!(history.with(WatchHistory::videos).is_empty());
    }

    #[test]
    fn a_collaborators_emptied_library_is_shown_empty() {
        let mut app = TestApp::<TubeShell>::capture(Some("watch")).unwrap();
        app.invoke(Invoke::edit(&Library::like("vid_donut", true)).unwrap())
            .unwrap();
        app.harness().relayout();
        let liked = |app: &mut TestApp<TubeShell>| {
            app.surface_mut::<TubeApp>()
                .model_for_test()
                .is_liked("vid_donut")
        };
        assert!(liked(&mut app));
        app.merge_peer_edit(|doc| {
            Library::like("vid_donut", false).apply_local(doc).unwrap();
        });
        app.harness().relayout();
        assert!(
            !liked(&mut app),
            "unliking the last liked video empties what the window shows"
        );
    }

    /// Liking a comment, queueing a video and replying are the library's
    /// own: field edits of its sets and the reply verb, each one call.
    #[test]
    fn comments_are_liked_and_answered_through_the_library() {
        let mut app = TestApp::<TubeShell>::capture(Some("watch")).unwrap();
        let video = crate::sample::model().video("vid_donut").unwrap().clone();
        let comment = crate::actions::comment_key("vid_donut", &video.comments[0]);
        app.call(
            "field-edit",
            serde_json::json!({ "edit": Library::like_comment(&comment, true) }),
        )
        .unwrap();
        app.call(
            "field-edit",
            serde_json::json!({ "edit": Library::enqueue("vid_donut", true) }),
        )
        .unwrap();
        let reply = || {
            Library::REPLY_TO_COMMENT.with(library::ReplyToComment {
                comment: comment.clone(),
                text: " Agreed ".into(),
            })
        };
        app.invoke(reply()).unwrap();
        app.invoke(reply()).unwrap();
        let state = app.state();
        assert!(state.queue.contains(&"vid_donut".to_owned()));
        assert!(state.comment_likes.contains(&comment));
        let replies: Vec<(&Id, &Reply)> = state.replies.iter().collect();
        assert_eq!(replies.len(), 1, "a retried reply is kept once");
        assert_eq!(
            replies[0].1,
            &Reply {
                comment: comment.clone(),
                text: "Agreed".into(),
            }
        );
        assert!(
            app.invoke(Library::REPLY_TO_COMMENT.with(library::ReplyToComment {
                comment: comment.clone(),
                text: "   ".into(),
            }))
            .is_err(),
            "a blank reply is refused"
        );

        let item = item_resource_id::<Library>("replies", replies[0].0);
        let resources = app.harness().resources();
        let retract = ui::tilo_actions::flatten(&resources)
            .into_iter()
            .find(|flat| flat.resource_id.0 == item && flat.action.id == "retract")
            .expect("the reply is published with its retraction");
        assert_eq!(retract.action.kind, OperationKind::Document);
        app.call_on(&item, "retract", serde_json::json!({}))
            .unwrap();
        assert!(app.state().replies.is_empty());
        assert!(app.undo(), "a retraction is one step");
        assert_eq!(app.state().replies.len(), 1);
    }

    #[test]
    fn the_published_tree_carries_document_edits_only_on_the_resource() {
        let mut app = TestApp::<TubeShell>::capture(Some("watch")).unwrap();
        let resources = app.harness().resources();
        let flat = ui::tilo_actions::flatten(&resources);
        let at = |resource: &str, action: &str| {
            flat.iter()
                .find(|flat| flat.resource_id.0 == resource && flat.action.id == action)
                .map(|flat| flat.action.kind)
        };
        let root = flat
            .iter()
            .find(|flat| flat.action.id == "reply-to-comment")
            .map(|flat| flat.resource_id.0.clone())
            .expect("the library's verbs are the resource's");
        for edit in ["reply-to-comment", "field-edit"] {
            assert_eq!(at(&root, edit), Some(OperationKind::Document), "{edit}");
        }
        // Rows the catalogue fetched carry no document edit.
        for prefix in [
            "tube.current:",
            "tube.video:",
            "tube.comment:",
            "tube.channel:",
        ] {
            assert!(
                flat.iter()
                    .any(|flat| flat.resource_id.0.starts_with(prefix))
                    || resources_have(&resources, prefix),
                "a {prefix} row is published"
            );
        }
        for row in flat.iter().filter(|flat| {
            [
                "tube.video:",
                "tube.current:",
                "tube.comment:",
                "tube.channel:",
                "tube.history:",
            ]
            .iter()
            .any(|prefix| flat.resource_id.0.starts_with(prefix))
        }) {
            assert_ne!(
                row.action.kind,
                OperationKind::Document,
                "{} {} is a document edit on a live row",
                row.resource_id.0,
                row.action.id
            );
        }
    }

    /// Whether a resource whose id starts with `prefix` is published.
    fn resources_have(resources: &[ui::tilo_actions::Resource], prefix: &str) -> bool {
        resources.iter().any(|resource| {
            resource.id.0.starts_with(prefix) || resources_have(&resource.children, prefix)
        })
    }

    /// Two people sharing a library at once converge: likes, the queue and
    /// subscriptions merge per entry, replies per reply.
    #[test]
    fn concurrent_edits_converge() {
        let mut ours = Replica::<Library>::new();
        ours.apply(Invoke::edit(&Library::like("vid_a", true)).unwrap())
            .unwrap();
        let mut theirs = ours.fork();
        ours.apply(Invoke::edit(&Library::like("vid_b", true)).unwrap())
            .unwrap();
        ours.apply(Invoke::edit(&Library::enqueue("vid_a", true)).unwrap())
            .unwrap();
        theirs
            .apply(Invoke::edit(&Library::like("vid_a", false)).unwrap())
            .unwrap();
        theirs
            .apply(Invoke::edit(&Library::subscribe("UC_A", true)).unwrap())
            .unwrap();
        theirs
            .apply(Library::REPLY_TO_COMMENT.with(library::ReplyToComment {
                comment: "c1".into(),
                text: "Nice".into(),
            }))
            .unwrap();
        ours.sync_with(&mut theirs).unwrap();
        let merged = ours.state().unwrap();
        assert_eq!(merged, theirs.state().unwrap());
        assert_eq!(
            merged.likes.iter().cloned().collect::<Vec<_>>(),
            ["vid_b"],
            "one unliked a video while the other liked another"
        );
        assert!(merged.queue.contains(&"vid_a".to_owned()));
        assert!(merged.subscriptions.contains(&"UC_A".to_owned()));
        assert_eq!(merged.replies.len(), 1);
    }

    /// What an agent does in the library reads as Tube's sentences.
    #[test]
    fn history_entries_render_as_the_apps_sentences() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};

        let mut app = TestApp::<TubeShell>::capture(Some("watch")).unwrap();
        let agent = ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("ada");
        let kind = <Library as ui::tilo_resource::Resource>::KIND;
        for edit in [
            Library::like("vid_donut", true),
            Library::like("vid_donut", false),
            Library::enqueue("vid_donut", true),
            Library::enqueue("vid_donut", false),
            Library::subscribe("UC_A", true),
            Library::subscribe("UC_A", false),
            Library::like_comment("c1", true),
            Library::like_comment("c1", false),
        ] {
            app.call_by(
                agent.clone(),
                kind,
                "field-edit",
                serde_json::json!({ "edit": edit }),
            )
            .unwrap();
        }
        app.call_by(
            agent.clone(),
            kind,
            "reply-to-comment",
            serde_json::json!({"comment": "c1", "text": "Agreed"}),
        )
        .unwrap();
        let reply = app.state().replies.ids().next().unwrap().clone();
        app.call_by(
            agent,
            &item_resource_id::<Library>("replies", &reply),
            "retract",
            serde_json::json!({}),
        )
        .unwrap();

        let sentences = app.sentences(app.history().iter().filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        }));
        assert_eq!(
            sentences,
            [
                "Liked a video",
                "Took back a like",
                "Added a video to the shared queue",
                "Removed a video from the shared queue",
                "Subscribed to a creator",
                "Unsubscribed from a creator",
                "Liked a comment",
                "Took back a comment like",
                "Replied Agreed",
                "Retracted the reply Agreed",
            ]
        );
    }

    #[test]
    fn effects_declare_their_contracts() {
        use ui::tilo_resource::Effect;
        let specs = TubeEffect::specs();
        let spec = |id: &str| specs.iter().find(|spec| spec.id == id).unwrap();
        assert_eq!(
            spec("pair-device").params()[0].ty,
            ui::tilo_actions::ParamType::Secret,
            "a pairing invite is kept out of logs and history"
        );
        for confirmed in [
            "clear-history",
            "pair-device",
            "approve-device",
            "revoke-device",
        ] {
            assert_eq!(
                spec(confirmed).contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::Confirm,
                "{confirmed}"
            );
        }
        assert_eq!(
            spec("open-video").contract.kind,
            ui::tilo_actions::ActionKind::Read
        );
    }
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::TubeShell::scenarios() {
            let mut app = TestApp::<super::TubeShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
