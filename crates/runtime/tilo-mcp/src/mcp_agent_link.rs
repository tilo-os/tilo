//! Links an agent hands a person: `tilo_link` answers the link to what a path
//! names, and a call that changed something answers the link that opens what
//! it made or changed, which the server that invoked it minted with its
//! answer. Each is the resource's own `share-link`, minted where the document
//! is served, under the web entry its server announces: the link a person
//! copies in the app. It carries no secret, so it opens only for the workspace's
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

/// What a change answers beside its result: the link that opens what it made or changed, and,
/// when its window did not come up on the person's layout, why not. The change stands either way.
#[derive(Default)]
pub(super) struct Shown {
    pub(super) link: Option<String>,
    pub(super) not_shown: Option<Failure>,
}

/// Say what `shown` holds after what a call answered as text: its link, and why its window is not
/// on the person's layout.
pub(super) fn tell(reply: &mut Reply, shown: &Shown) {
    if let Some(url) = &shown.link {
        let _ = write!(reply.text, "\nlink: {url}");
    }
    if let Some(failure) = &shown.not_shown {
        let _ = write!(reply.text, "\nnot shown on the person's layout: {failure}");
    }
}

/// Say what `shown` holds after what a call answered as text and put it into its typed answer,
/// when it has one.
pub(super) fn linked(reply: &mut Reply, typed: Option<Value>, shown: Shown) -> Option<Value> {
    tell(reply, &shown);
    let mut typed = typed?;
    put(&mut typed, shown);
    Some(typed)
}

/// Put what `shown` holds into a call's typed answer: `link`, and `not_shown` as the refusal it
/// is (`{code, message}`, the refusal's own code when the open was refused).
pub(super) fn put(typed: &mut Value, shown: Shown) {
    let Some(fields) = typed.as_object_mut() else {
        return;
    };
    if let Some(url) = shown.link {
        fields.insert("link".into(), json!(url));
    }
    if let Some(failure) = shown.not_shown {
        let code = failure
            .host
            .as_ref()
            .map_or("not_shown", tilo_actions::remote::ActionCallError::code);
        fields.insert(
            "not_shown".into(),
            json!({"code": code, "message": failure.message}),
        );
    }
}

/// The link an invocation's answer names, which its server minted where it
/// was invoked: what opens what it made or changed.
fn link_of(value: &Value) -> Option<String> {
    value.get("link").and_then(Value::as_str).map(str::to_owned)
}

