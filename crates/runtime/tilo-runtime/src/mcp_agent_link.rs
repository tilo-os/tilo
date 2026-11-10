//! Links an agent hands a person: `tilo_link` answers the link to what a path
//! names, and a call that changed a record answers that record's link beside
//! its result. Each is the resource's own `share-link`, minted where the record
//! is served, under the web entry its host announces: the link a person copies
//! in the app. It carries no secret, so it opens only for the workspace's
//! members; anyone else meets the workspace's admission.

use super::*;
use tilo_actions::remote::SHARE_LINK_ACTION;

/// What `tilo_link` takes: the path, and how the link opens.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LinkArgs {
    path: String,
    mode: Option<String>,
}

/// The URL a `share-link` invocation answered, or why it made none.
fn url_of(value: &Value) -> Result<String, String> {
    match value.pointer("/update/state").and_then(Value::as_str) {
        Some("succeeded") => value
            .pointer("/update/output/value/url")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "the host answered no link".to_owned()),
        Some("failed") => Err(value
            .pointer("/update/error/message")
            .and_then(Value::as_str)
            .unwrap_or("the host made no link")
            .to_owned()),
        _ => Err("the host did not answer with a link in time".to_owned()),
    }
}

/// Whether an invocation's answer says it succeeded.
fn succeeded(value: &Value) -> bool {
    value.pointer("/update/state").and_then(Value::as_str) == Some("succeeded")
}

/// Say `link` after what a call answered as text.
pub(super) fn tell(reply: &mut Reply, link: Option<&str>) {
    if let Some(url) = link {
        let _ = write!(reply.text, "\nlink: {url}");
    }
}

/// Say `link` after what a call answered as text and put it into its typed
/// answer, when it has one.
pub(super) fn linked(
    reply: &mut Reply,
    typed: Option<Value>,
    link: Option<String>,
) -> Option<Value> {
    tell(reply, link.as_deref());
    let mut typed = typed?;
    put(&mut typed, link);
    Some(typed)
}

/// Put `link` into a call's typed answer.
pub(super) fn put(typed: &mut Value, link: Option<String>) {
    if let (Some(fields), Some(url)) = (typed.as_object_mut(), link) {
        fields.insert("link".into(), json!(url));
    }
}

