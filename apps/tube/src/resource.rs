//! Tube as a [`tilo_resource::Resource`]: likes, queue, subscriptions and
//! settings that sync are document state; the route on screen is View.

use std::collections::BTreeMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{TUBE_ACTION_TYPE, TubeAction, document_children, register};
use crate::app::TubeApp;
use crate::model::{Settings, SkipMode, TubeModel};

/// Shared collaborative projection written by [`TubeAction`].
#[derive(Clone, Debug, Default)]
pub struct TubeShared {
    pub likes: BTreeMap<String, bool>,
    pub queue: BTreeMap<String, bool>,
    pub subscriptions: BTreeMap<String, bool>,
    pub settings: Settings,
}

impl TubeShared {
    /// Membership sets and settings only — no fetched catalogue.
    pub(crate) fn as_model_projection(&self) -> TubeModel {
        let on = |map: &BTreeMap<String, bool>| -> Vec<String> {
            map.iter()
                .filter(|(_, enabled)| **enabled)
                .map(|(id, _)| id.clone())
                .collect()
        };
        let mut model = TubeModel::default();
        model.liked = on(&self.likes);
        model.queue = on(&self.queue);
        model.subscriptions = on(&self.subscriptions);
        model.settings = self.settings.clone();
        model
    }

    /// The collaborative slice of a live model, without the fetched catalogue.
    pub(crate) fn from_model(model: &TubeModel) -> Self {
        let flags = |ids: &[String]| -> BTreeMap<String, bool> {
            ids.iter().cloned().map(|id| (id, true)).collect()
        };
        Self {
            likes: flags(&model.liked),
            queue: flags(&model.queue),
            subscriptions: flags(&model.subscriptions),
            settings: model.settings.clone(),
        }
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

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<TubeShared, ModelError> {
        Ok(read_shared(doc))
    }

    fn children(state: &TubeShared) -> Vec<tilo_actions::Resource> {
        document_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Navigation and settings ride `children`. Toggle-playback needs the
        // live watch route and stays on the widget's current-video extra.
        Vec::new()
    }

    fn search(state: &TubeShared) -> Vec<SearchEntry> {
        state
            .queue
            .iter()
            .filter(|(_, queued)| **queued)
            .map(|(video_id, _)| SearchEntry {
                anchor: Some(format!("tube.queue:{video_id}")),
                title: video_id.clone(),
                text: "queue".into(),
            })
            .chain(
                state
                    .subscriptions
                    .iter()
                    .filter(|(_, subscribed)| **subscribed)
                    .map(|(channel_id, _)| SearchEntry {
                        anchor: Some(format!("tube.subscription:{channel_id}")),
                        title: channel_id.clone(),
                        text: "subscription".into(),
                    }),
            )
            .collect()
    }
}

/// Document membership when `tube` has been written; otherwise the widget model.
pub(crate) fn shared_for_publish(model: &TubeModel, doc: &Doc) -> TubeShared {
    if matches!(doc.get(automerge::ROOT, "tube"), Ok(Some(_))) {
        read_shared(doc)
    } else {
        TubeShared::from_model(model)
    }
}

fn read_shared(doc: &Doc) -> TubeShared {
    let mut shared = TubeShared::default();
    let Ok(Some((Value::Object(ObjType::Map), tube))) = doc.get(ROOT, "tube") else {
        return shared;
    };
    shared.likes = bool_map(doc, &tube, "likes");
    shared.queue = bool_map(doc, &tube, "queue");
    shared.subscriptions = bool_map(doc, &tube, "subscriptions");
    let Ok(Some((Value::Object(ObjType::Map), settings))) = doc.get(&tube, "settings") else {
        return shared;
    };
    for (key, slot) in [
        ("sponsorblock", &mut shared.settings.sponsorblock_enabled),
        ("dearrow", &mut shared.settings.dearrow_enabled),
        ("dearrow_titles", &mut shared.settings.dearrow_titles),
        (
            "dearrow_thumbnails",
            &mut shared.settings.dearrow_thumbnails,
        ),
        (
            "return_dislike",
            &mut shared.settings.return_dislike_enabled,
        ),
        ("autoplay", &mut shared.settings.autoplay),
        ("show_comments", &mut shared.settings.show_comments),
    ] {
        if let Ok(Some((value, _))) = doc.get(&settings, key)
            && let Some(flag) = value.as_bool()
        {
            *slot = flag;
        }
    }
    if let Ok(Some((value, _))) = doc.get(&settings, "preferred_quality")
        && let Some(quality) = value.as_str()
    {
        shared.settings.preferred_quality = quality.to_owned();
    }
    if let Ok(Some((Value::Object(ObjType::Map), categories))) =
        doc.get(&settings, "sponsor_categories")
    {
        for category in crate::model::Category::ALL {
            let Some(mode) = doc
                .get(&categories, category.api_key())
                .ok()
                .flatten()
                .and_then(|(value, _)| value.as_str().map(str::to_owned))
                .and_then(|value| match value.as_str() {
                    "skip" => Some(SkipMode::Skip),
                    "show" => Some(SkipMode::Show),
                    "disabled" => Some(SkipMode::Disabled),
                    _ => None,
                })
            else {
                continue;
            };
            shared
                .settings
                .categories
                .insert(category.api_key().to_string(), mode);
        }
    }
    shared
}

fn bool_map(doc: &Doc, parent: &automerge::ObjId, key: &str) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(parent, key) else {
        return out;
    };
    for id in doc.keys(&map) {
        if let Ok(Some((value, _))) = doc.get(&map, id.as_str())
            && let Some(flag) = value.as_bool()
        {
            out.insert(id, flag);
        }
    }
    out
}

/// Framework app: the Tube widget is the surface over [`Tube`].
pub struct TubeShell;

impl ResourceApp for TubeShell {
    type Resource = Tube;
    const ID: &'static str = "so.tilo.tube";
    const TITLE: &'static str = "Tube";
    const ICON: Option<&'static str> = Some("tube");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Tube>) -> Element {
        cx.surface(TubeApp::new())
    }
}

impl ResourceSurface<Tube> for TubeApp {
    fn present(&mut self, state: &Rc<TubeShared>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Tube>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for shared likes, queue, subscriptions and settings.
/// Watch history stays on its own local host.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Tube>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Tube>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &TubeAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        if let Err(error) = self.try_dispatch_with_context(doc, type_id, action, context) {
            tracing::warn!(error = %error, type_id, "tube action dispatch failed");
        }
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &TubeAction,
        context: ui::tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                self.last = EncodedAction::encode(TUBE_ACTION_TYPE, action).ok();
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Hosted commits are recorded by the dispatcher. Standalone tests still
    /// read the local host's rising edge.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }
}

pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<TubeShell>()
}

pub fn standalone_launch() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots"
            | "--responsive-matrix"
            | "--native-audit"
            | "--headless"
            | "--narrow"
            | "--probe"
            | "--probe-trending"
            | "--probe-engine"
            | "--probe-play"
            | "--probe-stream"
            | "--probe-gst"
            | "--probe-ytdlp"
            | "--probe-avatars"
            | "--live-shots" => return true,
            "--viewport" => {
                let _ = args.next();
                return true;
            }
            flag if flag.starts_with("--probe") || flag.starts_with("--shots=") => return true,
            file if !file.starts_with('-') => return true,
            _ => {}
        }
    }
    false
}
