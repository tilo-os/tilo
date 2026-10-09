//! Tube as a [`tilo_resource::Resource`]: the library that syncs — likes,
//! the shared queue, subscriptions, and comment likes and replies — is
//! document state. What the catalogue fetched, the screen on show, and the
//! person's watch history and playback preferences are not.

use std::collections::BTreeMap;
use std::rc::Rc;

use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LiveRow, ResourceApp, ResourceSurface, RunMode,
    SurfaceCx, ViewCx,
};
use ui::tilo_resource::{ActionDecl, ActionRef, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{TUBE_ACTION_TYPE, TubeAction, TubeEffect, document_children};
use crate::app::TubeApp;

/// One reply to a comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    /// `false` once retracted.
    pub active: bool,
}

/// The shared library [`TubeAction`] writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TubeShared {
    pub likes: BTreeMap<String, bool>,
    pub queue: BTreeMap<String, bool>,
    pub subscriptions: BTreeMap<String, bool>,
    pub comment_likes: BTreeMap<String, bool>,
    /// Replies by comment id, then reply id.
    pub comment_replies: BTreeMap<String, BTreeMap<String, Reply>>,
}

impl TubeShared {
    /// The ids `map` holds as on, in id order.
    pub(crate) fn on(map: &BTreeMap<String, bool>) -> Vec<String> {
        map.iter()
            .filter(|(_, on)| **on)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// The Tube application resource.
pub struct Tube;

impl Resource for Tube {
    const KIND: &'static str = "tube.application";
    const LABEL: &'static str = "Tube";
    const ACTION_TYPE: &'static str = TUBE_ACTION_TYPE;

    type State = TubeShared;
    type Action = TubeAction;

    fn read(doc: &Doc) -> Result<TubeShared, ModelError> {
        Ok(read_shared(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        crate::actions::declarations()
    }

    fn children(state: &TubeShared) -> Vec<tilo_actions::Resource> {
        document_children(state)
    }

    fn search(state: &TubeShared) -> Vec<SearchEntry> {
        let queued = TubeShared::on(&state.queue).into_iter().map(|video_id| {
            SearchEntry::new(video_id.clone(), "queue").anchored(format!("tube.queue:{video_id}"))
        });
        let subscribed = TubeShared::on(&state.subscriptions)
            .into_iter()
            .map(|channel_id| {
                SearchEntry::new(channel_id.clone(), "subscription")
                    .anchored(format!("tube.subscription:{channel_id}"))
            });
        queued.chain(subscribed).collect()
    }
}

/// The named edits, for controls and scripts that invoke them.
impl Tube {
    pub const SET_VIDEO_LIKED: ActionRef<Tube, (String, bool)> =
        ActionRef::new("set-video-liked", &["video", "liked"]);
    pub const SET_QUEUED: ActionRef<Tube, (String, bool)> =
        ActionRef::new("set-queued", &["video", "queued"]);
    pub const SET_SUBSCRIBED: ActionRef<Tube, (String, bool)> =
        ActionRef::new("set-subscribed", &["creator", "subscribed"]);
    pub const SET_COMMENT_LIKED: ActionRef<Tube, (String, bool)> =
        ActionRef::new("set-comment-liked", &["comment", "liked"]);
    pub const REPLY_TO_COMMENT: ActionRef<Tube, (String, String)> =
        ActionRef::new("reply-to-comment", &["comment", "text"]);
}

fn map_at(doc: &Doc, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Map), map))) => Some(map),
        _ => None,
    }
}

fn bool_at(doc: &Doc, parent: &ObjId, key: &str) -> Option<bool> {
    doc.get(parent, key).ok().flatten()?.0.as_bool()
}

fn str_at(doc: &Doc, parent: &ObjId, key: &str) -> Option<String> {
    doc.get(parent, key)
        .ok()
        .flatten()?
        .0
        .as_str()
        .map(str::to_owned)
}

fn bool_map(doc: &Doc, parent: &ObjId, key: &str) -> BTreeMap<String, bool> {
    let Some(map) = map_at(doc, parent, key) else {
        return BTreeMap::new();
    };
    doc.keys(&map)
        .filter_map(|id| Some((id.clone(), bool_at(doc, &map, &id)?)))
        .collect()
}

fn read_replies(doc: &Doc, tube: &ObjId) -> BTreeMap<String, BTreeMap<String, Reply>> {
    let Some(comments) = map_at(doc, tube, "comment_replies") else {
        return BTreeMap::new();
    };
    doc.keys(&comments)
        .filter_map(|comment| {
            let replies = map_at(doc, &comments, &comment)?;
            let replies: BTreeMap<String, Reply> = doc
                .keys(&replies)
                .filter_map(|reply_id| {
                    let reply = map_at(doc, &replies, &reply_id)?;
                    let text = str_at(doc, &reply, "text")?;
                    let active = bool_at(doc, &reply, "active").unwrap_or(false);
                    Some((reply_id, Reply { text, active }))
                })
                .collect();
            (!replies.is_empty()).then_some((comment, replies))
        })
        .collect()
}

