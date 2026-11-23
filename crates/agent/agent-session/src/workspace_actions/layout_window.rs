//! A view an action attaches, put in front of the person on their layout
//! ([`tilo_desktop_model::place`]), or asked for in the layout's document when the layout is not
//! held here ([`tilo_desktop_model::asked`]). Nothing waits for the window.

use super::{
    APPLICATION_OPEN_ACTION, ActionCallError, ActionOutput, Caller, DocumentMutation,
    DocumentRoute, InvokeRequest, Needs, Outcome, Worker, WorkspaceDocuments, WorkspaceId,
    application_provider_resource, caller_context, commit_failed, said_invocation_title,
};

impl Worker {
    /// Show the record an application's `open` names (its own record when it names none) as a
    /// window on the workspace's shared desktop: what the device an agent acts through asks for
    /// each record the agent makes or changes (`tilo mcp`, the API server), so every member sees
    /// it open as they see a colleague's window, without anyone's view moving to it
    /// ([`tilo_desktop_model::place`]). The window is the person's the caller acts for — the host's own, or a
    /// member of the workspace — and names the agent run that opened it. A record a window
    /// already shows is left where the people put it.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the host records"
    )]
    pub(super) fn show_on_layout(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        (named, app): (&str, &str),
        request: &InvokeRequest,
    ) -> Result<ActionOutput, Outcome> {
        if request.target.address.action != APPLICATION_OPEN_ACTION
            || WorkspaceId::parse(named) != Some(workspace_id)
        {
            return Err(Outcome::Rejected(ActionCallError::UnknownAction));
        }
        let document = application_provider_resource(&request.target.provider).unwrap_or_default();
        let opened_by = opened_by(caller, request);
        self.place_window(workspace_id, caller, (app, document), opened_by)?;
        Ok(ActionOutput::default())
    }

    /// Put a window of `app` showing `document` (empty: the app's primary document) on the
    /// workspace's layout, as the person the caller acts for, named as the agent session that
    /// opened it. A document a window already shows is left where the people put it. Nothing waits
    /// for the layout: without it here, the view is asked for in the layout's document
    /// ([`tilo_desktop_model::asked`]).
    ///
    /// # Errors
    ///
    /// Who may not, a document the workspace does not have, or why the view could not be placed
    /// (typed: the caller is never told a window shows that does not).
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the service keeps"
    )]
    pub(super) fn place_window(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        (app, document): (&str, &str),
        opened_by: Option<tilo_desktop_model::ownership::OpenedBy>,
    ) -> Result<(), Outcome> {
        let unknown = || Outcome::Rejected(ActionCallError::UnknownAction);
        let person = self
            .layout_person(workspace_id, caller)
            .ok_or_else(unknown)?;
        let title = self
            .catalogue
            .get(app)
            .map(|definition| definition.title().to_owned())
            .ok_or_else(unknown)?;
        let workspace = workspace_id.to_string();
        let layout = crate::workspace_documents::desktop_route(&workspace);
        // Read, not acted on: a layout not here yet is asked for below.
        let state = self
            .refresh(
                workspace_id,
                Needs::Read {
                    app_id: &layout.app_id,
                    record: &layout.document,
                },
            )
            .map_err(Outcome::Rejected)?;
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: workspace,
            document: document.to_owned(),
        };
        if !document.is_empty()
            && !state.holds(&route)
            && !self.held_or_owed(workspace_id, &route)?
        {
            return Err(Outcome::Rejected(ActionCallError::Refused {
                reason: format!("this workspace has no {app} record {}", route.document),
            }));
        }
        let Some(held) = self.held_layout(workspace_id)? else {
            // A layout not held here (none yet, or not replayed here yet) is never waited for: the
            // view is asked for in the layout's document, and the person's page places it once it
            // holds the layout, now or when it next comes up.
            let view = tilo_desktop_model::asked::AskedView {
                app_id: app.to_owned(),
                title,
                document: document.to_owned(),
                person: person.to_string(),
                opened_by,
            };
            return self.ask_on_layout(workspace_id, &layout, &view);
        };
        let not_placed = |why: tilo_desktop_model::place::NotPlaced| not_placed(&title, &why);
        let view = tilo_desktop_model::place::View {
            app_id: app,
            title: &title,
            document: &route.document,
            beside: None,
        };
        let opener = tilo_desktop_model::place::Opener {
            person: &person,
            opened_by,
        };
        let Some(change) =
            tilo_desktop_model::place::place_view(&held, &opener, &view).map_err(not_placed)?
        else {
            return Ok(());
        };
        self.commit(
            workspace_id,
            &layout,
            &DocumentMutation::Incremental(change),
        )
        .map_err(|reason| commit_failed(reason, true))
    }

    /// The person whose window `caller` opens or closes on the workspace's layout: the host's own
    /// member for its owner, else the member the caller acts for; `None` for a caller the
    /// workspace does not admit.
    pub(super) fn layout_person(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
    ) -> Option<ui::permissions::policy::PersonId> {
        if self.acts_for_owner(caller) {
            return Some(self.member.clone());
        }
        self.bounded(caller)
            .filter(|who| self.admits_bounded(workspace_id, who))
            .map(|who| who.person)
    }

    /// `workspace_id`'s shared layout as the device's service holds it now, read even
    /// before anything landed here this session (an agent's first call is often an open):
    /// `None` while it holds none.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the service keeps"
    )]
    pub(super) fn held_layout(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<automerge::AutoCommit>, Outcome> {
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| commit_failed("the workspace was not joined".into(), true))?;
        let reads = workspace.reads.clone();
        Ok(workspace
            .documents
            .get_or_insert_with(|| WorkspaceDocuments::new(&workspace_id.to_string(), reads))
            .desktop())
    }

    /// Ask for `view` in the layout's document, as a document of its own that merges with any
    /// copy of the layout ([`tilo_desktop_model::asked`]): the person's page places it.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the service keeps"
    )]
    fn ask_on_layout(
        &mut self,
        workspace_id: WorkspaceId,
        layout: &DocumentRoute,
        view: &tilo_desktop_model::asked::AskedView,
    ) -> Result<(), Outcome> {
        let mut ask = automerge::AutoCommit::new();
        let id = format!("{}:{}:{}", view.person, view.app_id, view.document);
        tilo_desktop_model::asked::ask(&mut ask, &id, view)
            .map_err(|error| commit_failed(error.to_string(), false))?;
        ask.commit();
        self.commit(
            workspace_id,
            layout,
            &DocumentMutation::Snapshot(ask.save()),
        )
        .map_err(|reason| commit_failed(reason, true))
    }
}

/// The refusal of a window that is not on the person's layout: `title`'s, and why.
fn not_placed(title: &str, why: &tilo_desktop_model::place::NotPlaced) -> Outcome {
    Outcome::Rejected(ActionCallError::Unavailable {
        reason: format!("{title}'s window is not on the person's layout: {why}"),
    })
}

/// The agent session that opens a window for `request`, when an agent's session does: what the
/// window's mark says beside it.
fn opened_by(
    caller: &Caller,
    request: &InvokeRequest,
) -> Option<tilo_desktop_model::ownership::OpenedBy> {
    caller_context(caller, request)
        .group
        .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)
        .map(|group| {
            tilo_desktop_model::ownership::OpenedBy {
                run: group.id,
                agent: caller.acting().agent_id().unwrap_or_default().to_owned(),
                // The name the agent goes by here: its session's, where its client named one
                // (`Caller::in_session_of`), as everywhere else it appears.
                label: caller
                    .clone()
                    .in_session_of(request)
                    .agent_called()
                    .unwrap_or_default()
                    .to_owned(),
                // Only what the agent called the step that opened it: the group's label
                // also names a session asked nothing by its agent's name, which the mark
                // already says.
                title: said_invocation_title(request).unwrap_or_default(),
            }
        })
}