/// Documents a session keeps as shown at most: a session changing ever new
/// documents does not grow them without end.
const SHOWED_DOCUMENTS: usize = 256;

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

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// `tilo_link`: the link to the resource `args.path` names, or, when the
    /// resource offers none of its own, to the record it is in.
    pub(super) async fn agent_link(
        &self,
        args: LinkArgs,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let parsed = Address::parse(&args.path)?;
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
            return Err(tilo_actions::tree::Unresolved::NotRunning {
                name: app_of(&parsed),
                opens: true,
            }
            .to_string()
            .into());
        };
        let mut levels = Levels::new();
        let resolved = if pinned {
            self.tree
                .resolve_in(
                    &parsed,
                    app_id,
                    provider,
                    false,
                    None,
                    &mut levels,
                    usize::MAX,
                )
                .await?
        } else {
            self.tree
                .resolve_published(
                    &parsed,
                    app_id,
                    provider,
                    false,
                    None,
                    &mut levels,
                    usize::MAX,
                )
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
            let top = self.tree.level(provider, "", &mut levels).await?;
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
    /// which its answer names: where the person is sent to see it. Its window was put on the
    /// person's layout by the open itself ([`Self::show`]); a `start` starts the app without one.
    pub(super) async fn opened_link(&self, value: &Value) -> Shown {
        let Some(provider) = value
            .pointer("/update/output/value/provider")
            .and_then(Value::as_str)
            .filter(|_| succeeded(value))
        else {
            return Shown::default();
        };
        let link = match self.document_link(provider, serde_json::Map::new()).await {
            Ok(url) => Some(url),
            Err(error) => {
                tracing::debug!(%error, provider, "mcp: an opened document answered no link");
                None
            }
        };
        Shown {
            link,
            not_shown: None,
        }
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
    ) -> Shown {
        if descriptor.contract.kind == tilo_actions::ActionKind::Read {
            return Shown::default();
        }
        if succeeded(value) {
            return self.changed_shown(provider, value).await;
        }
        self.note_changing(asked, value, provider);
        Shown::default()
    }

    /// What a change of the document `provider` answers: the link its server
    /// minted with it ([`link_of`]), and its document's window shown on the
    /// person's layout ([`Self::show`]) the first time this session changes
    /// it. Once asked for, where the window stays (or that it was closed) is
    /// the person's: a session making many changes in one document opens it
    /// once.
    pub(super) async fn changed_shown(&self, provider: &str, value: &Value) -> Shown {
        if tilo_actions::remote::resource_parts(provider).is_none() {
            return Shown::default();
        }
        let first = {
            let mut showed = self.showed.lock_recover();
            if showed.len() >= SHOWED_DOCUMENTS {
                showed.clear();
            }
            showed.insert(provider.to_owned())
        };
        // Asked once: a window refused now (no layout yet) is said so on this
        // change, and not asked for again on every change after it.
        let not_shown = if first {
            self.show(provider, "").await.err()
        } else {
            None
        };
        Shown {
            link: link_of(value),
            not_shown,
        }
    }

    /// Show the record `provider` names as a window on the person's layout, as a member's
    /// open does: its app's `open` on it, a call of this call's step, so one run lists the write
    /// and the window it showed. The server leaves a document a window already shows where the people
    /// put it. A provider that is no record (a window of a device) names the app `app_id`'s own
    /// record; an empty `app_id` then names nothing.
    ///
    /// Answers at once: the refusal when the open was refused (typed), else that it was taken.
    /// Nothing waits for the window: the open is kept in the workspace, and whichever device
    /// holds the person's layout puts the window there, now or when it next comes up.
    pub(super) async fn show(&self, provider: &str, app_id: &str) -> Result<(), Failure> {
        use tilo_actions::remote::{
            APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, InvokeArgument, InvokeRequest,
            ReceiptStatus, application_action_target, application_resource_target,
        };
        let (app, record) = match tilo_actions::remote::resource_parts(provider) {
            Some((app, record)) => (app, record),
            None if !app_id.is_empty() => (app_id, ""),
            None => return Ok(()),
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
            signed: None,
        };
        let receipt = match self.host().invoke(&self.caller(), request).await {
            Some(Ok(receipt)) => receipt,
            Some(Err(error)) => return Err(error.into()),
            None => return Err(tilo_actions::remote::ActionCallError::Unanswered.into()),
        };
        match receipt.status {
            ReceiptStatus::Rejected { error } => Err(error.into()),
            _ => Ok(()),
        }
    }

    /// "Show this window" (`descriptor`, answered `value`) shows it to the person: its record opens
    /// on their layout when no window there shows it ([`Self::show`]), or the call fails with why
    /// it could not.
    pub(super) async fn show_activated(
        &self,
        descriptor: &ActionDescriptor,
        value: &Value,
        (provider, app_id): (&str, &str),
    ) -> Result<(), Failure> {
        let action = descriptor.address.action.as_str();
        let failed = value.pointer("/update/state").and_then(Value::as_str) == Some("failed");
        if action == tilo_actions::remote::APPLICATION_ACTIVATE_ACTION && !failed {
            self.show(provider, app_id).await?;
        }
        Ok(())
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
    pub(super) async fn changed_link_of(&self, invocation: &str, value: &Value) -> Shown {
        let Some(state) = value.pointer("/update/state").and_then(Value::as_str) else {
            return Shown::default();
        };
        if !matches!(state, "succeeded" | "failed" | "cancelled") {
            return Shown::default();
        }
        let Some(provider) = self.changing.lock_recover().remove(invocation) else {
            return Shown::default();
        };
        if state != "succeeded" {
            return Shown::default();
        }
        self.changed_shown(&provider, value).await
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
