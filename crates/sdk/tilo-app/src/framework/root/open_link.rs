//! The framework's `open-link` on a root: what a link of a scheme the app claims shows
//! ([`ResourceApp::open_link`](super::super::ResourceApp::open_link)).

use std::rc::Rc;

use collab_types::app_actions::{AppLink, OPEN_LINK, OPEN_LINK_OPERATION};
use tilo_actions::ResourceSpec;
use tilo_resource::Effect as _;

use super::ResourceRoot;

/// Shows what a link names on a root, or says why it does not.
pub(crate) type OpenLink = Rc<dyn Fn(&str) -> Result<(), String>>;

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
            .any(|existing| existing.id == OPEN_LINK);
        if self.open_link.is_some() && !published {
            resource.with_action(collab_types::app_actions::open_link_spec())
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
        if request.type_id != OPEN_LINK_OPERATION {
            return Ok(None);
        }
        let AppLink::OpenLink { link } =
            AppLink::from_payload(request.payload).map_err(|error| error.to_string())?;
        open(&link)?;
        Ok(Some(
            tilo_ui_base::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        ))
    }
}
