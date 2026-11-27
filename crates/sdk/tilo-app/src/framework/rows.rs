//! Live external rows: what a surface shows that lives outside the
//! resource's document — processes, devices, containers, journal entries,
//! fetched catalogue rows, running jobs — published beneath the resource for
//! the palette, agents and scripts.
//!
//! The row itself is [`tilo_resource::row::LiveRow`], the builder a session
//! or an app reaches without the app framework; this module re-exports it and
//! adds how the framework's publication merges rows into the resource's
//! children.

pub use tilo_resource::row::LiveRow;

use collab_types::app_actions::{ApplicationLaunch, ApplicationOpen};
use tilo_actions::{ActionBinding, ActionSpec, ResourceSemanticIdentity, ResourceSpec};
use tilo_resource::Effect as _;
use tilo_ui_base::widget::ActionEmitter;

pub(crate) use tilo_resource::share::{outside_document, resolved_outside_document};

/// An application the shell lists, as the type its rows are items of: the
/// kind every app's application rows share.
pub struct Application;

impl tilo_resource::words::RowType for Application {
    const KIND: &'static str = "applications.app";
    const LABEL: &'static str = "Application";
}

/// Handing verified content to the app that owns it: the app's own typed
/// `open`, one shape for every app that passes content on — Files handing a
/// file to the app its bytes say opens it, Camera handing a capture to Photos
/// or Video Player.
#[derive(Clone, Debug)]
pub struct ContentHandoff {
    app: String,
    label: String,
    description: String,
}

impl ContentHandoff {
    /// The handoff to `app`, offered as `label` and saying `description`.
    pub fn new(
        app: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            app: app.into(),
            label: label.into(),
            description: description.into(),
        }
    }

    /// The handoff opening the content `hash` names, as the action `id` on the
    /// row of the entity that holds those bytes: immediate, taking nothing.
    #[must_use]
    pub fn spec(&self, id: &str, hash: &str) -> ActionSpec {
        self.spec_named(id, hash, None)
    }

    /// [`Self::spec`] with the file's name, where the one handing it over knows it
    /// (`recording.mkv`): bytes alone say an image is a PNG, never that a video is an MKV, so
    /// the app it opens in reads the kind and the container from the name.
    #[must_use]
    pub fn spec_named(&self, id: &str, hash: &str, name: Option<&str>) -> ActionSpec {
        let launch = ApplicationLaunch::with_content(self.app.clone(), hash.to_owned());
        let launch = match name {
            Some(name) => launch.named(name),
            None => launch,
        };
        launch.spec(id, &self.label, &self.description)
    }

    /// An emitter opening whatever content hash it is handed, in this app:
    /// for bytes that arrive after the control that asked for them.
    /// The emitter's launches are addressed to this app's row: a window that emits them
    /// publishes [`Self::destination`] so each one resolves where it is dispatched.
    pub fn emitter(&self, id: &str) -> Option<ActionEmitter<String>> {
        let spec = ApplicationLaunch::content_spec(&self.app, id, &self.label, &self.description);
        let row = self.row(spec.clone());
        ActionBinding::new(&row, &spec, |hash: String| {
            ApplicationOpen::Open {
                content_hash: Some(hash),
                name: None,
            }
            .arguments()
        })
        .ok()
        .map(|binding| ActionEmitter::new(ui_core::Bind::bound(binding)))
    }

    /// [`Self::emitter`] for a file: what it is handed is the file's content hash and its
    /// name, which says what its bytes cannot (that text is a note, or a table).
    pub fn file_emitter(&self, id: &str) -> Option<ActionEmitter<(String, String)>> {
        let spec = ApplicationLaunch::content_spec(&self.app, id, &self.label, &self.description);
        let row = self.row(spec.clone());
        ActionBinding::new(&row, &spec, |(hash, name): (String, String)| {
            ApplicationOpen::Open {
                content_hash: Some(hash),
                name: Some(name),
            }
            .arguments()
        })
        .ok()
        .map(|binding| ActionEmitter::new(ui_core::Bind::bound(binding)))
    }

    /// The app's row [`Self::emitter`]'s launches name, offering its open as `id`.
    #[must_use]
    pub fn destination(&self, id: &str) -> ResourceSpec {
        let spec = ApplicationLaunch::content_spec(&self.app, id, &self.label, &self.description);
        self.row(spec).into_resource()
    }

    fn row(&self, open: ActionSpec) -> LiveRow {
        LiveRow::typed::<Application>(format!("{}.app", self.app), self.label.clone())
            .operation(open)
    }
}