/// The links of the steps of a batch, each once, in the order first made.
pub(super) fn step_links(steps: &[Value]) -> Vec<Value> {
    let mut links: Vec<Value> = Vec::new();
    for link in steps.iter().filter_map(|step| step.get("link")) {
        if link.is_string() && !links.contains(link) {
            links.push(link.clone());
        }
    }
    links
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// `tilo_link`: the link to the resource `args.path` names, or, when the
    /// resource offers none of its own, to the record it is in.
    pub(super) async fn agent_link(
        &self,
        args: LinkArgs,
        structured: bool,
    ) -> Result<Reply, String> {
        let parsed = parse_path(&args.path)?;
        if parsed.action.is_some() {
            return Err(format!(
                "a link names a resource, not an action: tilo_link {{path: \"{}\"}}",
                args.path.split('#').next().unwrap_or_default()
            ));
        }
        let Some((app_id, provider, pinned)) =
            self.acting_in(&parsed, SHARE_LINK_ACTION, None).await?
        else {
            return Err(Self::not_running_note(&parsed.app, true));
        };
        let resolved = if pinned {
            self.resolve_in(&parsed, app_id, provider, false, None)
                .await?
        } else {
            self.resolve_published(&parsed, app_id, provider, false, None)
                .await?
        };
        let mut arguments = serde_json::Map::new();
        if let Some(mode) = args.mode {
            arguments.insert("mode".into(), json!(mode));
        }
        let own = resolved
            .node
            .as_ref()
            .and_then(|_| {
                resolved
                    .actions
                    .iter()
                    .find(|action| action.address.action == SHARE_LINK_ACTION)
            })
            .map(|action| action.address.resource.clone());
        let url = match own {
            Some(resource) => {
                self.mint_link(&resolved.provider, resource, arguments)
                    .await?
            }
            None => self.record_link(&resolved.provider, arguments).await?,
        };
        let mut reply = Reply::ok(url.clone());
        if structured {
            reply.structured = Some(json!({
                "status": "ok",
                "path": resolved.path,
                "link": url,
            }));
        }
        Ok(reply)
    }

    /// The link to the record `provider` serves: its root's `share-link`. A
    /// record the workspace serves by its provider names its root by the
    /// empty resource; a window's root is found at its top level.
    pub(super) async fn record_link(
        &self,
        provider: &str,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<String, String> {
        let resource = if tilo_actions::remote::is_record_provider(provider) {
            String::new()
        } else {
            let mut levels = Levels::new();
            let top = self.level(provider, "", &mut levels).await?;
            top.children
                .iter()
                .filter(|node| node.parent.is_empty())
                .flat_map(|node| node.actions.iter())
                .find(|action| action.address.action == SHARE_LINK_ACTION)
                .map(|action| action.address.resource.clone())
                .ok_or("this record offers no link")?
        };
        self.mint_link(provider, resource, arguments).await
    }

    /// The link of the record an open of a hosted workspace's app opened,
    /// which its answer names: where the person is sent to see it.
    pub(super) async fn opened_link(&self, value: &Value) -> Option<String> {
        let provider = value
            .pointer("/update/output/value/provider")
            .and_then(Value::as_str)
            .filter(|_| succeeded(value))?;
        self.changed_link(provider).await
    }

    /// The link an action that ran on `provider` answers: a change to a
    /// record answers the link that shows it to a person, once it finished
    /// (`tilo_status` answers it for one still running); a read changed
    /// nothing to show.
    pub(super) async fn invoked_link(
        &self,
        asked: Option<&str>,
        value: &Value,
        provider: &str,
        descriptor: &ActionDescriptor,
    ) -> Option<String> {
        if descriptor.contract.kind == tilo_actions::ActionKind::Read {
            return None;
        }
        if succeeded(value) {
            return self.changed_link(provider).await;
        }
        self.note_changing(asked, value, provider);
        None
    }

    /// The link a record that a call just changed answers beside its result:
    /// `None` when the call changed no record or no link could be made, which
    /// never fails the call that did its work. Only a record the workspace
    /// serves by its provider (`record:<app>:<record>`) is one: a window of a
    /// person's desktop is not a record a link names.
    pub(super) async fn changed_link(&self, provider: &str) -> Option<String> {
        if !tilo_actions::remote::is_record_provider(provider) {
            return None;
        }
        match self.record_link(provider, serde_json::Map::new()).await {
            Ok(url) => Some(url),
            Err(error) => {
                tracing::debug!(%error, provider, "mcp: a changed record answered no link");
                None
            }
        }
    }

    /// Keep the record `provider` that the change `invocation` is still
    /// making, so `tilo_status` answers its link once it finished.
    fn note_changing(&self, invocation: Option<&str>, value: &Value, provider: &str) {
        /// Changes kept at most: a session that never asks after its changes
        /// does not grow this without end.
        const KEPT: usize = 256;
        let Some(id) = value
            .get("invocation_id")
            .and_then(Value::as_str)
            .or(invocation)
        else {
            return;
        };
        let finished = matches!(
            value.pointer("/update/state").and_then(Value::as_str),
            Some("succeeded" | "failed" | "cancelled")
        );
        if finished || !tilo_actions::remote::is_record_provider(provider) {
            return;
        }
        let mut changing = self.changing.lock_recover();
        if changing.len() >= KEPT {
            changing.clear();
        }
        changing.insert(id.to_owned(), provider.to_owned());
    }

    /// The link of the record the change `invocation` made, once `value`
    /// says it finished: `None` while it runs, when it failed, or when it
    /// was no change to a record.
    pub(super) async fn changed_link_of(&self, invocation: &str, value: &Value) -> Option<String> {
        let state = value.pointer("/update/state").and_then(Value::as_str)?;
        if !matches!(state, "succeeded" | "failed" | "cancelled") {
            return None;
        }
        let provider = self.changing.lock_recover().remove(invocation)?;
        if state != "succeeded" {
            return None;
        }
        self.changed_link(&provider).await
    }

    async fn mint_link(
        &self,
        provider: &str,
        resource: String,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<String, String> {
        let value = Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: provider.to_owned(),
                app_id: tilo_actions::remote::record_parts(provider).map(|(app, _)| app.to_owned()),
            },
            workspace: false,
            revision: None,
            resource,
            action: SHARE_LINK_ACTION.to_owned(),
            arguments,
            invocation_id: None,
            wait_ms: None,
        }))
        .await?;
        url_of(&value)
    }
}
