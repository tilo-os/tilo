//! A call of an action the caller's directory does not offer: told it is refused when the
//! workspace refuses it, sent with exactly the caller's arguments where one resource publishes
//! it, and told where actions are listed otherwise.

use super::resolved::Resolution;
use super::{
    ActionProvider, Addressed, CatalogueQuery, DEFAULT_WAIT_MS, Done, Failure, McpServer, OneCall,
    Reply, Resolved, Value, outcome, path_name,
};

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// The answer a caller whose rules keep a read off their directory is given: the app's
    /// refusal as not permitted, when it publishes a read of the target to others.
    pub(super) async fn refused_read_answer(
        &self,
        resolved: &Resolved,
    ) -> Result<Option<Reply>, String> {
        let Some(node) = resolved.node.as_ref() else {
            return Ok(None);
        };
        Ok(self
            .refused_read(&resolved.provider, &node.id)
            .await?
            .map(|why| Reply::failed(format!("not_permitted: {why}"))))
    }

    /// Why the app's own read of the resource `resource` at `provider` is not this caller's,
    /// when the caller's rules keep it off their directory and the workspace refuses it as not
    /// permitted; `None` when the app offers no read there either.
    async fn refused_read(&self, provider: &str, resource: &str) -> Result<Option<String>, String> {
        self.refused_when_asked(provider, resource, "read").await
    }

    /// How a call of `action`, which `resolved`'s directory does not offer this caller, ends
    /// (`error`, why it was not found there). Only an action nothing listed here offers by that
    /// name goes to the workspace, by its address and with exactly the arguments the caller
    /// sent, and the workspace decides it as it decides every call: refused for the caller's
    /// role, refused for what the caller sent, or done as asked, never with arguments nobody
    /// gave. Named on the app, the workspace is first asked which of the app's resources
    /// publish it, a lookup that does nothing: it goes only to the one that does, and when
    /// several do, nothing is done and the caller is told how to name each. One the workspace knows
    /// nowhere, offered twice, or whose name only begins another is the caller's to name
    /// better, and told where actions are listed.
    pub(super) async fn unoffered(
        &self,
        resolved: &Resolved,
        action: &str,
        error: &str,
        asked: (serde_json::Map<String, Value>, Option<String>, Option<u64>),
    ) -> Result<Resolution, Failure> {
        // Refused already, by the workspace's answer for the action named: told as it is.
        if error.starts_with("not_permitted: ")
            || error.starts_with(tilo_actions::Availability::PRIVATE_VIEW)
        {
            return Err(error.into());
        }
        let offered_nowhere = error.contains(&format!("has no action {action}"))
            && !error.contains("is another action");
        let not_listed = || Failure::from(format!("{error}; tilo_ls {} lists them", resolved.path));
        if !offered_nowhere {
            return Err(not_listed());
        }
        let resource = match resolved.node.as_ref() {
            Some(node) => node.id.clone(),
            None => match self.publishing(resolved, action).await?.as_slice() {
                [] => return Err(self.nothing_offered(resolved, action, not_listed()).await),
                [(resource, _)] => resource.clone(),
                several => {
                    // A caller that named the app alone named the app's own package resource:
                    // each action that takes one of the app's roots is published on it beside
                    // the root's own resource, and the package's row is the one an agent, a
                    // script or a device names. The root's row is the person's, in a window.
                    let package = format!("tl:{}", resolved.app_id);
                    if let Some((resource, _)) =
                        several.iter().find(|(resource, _)| *resource == package)
                    {
                        resource.clone()
                    } else {
                        // Named by the resource each publishes it on, which is what a caller
                        // passes to call one: two resources can be labelled alike, and a label
                        // names neither.
                        let named: Vec<String> = several
                            .iter()
                            .map(|(resource, label)| {
                                format!(
                                    "{}/{}#{action} ({label})",
                                    resolved.path,
                                    path_name(resource)
                                )
                            })
                            .collect();
                        return Err(format!(
                            "invalid_arguments: {} publishes {action} on {} resources; name one: {}",
                            resolved.path,
                            several.len(),
                            named.join(", ")
                        )
                        .into());
                    }
                }
            },
        };
        let (arguments, invocation_id, wait_ms) = asked;
        let call = OneCall {
            provider: Addressed {
                provider: resolved.provider.clone(),
                app_id: Some(resolved.app_id.clone()),
            },
            workspace: false,
            revision: None,
            resource,
            action: action.to_owned(),
            arguments,
            invocation_id,
            wait_ms,
        };
        match Box::pin(self.invoke_one(call)).await {
            Ok(value) => {
                let reply = outcome(&value);
                Ok(Resolution::Done(Box::new(Done::Launched {
                    value,
                    reply,
                    not_shown: None,
                })))
            }
            Err(why) if not_permitted(&why) => Err(format!("not_permitted: {why}").into()),
            Err(why)
                if matches!(
                    why.host,
                    Some(tilo_actions::remote::ActionCallError::UnknownAction)
                ) =>
            {
                Err(self.nothing_offered(resolved, action, not_listed()).await)
            }
            Err(why) => Err(why),
        }
    }

    /// The refusal of `action`, which nothing in `resolved`'s app offers this caller: that it
    /// is not offered to it, where the app declares it and the caller may list the app, else
    /// `otherwise`, as for an action that does not exist, coded as one.
    async fn nothing_offered(
        &self,
        resolved: &Resolved,
        action: &str,
        mut otherwise: Failure,
    ) -> Failure {
        if let Some(refusal) = self.not_offered_here(&resolved.app_id, action).await {
            return refusal.into();
        }
        if otherwise.host.is_none() && super::answer::coded(&otherwise.message).0 == "failed" {
            otherwise.message = format!("unknown_action: {}", otherwise.message);
        }
        otherwise
    }

    /// The resources of `resolved`'s app that publish `action` for this caller, each with its
    /// label, as the workspace answers the action named by its app alone: a lookup, never a
    /// call. One the workspace refuses this caller is answered as that refusal.
    async fn publishing(
        &self,
        resolved: &Resolved,
        action: &str,
    ) -> Result<Vec<(String, String)>, Failure> {
        let page = self
            .ask(CatalogueQuery {
                provider: Some(resolved.provider.clone()),
                action: Some(action.to_owned()),
                ..CatalogueQuery::default()
            })
            .await?;
        let mut publishing: Vec<(String, String)> = Vec::new();
        for row in page.rows {
            let Some(found) = row.action else { continue };
            let found = crate::refusal::not_allowed(found)?;
            if !publishing
                .iter()
                .any(|(resource, _)| *resource == found.address.resource)
            {
                publishing.push((found.address.resource, row.resource.label));
            }
        }
        Ok(publishing)
    }

    /// Why the workspace refuses `action` on `resource` at `provider` as not permitted, asked
    /// with no arguments; `None` for any other answer.
    async fn refused_when_asked(
        &self,
        provider: &str,
        resource: &str,
        action: &str,
    ) -> Result<Option<String>, String> {
        let call = OneCall {
            provider: Addressed {
                provider: provider.to_owned(),
                app_id: None,
            },
            workspace: false,
            revision: None,
            resource: resource.to_owned(),
            action: action.to_owned(),
            arguments: serde_json::Map::new(),
            invocation_id: None,
            wait_ms: Some(DEFAULT_WAIT_MS),
        };
        match Box::pin(self.invoke_one(call)).await {
            Err(why) if not_permitted(&why) => Ok(Some(why.to_string())),
            _ => Ok(None),
        }
    }
}

/// Whether the workspace refused a call as not the caller's: their role or grants keep it
/// from them.
fn not_permitted(failure: &Failure) -> bool {
    matches!(
        failure.host,
        Some(
            tilo_actions::remote::ActionCallError::NotPermitted
                | tilo_actions::remote::ActionCallError::Refused { .. }
        )
    )
}