/// Merge `rows` by id into `children`: a row naming an existing child adds
/// the operations and rows that child lacks, and its presentation when the
/// child has none; any other row is appended. The first publisher of an id
/// keeps its label and kind.
pub(crate) fn merge(
    children: &mut Vec<ResourceSpec>,
    rows: impl IntoIterator<Item = ResourceSpec>,
) {
    for row in rows {
        match children.iter_mut().find(|child| child.id == row.id) {
            Some(existing) => join(existing, row),
            None => children.push(row),
        }
    }
}

/// Add to `existing` the presentation, semantic identity, operations, views and rows of `row`,
/// which names it, that it lacks.
pub(crate) fn join(existing: &mut ResourceSpec, row: ResourceSpec) {
    if existing.presentation.is_none() {
        existing.presentation = row.presentation;
    }
    if existing.semantic_identity.is_none() {
        existing.semantic_identity = row
            .semantic_identity
            .filter(ResourceSemanticIdentity::is_valid);
    }
    for action in row.actions {
        if !existing.actions.iter().any(|known| known.id == action.id) {
            existing.actions.push(action);
        }
    }
    let mut views_added = false;
    for view in row.views {
        if !existing.views.iter().any(|known| known.id == view.id) {
            existing.views.push(view);
            views_added = true;
        }
    }
    if views_added {
        // The link actions name the views they were offered with.
        tilo_resource::share::reoffer(existing);
    }
    merge(&mut existing.children, row.children);
}

pub(crate) fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

pub use tilo_resource::status::{apply_row_statuses, apply_statuses};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_opens_the_content_it_is_given() {
        let handoff = ContentHandoff::new("so.tilo.photos", "Open in Photos", "Open the capture");
        let hash = "ab".repeat(32);
        let spec = handoff.spec("open", &hash);
        assert_eq!(spec.id, "open");
        let encoded = spec
            .encode_immediate()
            .expect("a handoff bound to its content takes nothing");
        let payload: serde_json::Value =
            serde_json::from_slice(&encoded.payload).expect("the launch is its payload");
        assert_eq!(payload["app_id"].as_str(), Some("so.tilo.photos"));
        assert_eq!(payload["content_hash"].as_str(), Some(hash.as_str()));
        assert!(
            handoff.emitter("open").is_some(),
            "the same handoff emits content that arrives later"
        );
    }

    tilo_test_env::scenario! {
        fn a_live_row_keeps_its_semantic_identity;

        phase merging_retains_the_selector_identity_without_overwriting_the_first_publisher;
        {
            let identity = ResourceSemanticIdentity::new("chat.agent", "chat.agent:scribe").unwrap();
            let row = || {
                LiveRow::new("agent:1", "chat.agent", "Scribe")
                    .semantic_identity(identity.clone())
                    .into_resource()
            };
            let mut children = vec![ResourceSpec::new("agent:1", "chat.agent", "Document label")];
            merge(&mut children, [row()]);
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].semantic_identity, Some(identity.clone()));
            assert_eq!(children[0].label, "Document label");

            let first = ResourceSemanticIdentity::new("chat.agent", "chat.agent:reed").unwrap();
            children[0].semantic_identity = Some(first.clone());
            merge(&mut children, [row()]);
            assert_eq!(children[0].semantic_identity, Some(first));
        }

        phase live_row_retains_only_a_valid_typed_semantic_identity;
        {
            let identity = ResourceSemanticIdentity::new("chat.agent", "chat.agent:scribe")
                .expect("the bounded typed identity is valid");
            let resource = LiveRow::new("opaque:1", "chat.agent", "Scribe")
                .semantic_identity(identity.clone())
                .into_resource();
            assert_eq!(resource.semantic_identity, Some(identity));

            let invalid = ResourceSemanticIdentity {
                kind: "chat agent".into(),
                key: "chat.agent:scribe".into(),
            };
            assert!(
                LiveRow::new("opaque:2", "chat.agent", "Scribe")
                    .semantic_identity(invalid)
                    .into_resource()
                    .semantic_identity
                    .is_none(),
                "a live row never publishes malformed selector metadata"
            );
        }
    }
}
