//! Tube as a [`tilo_resource::Resource`]: likes, queue, subscriptions and
//! settings that sync are document state; the route on screen is View.

use std::collections::BTreeMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, RunMode, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction};
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource, SearchEntry};
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
        TubeModel {
            liked: on(&self.likes),
            queue: on(&self.queue),
            subscriptions: on(&self.subscriptions),
            settings: self.settings.clone(),
            ..Default::default()
        }
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
                action: None,
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
                        action: None,
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
        quality.clone_into(&mut shared.settings.preferred_quality);
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

/// The screenshot scenarios, each over the bundled sample catalogue: Home,
/// its empty and loading states, device sync, the palette, a playing watch
/// page, the narrow window with its rail summoned, and every screen.
pub(crate) const SCENARIOS: &[&str] = &[
    "home",
    "empty",
    "loading",
    "device-sync",
    "palette",
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

impl ResourceSurface<Tube> for TubeApp {
    fn present(&mut self, state: &Rc<TubeShared>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Tube>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.stage_scenario(scenario);
        }
    }

    // Edits bound to where the user is publish as hand-built rows of the
    // widget's own rather than as `bound_declarations`.
    fn publishes_document_actions(&self) -> bool {
        true
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

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;

    #[test]
    fn every_scenario_captures_over_the_sample_library() {
        let sample = crate::sample::model().subscriptions;
        for scenario in SCENARIOS {
            let app = TestApp::<TubeShell>::capture(Some(scenario))
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            let subscribed: Vec<String> = app
                .state()
                .subscriptions
                .into_iter()
                .filter_map(|(id, on)| on.then_some(id))
                .collect();
            let mut expected = sample.clone();
            expected.sort();
            assert_eq!(subscribed, expected, "{scenario} stages the sample library");
        }
    }
}