fn read_shared(doc: &Doc) -> TubeShared {
    let mut shared = TubeShared::default();
    let Some(tube) = map_at(doc, &ROOT, "tube") else {
        return shared;
    };
    shared.likes = bool_map(doc, &tube, "likes");
    shared.queue = bool_map(doc, &tube, "queue");
    shared.subscriptions = bool_map(doc, &tube, "subscriptions");
    shared.comment_likes = bool_map(doc, &tube, "comment_likes");
    shared.comment_replies = read_replies(doc, &tube);
    shared
}

/// The screenshot scenarios, each over the bundled sample catalogue: Home,
/// its empty and loading states, device sync, a playing watch
/// page, the narrow window with its rail summoned, and every screen.
pub(crate) const SCENARIOS: &[&str] = &[
    "home",
    "empty",
    "loading",
    "device-sync",
    "play-demo",
    "narrow",
    "trending",
    "subscriptions",
    "queue",
    "history",
    "settings",
    "search",
    "watch",
];

/// Framework app: the Tube widget is the surface over [`Tube`].
pub struct TubeShell;

impl ResourceApp for TubeShell {
    type Resource = Tube;
    const ID: &'static str = "so.tilo.tube";
    const TITLE: &'static str = "Tube";
    const ICON: Option<&'static str> = Some("tube");

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// The narrow scenario draws a phone-sized window, below the width at
    /// which the navigation rail docks.
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario == Some("narrow") {
            (560, 800)
        } else {
            (1280, 800)
        }
    }

    /// The sample library: the creators the sample catalogue subscribes to.
    fn scenario(_name: Option<&str>) -> Vec<Invoke<Tube>> {
        crate::sample::model()
            .subscriptions
            .into_iter()
            .map(|channel_id| TubeAction::SetSubscription {
                channel_id,
                subscribed: true,
            })
            .filter_map(|action| Invoke::apply(&action).ok())
            .collect()
    }

    fn view(cx: &mut ViewCx<Tube>) -> Element {
        cx.surface(Self::surface(cx.mode()))
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

impl ResourceSurface<Tube, TubeEffect> for TubeApp {
    fn present(&mut self, state: &Rc<TubeShared>) {
        self.present_library(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Tube>) {
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
        self.attach_framework(cx.dispatcher(), history);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.stage_scenario(scenario);
        }
    }

    fn effect(&mut self, effect: TubeEffect, _cx: &mut EffectCx<Tube, Self>) -> EffectAnswer {
        self.perform(effect).map(|()| Effected::done())
    }

    fn effects(&self) -> Vec<EffectStatus> {
        crate::rows::effect_statuses(self.device_sync_view())
    }

    /// Edits bound to the open watch page: like it, queue it, subscribe to
    /// its creator.
    fn bound_declarations(&self) -> Vec<ActionDecl<Tube>> {
        self.watch_page_declarations()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        crate::rows::live_rows(self.model_ref(), self.device_sync_view())
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        crate::rows::live_row(self.model_ref(), self.device_sync_view(), id)
    }

    /// The videos, creators, playlists and watch history shown here.
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.search_entries_of_model()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::reply_key;
    use tilo_actions::OperationKind;
    use ui::framework::testing::TestApp;

    #[test]
    fn every_scenario_captures_over_the_sample_library() {
        let mut sample = crate::sample::model().subscriptions;
        sample.sort();
        for scenario in SCENARIOS {
            let app = TestApp::<TubeShell>::capture(Some(scenario))
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                TubeShared::on(&app.state().subscriptions),
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
        // Tall enough that the creator row sits on screen.
        let mut app = TestApp::<TubeShell>::capture_at(Some("watch"), (1280, 1200)).unwrap();
        click(&mut app, "watch_like");
        assert_eq!(app.state().likes.get("vid_donut"), Some(&true));
        click(&mut app, "watch_like");
        assert_eq!(
            app.state().likes.get("vid_donut"),
            Some(&false),
            "the second click unlikes what the first liked"
        );

        let channel = crate::sample::model()
            .video("vid_donut")
            .unwrap()
            .channel_id
            .clone();
        let subscribed = app.state().subscriptions.get(&channel) == Some(&true);
        click(&mut app, "subscribe_btn");
        assert_eq!(
            app.state().subscriptions.get(&channel),
            Some(&!subscribed),
            "the subscribe pill flips the subscription"
        );

        // Each click is one step the person can take back.
        assert!(app.undo());
        assert_eq!(
            app.state().subscriptions.get(&channel) == Some(&true),
            subscribed
        );
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

        // Agents reach the same preferences through effects.
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
        app.invoke(Tube::SET_VIDEO_LIKED.with(("vid_donut".into(), true)))
            .unwrap();
        app.harness().relayout();
        // What the window shows, as the edit bound to the open video says.
        let liked = |app: &mut TestApp<TubeShell>| {
            let resources = app.harness().resources();
            let like = tilo_actions::flatten(&resources)
                .into_iter()
                .find(|flat| flat.action.id == "like-this-video")
                .expect("the open video's like is published");
            like.action.label == "Unlike this video"
        };
        assert!(liked(&mut app));
        app.merge_peer_edit(|doc| {
            tilo_actions::Action::apply(
                &TubeAction::SetVideoLiked {
                    video_id: "vid_donut".into(),
                    liked: false,
                },
                doc,
            )
            .unwrap();
        });
        app.harness().relayout();
        assert!(
            !liked(&mut app),
            "unliking the last liked video empties what the window shows"
        );
    }

    #[test]
    fn edits_on_entity_rows_are_declarations_and_comments_read_back() {
        let mut app = TestApp::<TubeShell>::capture(Some("watch")).unwrap();
        let video = crate::sample::model().video("vid_donut").unwrap().clone();
        let comment = crate::actions::comment_key("vid_donut", &video.comments[0]);
        app.invoke(Tube::SET_COMMENT_LIKED.with((comment.clone(), true)))
            .unwrap();
        // The queue and replies are ordered by an authoritative host, which
        // this test host is not: they arrive as the host applied them.
        assert!(
            app.invoke(Tube::SET_QUEUED.with(("vid_donut".into(), true)))
                .is_err()
        );
        let reply = crate::actions::declarations()
            .into_iter()
            .find(|decl| decl.id == Tube::REPLY_TO_COMMENT.id)
            .unwrap();
        let reply = (reply.build)(
            &tilo_actions::ActionArgs::bind_json(
                &reply.params,
                &Tube::REPLY_TO_COMMENT
                    .arguments(&(comment.clone(), "Agreed".into()))
                    .unwrap(),
            )
            .unwrap(),
            0,
        )
        .unwrap();
        app.merge_peer_edit(|doc| {
            for action in [
                TubeAction::SetQueueMembership {
                    video_id: "vid_donut".into(),
                    queued: true,
                },
                reply,
            ] {
                tilo_actions::Action::apply(&action, doc).unwrap();
            }
        });
        let state = app.state();
        assert_eq!(state.queue.get("vid_donut"), Some(&true));
        assert_eq!(state.comment_likes.get(&comment), Some(&true));
        assert_eq!(
            state.comment_replies[&comment][&reply_key(&comment, "Agreed")],
            Reply {
                text: "Agreed".into(),
                active: true
            }
        );

        // The reply is published beneath the library, and can be retracted.
        let resources = app.harness().resources();
        let reply = tilo_actions::flatten(&resources)
            .into_iter()
            .find(|flat| {
                flat.resource_id.0.starts_with("tube.reply:") && flat.action.id == "retract"
            })
            .expect("the reply is published with its retraction");
        assert_eq!(reply.action.kind, OperationKind::Document);
    }

    #[test]
    fn the_published_tree_carries_document_edits_only_on_the_resource() {
        let mut app = TestApp::<TubeShell>::capture(Some("watch")).unwrap();
        let resources = app.harness().resources();
        let flat = tilo_actions::flatten(&resources);
        let at = |resource: &str, action: &str| {
            flat.iter()
                .find(|flat| flat.resource_id.0 == resource && flat.action.id == action)
                .map(|flat| flat.action.kind)
        };
        let root = flat
            .iter()
            .find(|flat| flat.action.id == "set-video-liked")
            .map(|flat| flat.resource_id.0.clone())
            .expect("the edits on a video are the resource's declarations");
        for edit in [
            "set-queued",
            "set-subscribed",
            "set-comment-liked",
            "reply-to-comment",
            "like-this-video",
            "queue-this-video",
            "subscribe-to-this-creator",
        ] {
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
        // The library's own entries carry its edits.
        let channel = crate::sample::model().subscriptions[0].clone();
        assert_eq!(
            at(&format!("tube.subscription:{channel}"), "unsubscribe"),
            Some(OperationKind::Document)
        );
    }

    /// Whether a resource whose id starts with `prefix` is published.
    fn resources_have(resources: &[tilo_actions::Resource], prefix: &str) -> bool {
        resources.iter().any(|resource| {
            resource.id.0.starts_with(prefix) || resources_have(&resource.children, prefix)
        })
    }

    #[test]
    fn effects_declare_their_contracts() {
        use ui::tilo_resource::Effect;
        let specs = TubeEffect::specs();
        let spec = |id: &str| specs.iter().find(|spec| spec.id == id).unwrap();
        assert_eq!(
            spec("pair-device").params()[0].ty,
            tilo_actions::ParamType::Secret,
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
                tilo_actions::ConfirmationPolicy::Confirm,
                "{confirmed}"
            );
        }
        assert_eq!(
            spec("open-video").contract.permission,
            tilo_actions::PermissionRequirement::Read
        );
    }
}
