//! The framework's `open-link` on a root: what a link of a scheme the app claims shows
//! ([`ResourceApp::open_link`](super::super::ResourceApp::open_link)).

use std::rc::Rc;

use tilo_actions::ResourceSpec;

use super::ResourceRoot;

/// Shows what a link names on a root, or says why it does not.
pub(crate) type OpenLink = Rc<dyn Fn(&str) -> Result<(), String>>;

/// `open-link`: show what a link of a scheme the app claims names. Every app's
/// resource publishes it; a shell hands a link to the app that claims its
/// scheme through it, the app running or not, and an app answers a link it
/// does not open with why.
pub(super) fn spec() -> tilo_actions::ActionSpec {
    use tilo_actions::remote::{
        APPLICATION_OPEN_LINK_ACTION, APPLICATION_OPEN_LINK_TYPE, OPEN_LINK_PARAM,
    };
    tilo_actions::ActionSpec::typed_operation(
        APPLICATION_OPEN_LINK_ACTION,
        "Open link",
        "Link to open",
        tilo_actions::OperationKind::View,
        APPLICATION_OPEN_LINK_TYPE,
        vec![tilo_actions::ActionParam::required(
            OPEN_LINK_PARAM,
            tilo_actions::ParamType::Text,
            "The link to show, of a scheme the app claims (geo:52.5,13.4)",
        )],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                APPLICATION_OPEN_LINK_TYPE,
                &serde_json::json!({ OPEN_LINK_PARAM: args.text(OPEN_LINK_PARAM) }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: APPLICATION_OPEN_LINK_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description("Show what a link names, in the app that claims its scheme.")
}

impl ResourceRoot {
    /// Show what a link names through `open`: the root publishes `open-link`.
    pub(crate) fn opening_links(mut self, open: OpenLink) -> Self {
        self.open_link = Some(open);
        self
    }

    /// `resource` with the framework's `open-link`, when the root shows links and the app
    /// publishes none of its own under the id: what every publication of the root carries.
    pub(crate) fn offer_open_link(&self, resource: ResourceSpec) -> ResourceSpec {
        let published = resource
            .actions
            .iter()
            .any(|existing| existing.id == tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION);
        if self.open_link.is_some() && !published {
            resource.with_action(spec())
        } else {
            resource
        }
    }

    /// Run the framework's `open-link`, when `op` is it: the app shows what the link names.
    pub(super) fn start_open_link(
        &self,
        op: &tilo_ui_base::host_operation::ResourceOperation<'_>,
    ) -> Result<Option<tilo_ui_base::host_operation::ResourceOperationStart>, String> {
        let request = op.request();
        let Some(open) = &self.open_link else {
            return Ok(None);
        };
        if request.type_id != tilo_actions::remote::APPLICATION_OPEN_LINK_TYPE {
            return Ok(None);
        }
        let args: serde_json::Value =
            tilo_actions::decode_json(request.payload).map_err(|error| error.to_string())?;
        let link = args
            .get(tilo_actions::remote::OPEN_LINK_PARAM)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "open-link needs the link to open".to_owned())?;
        open(link)?;
        Ok(Some(
            tilo_ui_base::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        ))
    }
}
