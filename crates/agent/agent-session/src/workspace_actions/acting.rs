//! Claiming an action and performing it: what a worker does with a job
//! once it knows the workspace it is for.

#[allow(
    clippy::wildcard_imports,
    reason = "claiming and performing an action name the worker's own types"
)]
use super::*;

impl Worker {
    #[allow(
        clippy::result_large_err,
        reason = "a claim that cannot be made is the receipt answered"
    )]
    pub(super) fn claim(
        &self,
        invocation: InvocationId,
    ) -> Result<(Caller, InvokeRequest), InvocationReceipt> {
        let mut log = self.log();
        let claimed = match log.entries.get(&invocation) {
            None => Err(rejected(invocation, ActionCallError::UnknownInvocation)),
            Some(entry) if entry.update.is_terminal() => {
                // Cancelled before it started.
                Err(InvocationState::new(invocation, entry.update.clone()).receipt())
            }
            Some(entry) => Ok((entry.caller.clone(), entry.request.clone())),
        };
        if claimed.is_ok() {
            log.running.insert(invocation);
        }
        claimed
    }

    /// The appearance `request` may show: none, as no caller may show
    /// another name.
    pub(super) fn admit_appearance(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Option<tilo_actions::Appearance>, ActionCallError> {
        let Some(appearance) = &request.appearance else {
            return Ok(None);
        };
        let link = self.shared.link.get();
        let admitted = tilo_actions::appearance::admit(
            caller,
            appearance,
            |_| false,
            |hash| link.and_then(|link| link.content_blob(workspace_id, hash)),
        )?;
        if provider_workspace(&request.target.provider).is_some() {
            return Err(ActionCallError::Refused {
                reason: "an appearance is shown only on a record's own actions".into(),
            });
        }
        Ok(Some(admitted))
    }

    /// `target`, its empty resource naming the record's own: a caller that
    /// knows the app but not its code (the desktop asking a connector) names
    /// its actions so.
    pub(super) fn with_document_resource(
        &self,
        workspace_id: WorkspaceId,
        target: &tilo_actions::remote::RemoteActionTarget,
        app_id: &str,
    ) -> tilo_actions::remote::RemoteActionTarget {
        let mut target = target.clone();
        if !target.address.resource.is_empty() {
            return target;
        }
        if let Some(definition) = self.catalogue.get(app_id) {
            definition
                .resource_type
                .as_str()
                .clone_into(&mut target.address.resource);
        } else if let Some(surface) = self.packaged_surface(workspace_id, app_id)
            && let Some(resource) = packaged::resource_of(&surface, &target.address.action)
        {
            resource.kind.clone_into(&mut target.address.resource);
        }
        target
    }

    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one request dispatched in the order it is admitted through: appearance, keys, the document, action, save"
    )]
    /// Invocation `request`: what it answered, and where it left its actor in the
    /// record it changed.
    pub(super) fn perform(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(ActionOutput, Option<ActorPlace>), Outcome> {
        let appearance = self
            .admit_appearance(workspace_id, caller, request)
            .map_err(Outcome::Rejected)?;
        if let Some((named, app)) = application_provider(&request.target.provider) {
            return self
                .show_on_layout(workspace_id, caller, (named, app), request)
                .map(|output| (output, None));
        }
        if let Some(named) = provider_workspace(&request.target.provider)
            && WorkspaceId::parse(named) != Some(workspace_id)
        {
            return Err(unknown_action());
        }
        // Acting on a record waits for that record's keys and no other's;
        // opening an app on a new record needs only the workspace, and on the
        // record it keeps per workspace, the host's answer for that record
        // ([`Self::invoke_keys`], which the service's thread admitted this
        // request by).
        let keys = self.invoke_keys(request);
        let refreshing = timing_start();
        let state = self
            .refresh(workspace_id, keys.needs())
            .map_err(Outcome::Rejected)?;
        timing_done("action.refresh", refreshing);
        // A page names its source's revision: the workspace's, or a registered
        // read's committed source (checked where that source is read, below).
        let workspace = state.workspace.clone();
        let target = &request.target;
        let owner = self.serves_as_owner(caller);
        if provider_workspace(&target.provider).is_some() {
            if state.stale(request.revision.as_deref()) {
                return Err(Outcome::Rejected(ActionCallError::StaleRevision));
            }
            return self.open_in_workspace(workspace_id, caller, request, &state);
        }
        let workspace = workspace.ok_or_else(unknown_action)?;
        let route = resource_of(&target.provider, &workspace).ok_or_else(unknown_action)?;
        // A document an app keeps for its whole workspace is reached through the record the app
        // keeps, which publishes it beside itself: mounted over that, its verbs are the app's own
        // to run, and what they write lands on the document's route.
        let (route, state) =
            match self.holder_of_workspace_document(workspace_id, caller, &route, state.clone()) {
                Some((holder, state)) => (holder, state),
                None => (route, state),
            };
        // The package of the version `workspace` installs, when this device
        // keeps it: an installed app's action is applied from the model its
        // package carries, whichever device built the app, because that is the
        // same module every holder of the workspace replays it with.
        let packaged = self.installed_package(workspace_id, &route.app_id);
        if self.catalogue.get(&route.app_id).is_none() && packaged.is_none() {
            return Err(unknown_action());
        }
        let target = &self.with_document_resource(workspace_id, target, &route.app_id);
        let arguments = invoke_arguments(&request.argument);
        // What is said in a direct conversation the caller's person takes no part in is not
        // there for them: an action on it is answered as one on nothing, and no read answers
        // with a row of it ([`direct`]).
        let withheld = self.kept_from_caller(caller, &route, &state);
        if let Some(package) = packaged {
            if state.stale(request.revision.as_deref()) {
                return Err(Outcome::Rejected(ActionCallError::StaleRevision));
            }
            let performed = self.perform_packaged(
                workspace_id,
                caller,
                request,
                &route,
                &package,
                (target, arguments, &withheld),
            )?;
            return Ok((withheld.shown(performed.0), performed.1));
        }
        // What the record publishes, its own API runs. What only its
        // committed source publishes — a canonical read of it — is asked of
        // that source, which reads the record and opens a worker for it: an
        // action the record publishes never pays for that.
        let holder = self.holder_of(&route);
        // A caller served within bounds acts through a replica of their own:
        // what the document publishes is asked of it, and this device's own
        // mount is neither brought up to date nor opened for them. Catching that up
        // on every write since it was last used cost a member's first call
        // after another app's writes most of a second.
        let bounded_caller = (!owner).then(|| self.bounded(caller)).flatten();
        let published_here = if let Some(who) = &bounded_caller {
            let describing = timing_start();
            let described = self
                .bounded_descriptor(workspace_id, who, (&route, &state), &target.address)
                .map_err(Outcome::Rejected)?
                .is_some();
            timing_done("action.descriptor", describing);
            described
        } else {
            // What the document publishes now, with what was written since it
            // was last used taken up: an item made since is one of its
            // resources.
            self.brought_up_to_date(&holder, &state);
            // Parts of its chains on their way: it waits off this thread for them.
            if self.hold_for_chains() {
                return Err(api_outcome(&part_wait::not_ready()));
            }
            self.publishes(&holder, &target.address)
        };
        if let Some(output) = self.source_read(workspace_id, caller, request, published_here)? {
            self.note_reads(request);
            return Ok((withheld.shown(output), None));
        }
        if state.stale(request.revision.as_deref()) {
            return Err(Outcome::Rejected(ActionCallError::StaleRevision));
        }
        withheld.refuses(&target.address.resource)?;
        let share_link = target.address.action == ui::tilo_resource::share::SHARE_LINK;
        if !owner && !share_link {
            let who = bounded_caller.ok_or_else(unknown_action)?;
            if appearance.is_some() {
                return Err(unknown_action());
            }
            return self
                .perform_bounded(
                    workspace_id,
                    caller,
                    &who,
                    (request, &route, &state),
                    (target, arguments, &withheld),
                )
                .map(|(output, left_at)| (withheld.shown(output), left_at));
        }
        let mounting = timing_start();
        self.mount(&route, &state).map_err(|_| unknown_action())?;
        if self.hold_for_chains() {
            return Err(api_outcome(&part_wait::not_ready()));
        }
        // A part this request was the first to open is held beside the
        // document its app keeps now, although `holder` was read before it was
        // opened: the action is answered from that document, which holds the
        // part.
        let holder = self.holder_of(&route);
        let mounted = self.mounted.get_mut(&holder).ok_or_else(unknown_action)?;
        timing_done("action.mount", mounting);
        if share_link {
            // Sharing asks the record for everything it lists, which is what
            // the link is chosen from.
            // The kind of the resource the link names, from the action
            // asked for, resolved by its address: never a survey of all the
            // record publishes, which grows with everything it holds.
            let kind = mounted
                .app
                .descriptor(&target.address)
                .map(|descriptor| descriptor.resource_kind)
                .ok_or_else(unknown_action)?;
            let at = (&route, &state, kind);
            self.note_reads(request);
            return self
                .share_link(workspace_id, caller, at, target, arguments)
                .map(|output| (output, None));
        }
        // The one action asked for, not the whole listing: a resource of ten
        // thousand items costs this what one of ten does. A request applied again after
        // handing its operation off the thread answers from the action as it was published
        // then: the operation may have removed the item it named (a delete), and the resource
        // has no such action now.
        let descriptor = mounted
            .app
            .descriptor(&target.address)
            .or_else(|| mounted.app.handed_descriptor(request.invocation))
            .ok_or_else(unknown_action)?;
        if descriptor.contract.kind == ActionKind::Read {
            self.note_reads(request);
        }
        let arguments = withheld.given(descriptor.bind_prompt_text(arguments))?;
        let confirmed = self.confirmed_here(workspace_id, caller, request, &descriptor);
        let holder = self.holder_of(&route);
        let context = caller_context(caller, request);
        let running = timing_start();
        let output = self
            .invoke_on(
                workspace_id,
                &holder,
                keys.needs(),
                &Invoked {
                    invocation: request.invocation,
                    address: &target.address,
                    arguments: &arguments,
                    context: Some(&context),
                    appearance: appearance.as_ref(),
                    confirmed,
                    descriptor: &descriptor,
                },
            )
            .map_err(|error| api_outcome(&error))?;
        timing_done("action.call", running);
        let mounted = self.mounted.get_mut(&holder).ok_or_else(unknown_action)?;
        // Where the action left its actor, for the API that shows it
        // working: the text it wrote, the item it acted on.
        let left_at = mounted.app.presence().map(bounded::placed);
        let taking = timing_start();
        let taken = take_all(&mounted.app, &holder)
            .map_err(|error| commit_failed(error.to_string(), false))?;
        timing_done("action.take", taking);
        let committing = timing_start();
        self.commit_taken(workspace_id, &route, taken)
            .map_err(|reason| commit_failed(reason, true))?;
        timing_done("action.commit", committing);
        Ok((withheld.shown(output), left_at))
    }

    /// The answer the committed source gives `request`, when what answers here
    /// does not publish that action itself: one it publishes never pays to
    /// open a worker for it.
    #[allow(
        clippy::result_large_err,
        reason = "a failure here is the action API's Outcome, handed back to its caller unchanged"
    )]
    pub(super) fn source_read(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        published_here: bool,
    ) -> Result<Option<ActionOutput>, Outcome> {
        if published_here {
            return Ok(None);
        }
        self.try_registered_read(workspace_id, caller, request)
            .map_err(|error| *error)
    }

    /// Whether this call is confirmed as the action's own contract asks and
    /// the caller answered: a token its person's device issued for this very
    /// call, taken once.
    pub(super) fn confirmed_here(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> bool {
        let binding = bounded::ApprovalBinding::of(workspace_id, caller, request);
        self.part_waits.confirmed_or(|| {
            confirms(
                &descriptor.contract.confirmation,
                request.confirmation.as_ref(),
                |token| self.shared.approvals.take(token, &binding),
            )
        })
    }

    /// Open an app on a new record of the workspace `named`, as its
    /// workspace's own action asks (`app.<app>#open`), in the workspace the
    /// session is in: for the host's own person as its owner, and for anyone
    /// else, a member or an agent of anyone's, within their member access there
    /// now.
    ///
    /// Called only from [`Self::open_in_workspace`] (reached only through
    /// [`Self::perform`], which has already checked that the target's named
    /// workspace parses to `workspace_id`).
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    pub(super) fn open_asked(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Result<ActionOutput, Outcome> {
        let who = if self.serves_as_owner(caller) {
            None
        } else {
            Some(self.bounded(caller).ok_or_else(unknown_action)?)
        };
        let app = self.asked_app(target).ok_or_else(unknown_action)?;
        self.open_new(workspace_id, &app, who.as_ref())
    }

    /// The app a workspace's `app.<app>#open` (or `#start`) names, when this
    /// host runs it.
    pub(super) fn asked_app(
        &self,
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Option<String> {
        target
            .address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            // Its records have no window to show: starting the app is opening it.
            .filter(|_| {
                target.address.action == APPLICATION_OPEN_ACTION
                    || target.address.action == APPLICATION_START_ACTION
            })
            // Files is the workspace's own folder, not an app this host runs.
            .filter(|app| self.catalogue.has(app) || *app == fs_protocol::tree::FILES_APP)
            .map(str::to_owned)
    }

    /// Commit what the owner's action changed, each record on its own
    /// route: a part it started is held beside the record it acted on. The
    /// action is done once the host holds them ([`Self::saving`]). The error
    /// is why a commit failed, and the mounted copy is let go with it.
    pub(super) fn commit_taken(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        taken: Vec<(DocumentRoute, DocumentMutation)>,
    ) -> Result<(), String> {
        let holder = self.holder_of(route);
        let wrote: Vec<DocumentRoute> = taken.iter().map(|(route, _)| route.clone()).collect();
        for (changed, mutation) in taken {
            if changed != holder {
                self.beside.insert(changed.clone(), holder.clone());
            }
            if let Err(reason) = self.commit(workspace_id, &changed, &mutation) {
                // The mounted copy made the change and the session was not handed it: the
                // copy is let go, so the next action mounts what the workspace holds. Kept,
                // the next change made in it would build on one no device ever sends, and
                // wait for it for good.
                self.mounted.remove(&holder);
                self.beside.retain(|_, kept| *kept != holder);
                return Err(reason);
            }
        }
        if let Some(mounted) = self.mounted.get_mut(&holder) {
            caught_up_with_own(&holder, mounted, &wrote);
        }
        self.behind_all_but(&Writer::Mounted(holder), &wrote);
        Ok(())
    }

    /// The record of `app` that holds the document at `route` in a listing, with the state it is
    /// held in: a document an app keeps for its whole workspace
    /// ([`collab_types::WORKSPACE_DOCUMENTS`], Terminal's machine secrets) is published by the
    /// app's replica over the record the app keeps, never mounted on its own. `None` for a route
    /// that is no such document.
    ///
    /// The app's own record is founded as `caller` when the workspace holds none, so the first
    /// caller naming one of the app's documents reaches what the app declares there: the record
    /// the app keeps is where those rows stand.
    pub(super) fn holder_of_workspace_document(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
        state: super::WorkspaceState,
    ) -> Option<(DocumentRoute, super::WorkspaceState)> {
        collab_types::workspace_document(&route.app_id, &route.document)?;
        if let Some(holder) = self.holding_record(route, &state) {
            return Some((holder, state));
        }
        let who = if self.acts_for_owner(caller) {
            None
        } else {
            self.bounded(caller)
        };
        self.open_new(workspace_id, &route.app_id, who.as_ref())
            .ok()?;
        let state = self.refresh(workspace_id, Needs::Workspace).ok()?;
        Some((self.holding_record(route, &state)?, state))
    }

    /// The route of a record `app` keeps in `state`, the app's own: what its replica is mounted
    /// over, and what publishes the app's documents for its workspace beside it.
    fn holding_record(
        &self,
        route: &DocumentRoute,
        state: &super::WorkspaceState,
    ) -> Option<DocumentRoute> {
        state
            .documents
            .iter()
            .find(|held| {
                held.app_id == route.app_id
                    && held.cluster_id == route.cluster_id
                    && held.document != route.document
                    && self
                        .catalogue
                        .get(&held.app_id)
                        .is_some_and(|app| opens_as_its_own(app, &held.document))
            })
            .cloned()
    }

    /// Whether `caller` is the host's own person, whom the service acts for.
    pub(super) fn serves_as_owner(&self, caller: &Caller) -> bool {
        caller.acting().person_id() == Some(self.member.as_str())
    }

    /// Whether `caller` is the host's own person or an agent one of their
    /// devices handed its session to: who may open records, show them on the
    /// shared layout and run commands in the workspace's shell.
    pub(super) fn acts_for_owner(&self, caller: &Caller) -> bool {
        acts_for(caller, &self.member)
    }

    pub(super) fn try_registered_read(
        &mut self,
        workspace: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Option<ActionOutput>, Box<Outcome>> {
        let Some(hint) = resource_of(&request.target.provider, "") else {
            return Ok(None);
        };
        let Some(link) = self.shared.link.get().cloned() else {
            return Ok(None);
        };
        let Some(policy) = link.view_read_policy(workspace, caller, &hint.app_id, &hint.document)
        else {
            return Ok(None);
        };
        let route = DocumentRoute {
            cluster_id: policy.workspace,
            ..hint
        };
        let arguments = match &request.argument {
            InvokeArgument::None => serde_json::Map::new(),
            InvokeArgument::Json(arguments) => arguments.clone(),
            InvokeArgument::Text(_) => {
                return Err(Box::new(Outcome::Rejected(
                    ActionCallError::InvalidRequest {
                        reason: "a registered read requires typed arguments".into(),
                    },
                )));
            }
        };
        let revision = request.revision.clone();
        // The read a listing of this document already prepared answers it
        // too, while the document stands where it was: a worker on the
        // document's retained source is a process, and one per call made
        // every read of such a document cost its start.
        let heads = self
            .mounted
            .get(&self.holder_of(&route))
            .and_then(|mounted| mounted.merged.clone());
        let kept = {
            let mut hand =
                call_wait::Handing::of(&mut self.call_waits, &mut self.part_waits, &self.shared);
            self.canonical.invoke_kept(
                &link,
                workspace,
                caller,
                &canonical::KeptAsk {
                    route: &route,
                    heads: heads.as_deref(),
                    address: &request.target.address,
                    arguments: &arguments,
                    revision: revision.as_deref(),
                },
                &mut hand,
            )
        };
        match kept {
            Ok(canonical::KeptRead::Answered(output)) => return Ok(Some(output)),
            Ok(canonical::KeptRead::NotOffered) => {}
            Ok(canonical::KeptRead::Stale) => {
                return Err(Box::new(Outcome::Rejected(ActionCallError::Refused {
                    reason: "the committed source changed".into(),
                })));
            }
            // Its worker answers it off this thread: the request waits for that answer.
            Ok(canonical::KeptRead::Held) => {
                return Err(Box::new(Outcome::Rejected(ActionCallError::Unavailable {
                    reason: "the app's worker is still answering".into(),
                })));
            }
            Err(error) => return Err(Box::new(api_outcome(&error))),
        }
        self.registered_read(workspace, caller, &route, request, revision, arguments)
    }

    /// The registered read `request` asks of the document at `route` in `workspace`, mounted in
    /// a worker this request has not prepared it in: what it answered, or `None` for a document
    /// that offers no such read. The mount is a worker process replaying the document's retained
    /// history, so the call goes on off this thread once it outlasts one, and this request
    /// takes its answer when it is applied again.
    fn registered_read(
        &mut self,
        workspace: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
        request: &InvokeRequest,
        revision: Option<String>,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ActionOutput>, Box<Outcome>> {
        let Some(link) = self.shared.link.get().cloned() else {
            return Ok(None);
        };
        let address = request.target.address.clone();
        let asked = {
            let (catalogue, link, asked_caller, asked) =
                (self.catalogue.clone(), link, caller.clone(), route.clone());
            let mut hand =
                call_wait::Handing::of(&mut self.call_waits, &mut self.part_waits, &self.shared);
            hand.hand(
                workspace,
                route,
                move || -> Result<Option<ActionOutput>, Box<Outcome>> {
                    let Ok(mut read) = reads::CanonicalRead::prepare(
                        &catalogue,
                        link.as_ref(),
                        workspace,
                        &asked_caller,
                        &asked,
                    ) else {
                        // A missing canonical read source must not replace or grant the
                        // ordinary document API. Unregistered effects stay refused there.
                        return Ok(None);
                    };
                    if !read
                        .operations()
                        .iter()
                        .any(|entry| entry.address == address)
                    {
                        return Ok(None);
                    }
                    if revision
                        .as_deref()
                        .is_some_and(|revision| revision != read.revision())
                    {
                        return Err(Box::new(Outcome::Rejected(ActionCallError::Refused {
                            reason: "the committed source changed".into(),
                        })));
                    }
                    read.invoke(
                        link.as_ref(),
                        workspace,
                        &asked_caller,
                        &asked,
                        &address,
                        &arguments,
                    )
                    .map(Some)
                    .map_err(|error| Box::new(api_outcome(&error)))
                },
            )
        };
        let Some(asked) = asked else {
            return Err(Box::new(Outcome::Rejected(ActionCallError::Unavailable {
                reason: "the app's worker is still answering".into(),
            })));
        };
        asked
    }

    /// A new record of `app` in `workspace_id`, created by the host's own person
    /// as its owner, or by `who` on their member access there now: a genesis they
    /// may not write is never saved, as no change of theirs is.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    pub(super) fn open_new(
        &mut self,
        workspace_id: WorkspaceId,
        app: &str,
        who: Option<&bounded::Bounded>,
    ) -> Result<ActionOutput, Outcome> {
        let workspace = workspace_id.to_string();
        let founder = self.founder(workspace_id, &workspace, app, who)?;
        let route = if self.catalogue.placed(app).is_some() {
            let (route, genesis) = self.placed_route(workspace_id, &workspace, app)?;
            self.save_genesis(workspace_id, &route, genesis, founder.permissions.as_ref())?;
            route
        } else {
            // An app that keeps one record per workspace opens that record: the
            // one the workspace holds when it has it, founded only when its host
            // answered it holds none.
            if self
                .catalogue
                .get(app)
                .is_some_and(ui::application::ApplicationDefinition::keeps_workspace_resource)
            {
                let route = DocumentRoute {
                    app_id: app.to_owned(),
                    cluster_id: workspace.clone(),
                    document: String::new(),
                };
                if self.held_or_owed(workspace_id, &route)? {
                    return Ok(ActionOutput {
                        value: Some(serde_json::json!({
                            "provider": resource_provider(&route),
                            "record": route.document,
                        })),
                        resources: Vec::new(),
                    });
                }
            }
            self.found(workspace_id, &workspace, app, Seed::New, founder)?
        };
        Ok(ActionOutput {
            value: Some(serde_json::json!({
                "provider": resource_provider(&route),
                "record": route.document,
            })),
            resources: Vec::new(),
        })
    }

    /// Who founds a record of `app` in `workspace` for `who` (the host's own person when
    /// `None`), refused when `who` may not make records of `app` there.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    pub(super) fn founder(
        &self,
        workspace_id: WorkspaceId,
        workspace: &str,
        app: &str,
        who: Option<&bounded::Bounded>,
    ) -> Result<Founder, Outcome> {
        let Some(who) = who else {
            return Ok(Founder {
                creator: self.member.clone(),
                actor: crate::ReplicaActor::Owner,
                permissions: None,
            });
        };
        let policy = self
            .policy_for(workspace_id, who, workspace, None)
            .map_err(Outcome::Rejected)?;
        let permissions = bounded::bounded_permissions(&policy, who).map_err(Outcome::Rejected)?;
        if permissions.answer(&TreePath::root().app(app), ActionKind::Undoable) != Answer::Allow {
            return Err(Outcome::Rejected(ActionCallError::NotPermitted));
        }
        let actor = bounded::member_actor(&policy, who, &self.own_device, None)
            .map_err(Outcome::Rejected)?;
        Ok(Founder {
            creator: who.person.clone(),
            actor,
            permissions: Some(permissions),
        })
    }

    /// Found a record of `app` in `workspace` as `seed` names it, as `founder`
    /// ([`Self::founder`]): its genesis, as the app makes it, saved as theirs. A
    /// record named by its id ([`Seed::NewAt`]) is founded with the genesis every device's
    /// first window of it makes, so what they write to it later merges with this.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    pub(super) fn found(
        &mut self,
        workspace_id: WorkspaceId,
        workspace: &str,
        app: &str,
        seed: Seed,
        founder: Founder,
    ) -> Result<DocumentRoute, Outcome> {
        let refused = |reason: String| Outcome::Rejected(ActionCallError::Refused { reason });
        let Founder {
            creator,
            actor,
            permissions,
        } = founder;
        let hosted = ConfinedReplica::open(
            &self.catalogue,
            app,
            workspace,
            &MemberId::Person(creator),
            &actor,
            seed,
            true,
        )
        .map_err(|error| refused(error.to_string()))?;
        let hosted = self.lend(hosted, workspace);
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: workspace.to_owned(),
            document: hosted.wire_document().to_owned(),
        };
        let genesis = hosted
            .take_mutation()
            .map_err(|error| refused(error.to_string()))?;
        self.save_genesis(workspace_id, &route, genesis, permissions.as_ref())?;
        Ok(route)
    }

    /// Save `genesis`, the first change of the new record at `route`, bounded by
    /// `permissions` when someone other than the host's own person founds it.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt answered"
    )]
    fn save_genesis(
        &mut self,
        workspace_id: WorkspaceId,
        route: &DocumentRoute,
        genesis: Option<DocumentMutation>,
        permissions: Option<&ui::permissions::PermissionContext>,
    ) -> Result<(), Outcome> {
        let refused = |reason: String| Outcome::Rejected(ActionCallError::Refused { reason });
        let app = route.app_id.as_str();
        if let (Some(permissions), Some(mutation)) = (permissions, &genesis) {
            let unwritable =
                bounded::refused_writes(permissions, (app, None), None, mutation, (None, false))
                    .map_err(|reason| commit_failed(reason, false))?;
            if !unwritable.is_empty() {
                return Err(Outcome::Rejected(ActionCallError::NotPermitted));
            }
        }
        // When the host device takes part in the workspace's documents, it opens a
        // new record's document for the member and hands it the keys, so the
        // first change goes out sealed instead of waiting for keys.
        if genesis.is_some()
            && let Some(keys) = self
                .shared
                .link
                .get()
                .and_then(|link| link.serving_keys(route, true))
        {
            let keys = keys.map_err(|reason| {
                Outcome::Rejected(ActionCallError::Unavailable {
                    reason: format!("the host holds no keys to a new {app} record: {reason}"),
                })
            })?;
            self.workspaces
                .get(&workspace_id)
                .ok_or_else(|| refused("the workspace was not joined".into()))?
                .peer
                .send(Cmd::InstallGroupKeys(route.clone(), keys))
                .map_err(|closed| refused(closed.to_string()))?;
        }
        if let Some(mutation) = genesis {
            self.commit(workspace_id, route, &mutation)
                .map_err(refused)?;
        }
        Ok(())
    }
}

/// Who founds a new record ([`Worker::founder`]): the member it is made by, the actor it is
/// written as, and the access that bounds its genesis when that is not the host's own person.
pub(super) struct Founder {
    creator: ui::permissions::policy::PersonId,
    actor: crate::ReplicaActor,
    permissions: Option<ui::permissions::PermissionContext>,
}
