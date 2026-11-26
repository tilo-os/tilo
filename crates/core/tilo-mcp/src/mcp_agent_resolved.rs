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
pub(super) const RESOLVED_PATHS: usize = 256;

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
        // A caller naming an invocation is following the call it made: that call is answered
        // where it is, without resolving its action again, which the app serving the target now
        // may never publish. A founding call is answered by the person's services that started
        // after it, which publish no founding.
        if let Some(id) = invocation.as_deref().map(invocation_id).transpose()?
            && self.observed(id).await.is_some()
        {
            let value = self.settled(id, self.held_for(wait_ms)).await?;
            return Ok(Done::Launched {
                reply: outcome(&value),
                value,
                not_shown: None,
            });
        }
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
                // it is resolved again, once, and the app it was in looked up again.
                Err(_) => {
                    self.resolved_paths.lock_recover().remove(path);
                    self.tree.forget_app(&known.app_id);
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
        // What a caller wrote is answered as the forms the app takes: an
        // address where it picks a resource, a label where it chooses one.
        let arguments = self
            .bind_resource_arguments(&known.descriptor, arguments)
            .await?;
        self.acted_in(&known.provider, &known.app_id);
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

    /// The action `path` names, as its app publishes it now; or how the call
    /// ended when it named the app itself (started or opened it).
    async fn resolve_action(
        &self,
        path: &str,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Resolution, Failure> {
        let answered = |done: Done| Resolution::Done(Box::new(done));
        let parsed = Address::parse(path).map_err(|why| format!("invalid_arguments: {why}"))?;
        let action = parsed.action.clone().ok_or_else(|| {
            format!(
                "invalid_arguments: name the action: {path}#<action>; tilo_ls {path} lists them"
            )
        })?;
        // An app that is not running declares its actions in the surface its package
        // publishes: a name none of them has is no action of it, and nothing starts for it.
        // Answered here, the caller reads the refusal at once instead of after a launch.
        if below_app(&parsed).is_empty()
            && !starts_app(&action)
            && !self.on_workspace_shell(&parsed, &action)
            && let Ok(AppRef::Installed { app_id, .. }) = self.tree.app_ref(&app_of(&parsed)).await
            && self.declares_action(&app_id, &action).await == Some(false)
        {
            let name = app_of(&parsed);
            return Err(Failure {
                message: format!(
                    "unknown_action: {name} has no action {action}; tilo_ls {name} lists them"
                ),
                host: Some(tilo_actions::remote::ActionCallError::UnknownAction),
            });
        }
        // The app is looked up once: resolving the path in it does not ask
        // again which apps run.
        let Some((app_id, provider, pinned)) = self
            .acting_in(&parsed, &action, wait_ms)
            .await
            .map_err(|why| refused_as("not_found", why))?
        else {
            let AppRef::Installed { app_id, .. } = self.tree.app_ref(&app_of(&parsed)).await?
            else {
                return Err(format!("{} started while it was asked to", app_of(&parsed)).into());
            };
            return (self.launched((&parsed, &app_id, &action), arguments, invocation, wait_ms))
                .await
                .map(answered);
        };
        // A path names a type the way a listing does (`tasks.Task`), so an action is found on
        // the resource a listing showed: such a segment is read by the program form first, and
        // a form no app declares is read plainly, as a title may be.
        let forms = self.forms_named(&parsed).await;
        // What this session read of the app's directory answers the path where
        // it names the resource: a write on another of the app's resources
        // asks the workspace for nothing but the write.
        let mut levels = self.tree.levels_kept(&provider);
        let mut resolved = if pinned {
            self.tree
                .resolve_in(
                    &parsed,
                    app_id.clone(),
                    provider.clone(),
                    false,
                    typed(forms.as_ref()),
                    &mut levels,
                    usize::MAX,
                )
                .await
                .map_err(|why| refused_as("not_found", why.to_string()))?
        } else {
            self.tree
                .resolve_published(
                    &parsed,
                    app_id.clone(),
                    provider.clone(),
                    false,
                    typed(forms.as_ref()),
                    &mut levels,
                    usize::MAX,
                )
                .await
                .map_err(|why| refused_as("not_found", why.to_string()))?
        };
        if forms.is_some() && resolved.chain.is_empty() {
            resolved = if pinned {
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
        }
        self.tree.keep_levels(&resolved.provider, &resolved.levels);
        self.acted_in(&resolved.provider, &resolved.app_id);
        self.resolve_in_target(&parsed, &action, resolved, arguments, invocation, wait_ms)
            .await
    }

    /// Whether the installed app `app_id` declares `action` in the surface its package
    /// publishes: the actions its resources offer, and those that open a window. `None` when
    /// the workspace lists no surface for it here, or one that does not read.
    ///
    /// What this device and the caller's workspace answer for the app is asked, never the
    /// fleet's compiled copy of that package: the app installed here may be another version,
    /// and refusing an action the fleet's copy lacks would refuse one this app has.
    async fn declares_action(&self, app_id: &str, action: &str) -> Option<bool> {
        let surface = crate::offered::installed_surface(&self.tree, app_id).await?;
        let text = String::from_utf8_lossy(&surface);
        let file = collab_types::surface::SurfaceFile::from_json(text.as_ref()).ok()?;
        Some(crate::offered::declared(&file, action).is_some())
    }

    /// The action `action` in the target `resolved`: the action and the path
    /// to the resource it is published on, or how the call ended instead (its
    /// app started or launched, or the workspace refused it).
    async fn resolve_in_target(
        &self,
        parsed: &Address,
        action: &str,
        mut resolved: Resolved,
        arguments: serde_json::Map<String, Value>,
        invocation: Option<String>,
        wait_ms: Option<u64>,
    ) -> Result<Resolution, Failure> {
        let answered = |done: Done| Resolution::Done(Box::new(done));
        // The rows the path was resolved through carry the actions each resource
        // offers: the action named is run from what the workspace already
        // answered, and only a name none of them holds is asked of it again.
        let (named_at, descriptor) = if let Some(found) = delivered_action(&resolved, action) {
            (resolved.path.clone(), crate::refusal::not_allowed(found)?)
        } else if let Some(descriptor) = self.action_named(&resolved, action).await? {
            (resolved.path.clone(), descriptor)
        } else {
            // Not one of the target's actions by its id: what the target
            // offers decides (a label, the start of an id), as listing it
            // shows. A path naming the app alone means the app's own
            // actions and those it publishes on its resources.
            if let Some(node) = resolved.node.clone() {
                let mut levels = std::mem::take(&mut resolved.levels);
                let here = self
                    .tree
                    .level(&resolved.provider, &node.id, &mut levels)
                    .await?;
                resolved.levels = levels;
                resolved.actions = if here.own.is_empty() {
                    node.actions.clone()
                } else {
                    here.own
                };
            }
            // `notes#open` is what the listing of a stopped app says to
            // call; once the app runs, it answers what is in it.
            let opens = below_app(parsed).is_empty() && arguments.is_empty();
            // There it names the app's own verb, never an action whose id only begins with it
            // (Notes' `open-link`): one by its whole id was asked above.
            if opens && starts_app(action) {
                return self.running(&resolved, action).await.map(answered);
            }
            // With arguments the word that starts an app is no other action's either: it names
            // an action of the running app only where the app calls one that, by its id or its
            // label, never another whose id merely begins with it, which nobody asked for.
            let starts = below_app(parsed).is_empty() && starts_app(action);
            let picked = self.pick_action_in(action, &resolved).await;
            // Found, and refused below as the word that starts an app: the word only begins
            // the id of another action the app offers.
            let begins_another = starts && picked.is_ok();
            match named_beyond_its_start(starts, action, &resolved.path, picked) {
                Ok((at, found)) => (at, crate::refusal::not_allowed(found)?),
                // The open's own arguments are a file, which reaches the app's open whether
                // the app runs or not. Arguments that name no file are another action's where
                // the app offers one the word begins: the caller is told which, and nothing
                // is run.
                Err(_) if starts && (opens_a_file(&arguments) || !begins_another) => {
                    let opening = (parsed, resolved.app_id.as_str(), action);
                    return self
                        .launched(opening, arguments, invocation, wait_ms)
                        .await
                        .map(answered);
                }
                // Not offered here: the workspace decides whether it is the caller's role
                // that keeps it from them, as a call by its address is decided.
                Err(error) => {
                    let asked = (arguments, invocation, wait_ms);
                    return Box::pin(self.unoffered(&resolved, action, &error, asked)).await;
                }
            }
        };
        self.tree.keep_levels(&resolved.provider, &resolved.levels);
        let history = resolved.history();
        let known = ResolvedAction {
            app_id: resolved.app_id,
            provider: resolved.provider,
            path: named_at,
            descriptor,
            history,
        };
        Ok(Resolution::At(Box::new(known), arguments))
    }
}

/// The action `action` among the actions the rows `resolved` read delivered for
/// its target, when they hold it: what the workspace answered with the path,
/// without asking it again.
fn delivered_action(resolved: &Resolved, action: &str) -> Option<ActionDescriptor> {
    let resource = resolved.node.as_ref().map(|node| node.id.as_str());
    resolved
        .actions
        .iter()
        .chain(resolved.node.iter().flat_map(|node| node.actions.iter()))
        .find(|descriptor| {
            descriptor.address.action == action
                && resource.is_none_or(|resource| descriptor.address.resource == resource)
        })
        .cloned()
}
