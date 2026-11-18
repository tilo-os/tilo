//! Invoking the action a path names: resolved once per session, and invoked
//! straight away the next time the session names the same path.

use super::*;

/// The action a path named, as resolving it found it: what the session's
/// next call of the same path invokes without resolving it again.
#[derive(Clone)]
pub(crate) struct ResolvedAction {
    pub(super) app_id: String,
    /// The provider it is invoked on.
    pub(super) provider: String,
    /// The path to the resource it is published on.
    pub(super) path: String,
    pub(super) descriptor: ActionDescriptor,
    /// Where what it changes is taken back ([`Resolved::history`]).
    pub(super) history: Option<String>,
}

/// Paths a session keeps resolved at most: a session naming ever new paths
/// does not grow them without end.
const RESOLVED_PATHS: usize = 256;

/// What resolving a call's path came to: the action to invoke (with the
/// arguments handed back), or the call already answered (an app started or
/// opened).
pub(super) enum Resolution {
    At(Box<ResolvedAction>, serde_json::Map<String, Value>),
    Done(Box<Done>),
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Do the action `path` names in the app that offers it.
    pub(super) async fn do_in_app(
        &self,
        path: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Done, Failure> {
        // A path this session invoked before goes straight to what it resolved
        // to: the call costs the invocation alone, however much the app holds.
        let known = self.resolved_paths.lock_recover().get(path).cloned();
        if let Some(known) = known {
            match self
                .invoke_at(&known, arguments.clone(), invocation.clone(), wait_ms)
                .await
            {
                Ok(value) => return self.invoked_at(known, value).await,
                Err(failure) if crate::confirm::needs_the_person(&failure) => return Err(failure),
                // Nothing was invoked: what the path named has gone or moved since, so
                // it is resolved again, once.
                Err(_) => {
                    self.resolved_paths.lock_recover().remove(path);
                }
            }
        }
        let (known, arguments) =
            match Box::pin(self.resolve_action(path, arguments, invocation.clone(), wait_ms))
                .await?
            {
                Resolution::Done(done) => return Ok(*done),
                Resolution::At(known, arguments) => (*known, arguments),
            };
        let value = self
            .invoke_at(&known, arguments, invocation, wait_ms)
            .await?;
        {
            let mut kept = self.resolved_paths.lock_recover();
            if kept.len() >= RESOLVED_PATHS {
                kept.clear();
            }
            kept.insert(path.to_owned(), known.clone());
        }
        self.invoked_at(known, value).await
    }

    /// Invoke the action `known` names with `arguments`: the server's answer,
    /// or why nothing was invoked.
    async fn invoke_at(
        &self,
        known: &ResolvedAction,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Value, Failure> {
        self.acted_in(&known.provider, &known.app_id);
        let arguments = instead::as_declared(&known.descriptor, arguments);
        Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: known.provider.clone(),
                app_id: Some(known.app_id.clone()),
            },
            workspace: false,
            revision: None,
            resource: known.descriptor.address.resource.clone(),
            action: known.descriptor.address.action.clone(),
            arguments,
            invocation_id: invocation,
            wait_ms,
        }))
        .await
    }

    /// How the action `known` names went, answering `value`.
    async fn invoked_at(&self, known: ResolvedAction, value: Value) -> Result<Done, Failure> {
        self.show_activated(&known.descriptor, &value, (&known.provider, &known.app_id))
            .await?;
        Ok(Done::Invoked {
            value,
            path: known.path,
            descriptor: Box::new(known.descriptor),
            app: known.app_id,
            provider: known.provider,
            history: known.history,
        })
    }
}
