//! Choosing one of a widget's entries — a sidebar's destination, a tree's
//! node, a list's or a table's row — published as the widget's view action
//! `select`, so a person's click, a screen reader, a script and an agent
//! choose through the same action. What is chosen is the window's own
//! state, never an edit: the action is absent from history.
//!
//! The widget keeps a [`ViewChoice`] beside the app's handler. An agent's
//! choice arrives through the host as the operation [`ViewChoice::start`]
//! reads; the widget then shows the entry and hands it to the app, as it
//! does a person's choice at once: no host decides a view or records it.

use super::{ActionEmitter, ControlBindingMetadata};

pub(crate) const CHOICE_SELECT: &str = "tilo.ui.choice.select";

#[derive(serde::Deserialize)]
struct Payload {
    control: String,
    entry: String,
}

enum Choices {
    Labelled(Vec<(String, String)>),
    Keys(Vec<String>),
}

pub(crate) struct ViewChoice {
    id: String,
    label: &'static str,
    /// What the control is, as a person reads it ("Mailboxes"): the resource's
    /// published label. Never its id, which is an address, not a name.
    name: Option<String>,
    action: ActionEmitter<String>,
}

impl ViewChoice {
    #[expect(
        clippy::expect_used,
        reason = "the generated text parameter binds the entry it is handed"
    )]
    pub(crate) fn new(id: impl Into<String>, label: &'static str) -> Self {
        let id = id.into();
        let resource = Self::resource_of(&id, label, None, None);
        let action = tilo_actions::ActionBinding::new(&resource, &resource.actions[0], |entry| {
            serde_json::Map::from_iter([("entry".into(), serde_json::Value::String(entry))])
        })
        .expect("the select action is the resource's own");
        Self {
            id,
            label,
            name: None,
            action: ActionEmitter::new(action),
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn resource_named(
        &self,
        name: Option<&str>,
        entries: Vec<(String, String)>,
    ) -> tilo_actions::ResourceSpec {
        let name = name.map(str::to_owned).or_else(|| self.name.clone());
        Self::resource_of(&self.id, self.label, name, Some(Choices::Labelled(entries)))
    }

    pub(crate) fn resource(&self, entries: Vec<(String, String)>) -> tilo_actions::ResourceSpec {
        Self::resource_of(
            &self.id,
            self.label,
            self.name.clone(),
            Some(Choices::Labelled(entries)),
        )
    }

    pub(crate) fn resource_unlisted(&self) -> tilo_actions::ResourceSpec {
        Self::resource_of(&self.id, self.label, self.name.clone(), None)
    }

    pub(crate) fn resource_of_keys(&self, keys: Vec<String>) -> tilo_actions::ResourceSpec {
        Self::resource_of(
            &self.id,
            self.label,
            self.name.clone(),
            Some(Choices::Keys(keys)),
        )
    }

    #[expect(
        clippy::expect_used,
        reason = "the generated payload of two strings always serializes"
    )]
    fn resource_of(
        id: &str,
        label: &'static str,
        name: Option<String>,
        entries: Option<Choices>,
    ) -> tilo_actions::ResourceSpec {
        use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};
        let mut entry = ActionParam::required("entry", ParamType::Text, "The entry to choose");
        match entries {
            Some(Choices::Labelled(entries)) => entry = entry.with_labelled_choices(entries),
            Some(Choices::Keys(keys)) => entry = entry.with_choices(keys),
            None => {}
        }
        let control = id.to_owned();
        let spec = ActionSpec::typed_operation(
            "select",
            label,
            "Choose an entry",
            OperationKind::View,
            CHOICE_SELECT,
            vec![entry],
            move |args| {
                EncodedAction::encode_operation(
                    CHOICE_SELECT,
                    &serde_json::json!({
                        "control": control,
                        "entry": args.text("entry").unwrap_or_default(),
                    }),
                )
                .expect("generated choice payload is serializable")
            },
        )
        .for_audience(tilo_actions::Audience::Agents);
        let shown = name.filter(|name| !name.trim().is_empty());
        tilo_actions::ResourceSpec::new(
            format!("tilo.control:{id}"),
            "tilo.control.choice",
            shown.unwrap_or_else(|| label.to_owned()),
        )
        .with_action(spec)
    }

    /// The entry an operation addressed to this control chooses; `None`
    /// when it is not for it.
    ///
    /// # Errors
    /// Returns why the payload does not read.
    pub(crate) fn start(
        &self,
        op: &super::ResourceOperation<'_>,
    ) -> Result<Option<String>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        if request.kind != tilo_actions::OperationKind::View || request.type_id != CHOICE_SELECT {
            return Ok(None);
        }
        let payload: Payload =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        Ok((payload.control == self.id).then_some(payload.entry))
    }

    pub(crate) fn install_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        let _ = self.action.install_custody(domain, attachment);
    }

    pub(crate) fn emit(&mut self, entry: String) {
        if let Err(error) = self.action.emit(entry) {
            tracing::warn!(%error, control = %self.id, "a choice refused its entry");
        }
    }

    pub(crate) fn take(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action.take()
    }

    pub(crate) fn refresh_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.action.refresh_availability(allowed);
    }

    pub(crate) fn metadata(&self, control: String) -> ControlBindingMetadata {
        self.action.metadata(control)
    }
}

pub(crate) fn chosen() -> super::ResourceOperationStart {
    super::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
}
