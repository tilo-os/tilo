//! Links an agent hands a person: `tilo_link` answers the link to what a path
//! names, and a call that changed something answers the link that opens what
//! it made or changed (the resources its answer names after `->`), else its
//! record's. Each is the resource's own `share-link`, minted where the record
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

/// The resources an invocation's answer says it made or changed, in the
/// order it names them: the `-> <resource>` lines under its result.
fn named(value: &Value) -> Vec<&str> {
    value
        .pointer("/update/output/resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|resource| !resource.is_empty())
        .collect()
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
    ) -> Result<Reply, Failure> {
        let parsed = parse_path(&args.path)?;
        if parsed.action.is_some() {
            return Err(format!(
                "a link names a resource, not an action: tilo_link {{path: \"{}\"}}",
                args.path.split('#').next().unwrap_or_default()
            )
            .into());
        }
        let Some((app_id, provider, pinned)) =
            self.acting_in(&parsed, SHARE_LINK_ACTION, None).await?
        else {
            return Err(Self::not_running_note(&parsed.app, true).into());
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
            None => self.document_link(&resolved.provider, arguments).await?,
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
    pub(super) async fn document_link(
        &self,
        provider: &str,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<String, String> {
        let resource = if tilo_actions::remote::is_resource_provider(provider) {
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

    /// The link an action that ran on `provider` answers: a change answers
    /// the link that opens what it made or changed to a person, once it
    /// finished (`tilo_status` answers it for one still running); a read
    /// changed nothing to show.
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
            return self.changed_link_naming(provider, &named(value)).await;
        }
        self.note_changing(asked, value, provider);
        None
    }

    /// The link a record that a call just changed answers beside its result,
    /// once its window is shown on the person's layout ([`Self::show`]): `None`
    /// when the call changed no record or no link could be made, which never
    /// fails the call that did its work. Only a record the workspace
    /// serves by its provider (`record:<app>:<record>`) is one: a window of a
    /// person's desktop is not a record a link names.
    pub(super) async fn changed_link(&self, provider: &str) -> Option<String> {
        self.changed_link_naming(provider, &[]).await
    }

    /// The link a change answers: the own `share-link` of the first of
    /// `named` (the resources its answer says it made or changed, as
    /// `-> <resource>` lines name them) that offers one, which opens that
    /// resource; else the record's own link. The record's window is shown on
    /// the person's layout either way ([`Self::show`]).
    pub(super) async fn changed_link_naming(
        &self,
        provider: &str,
        named: &[&str],
    ) -> Option<String> {
        tilo_actions::remote::resource_parts(provider)?;
        self.show(provider, "").await;
        for resource in named {
            if let Some(url) = self.own_link(provider, resource).await {
                return Some(url);
            }
        }
        match self.document_link(provider, serde_json::Map::new()).await {
            Ok(url) => Some(url),
            Err(error) => {
                tracing::debug!(%error, provider, "mcp: a changed record answered no link");
                None
            }
        }
    }

    /// The link `resource` offers of its own on `provider`: its
    /// `share-link`, present when it declares a view (an item the app opens
    /// does, which the answer's `->` names), asked of the app by its address:
    /// a note just made is found without listing the notebook it is in.
    /// `None` when it offers none, so the caller answers the record's link
    /// instead.
    async fn own_link(&self, provider: &str, resource: &str) -> Option<String> {
        if resource.is_empty() {
            return None;
        }
        let page = self
            .ask(CatalogueQuery {
                provider: Some(provider.to_owned()),
                target: Some(tilo_actions::ActionAddress::new(
                    resource.to_owned(),
                    SHARE_LINK_ACTION,
                )),
                max_results: 1,
                ..CatalogueQuery::default()
            })
            .await
            .ok()?;
        let own = page
            .rows
            .into_iter()
            .filter_map(|row| row.action)
            .find(|action| {
                action.address.resource == resource && action.address.action == SHARE_LINK_ACTION
            })?
            .address
            .resource;
        match self.mint_link(provider, own, serde_json::Map::new()).await {
            Ok(url) => Some(url),
            Err(error) => {
                tracing::debug!(%error, %resource, "mcp: a changed resource answered no link");
                None
            }
        }
    }

    /// Show the record `provider` names as a window on the person's layout, as a member's
    /// open does: its app's `open` on it, a call of this call's step, so one run lists the write
    /// and the window it showed. The host leaves a record a window already shows where the people
    /// put it. A provider that is no record (a window of a device) names the app `app_id`'s own
    /// record; an empty `app_id` then names nothing. Whether it showed is no matter to the call,
    /// which answers once the host took the open and never waits for the window: a record the host
    /// cannot show is still what the agent changed, and each page watching shows the change in its
    /// own way.
    pub(super) async fn show(&self, provider: &str, app_id: &str) {
        use tilo_actions::remote::{
            APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, InvokeArgument, InvokeRequest,
            application_action_target, application_resource_target,
        };
        let (app, record) = match tilo_actions::remote::resource_parts(provider) {
            Some((app, record)) => (app, record),
            None if !app_id.is_empty() => (app_id, ""),
            None => return,
        };
        let resource = format!("{APPLICATION_RESOURCE_PREFIX}{app}");
        let target = if record.is_empty() {
            application_action_target(&self.workspace, app, resource, APPLICATION_OPEN_ACTION)
        } else {
            application_resource_target(
                &self.workspace,
                app,
                record,
                resource,
                APPLICATION_OPEN_ACTION,
            )
        };
        let invocation = tilo_actions::InvocationId::new();
        let request = InvokeRequest {
            invocation,
            target,
            argument: InvokeArgument::None,
            // A call of this call's step, so one run lists what the call changed and the window
            // it showed. The open takes no step of its own: what it writes is the window's place,
            // which the run's undo leaves alone.
            context: self.context().map(|context| tilo_actions::ActionContext {
                group: context.group,
                gesture: context.gesture,
                ..tilo_actions::ActionContext::default()
            }),
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let shown = match self.host().invoke(&self.caller(), request).await {
            Some(Ok(receipt)) => Ok(receipt.status),
            Some(Err(error)) => Err(error.to_string()),
            None => Err("the host did not answer".to_owned()),
        };
        tracing::debug!(
            ?shown,
            provider,
            app,
            "mcp: a window asked for on the layout"
        );
    }

    /// "Show this window" (`descriptor`, answered `value`) shows it to the person: its record opens
    /// on their layout when no window there shows it ([`Self::show`]).
    pub(super) async fn show_activated(
        &self,
        descriptor: &ActionDescriptor,
        value: &Value,
        resolved: &super::Resolved,
    ) {
        let action = descriptor.address.action.as_str();
        let failed = value.pointer("/update/state").and_then(Value::as_str) == Some("failed");
        if action == tilo_actions::remote::APPLICATION_ACTIVATE_ACTION && !failed {
            self.show(&resolved.provider, &resolved.app_id).await;
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
        if finished || !tilo_actions::remote::is_resource_provider(provider) {
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
        self.changed_link_naming(&provider, &named(value)).await
    }

    async fn mint_link(
        &self,
        provider: &str,
        resource: String,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<String, String> {
        let value =
            Box::pin(
                self.invoke_one(OneCall {
                    provider: Addressed {
                        provider: provider.to_owned(),
                        app_id: tilo_actions::remote::resource_parts(provider)
                            .map(|(app, _)| app.to_owned()),
                    },
                    workspace: false,
                    revision: None,
                    resource,
                    action: SHARE_LINK_ACTION.to_owned(),
                    arguments,
                    invocation_id: None,
                    wait_ms: None,
                }),
            )
            .await?;
        url_of(&value)
    }
}
