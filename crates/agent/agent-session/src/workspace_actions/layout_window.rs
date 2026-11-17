//! A view an action attaches, put in front of the person on their layout
//! ([`tilo_desktop_model::place`]), or asked for in the layout's document when the layout is not
//! held here ([`tilo_desktop_model::asked`]). Nothing waits for the window.

use super::{
    APPLICATION_OPEN_ACTION, ActionCallError, ActionOutput, Arc, Caller, DocumentMutation,
    DocumentRoute, InvokeRequest, Needs, Outcome, Worker, WorkspaceDocuments, WorkspaceId,
    application_provider_resource, caller_context, commit_failed,
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
        self.place_window(workspace_id, caller, (named, app, document), request)?;
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
    fn place_window(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        (named, app, document): (&str, &str, &str),
        request: &InvokeRequest,
    ) -> Result<(), Outcome> {
        let unknown = || Outcome::Rejected(ActionCallError::UnknownAction);
        let person = if self.acts_for_owner(caller) {
            self.member.clone()
        } else {
            self.bounded(caller)
                .filter(|who| self.admits_bounded(workspace_id, who))
                .ok_or_else(unknown)?
                .person
        };
        let opened_by = caller_context(caller, request)
            .and_then(|context| context.group)
            .filter(|group| group.kind == tilo_actions::context::AGENT_SESSION_GROUP)
            .map(|group| tilo_desktop_model::ownership::OpenedBy {
                run: group.id,
                // The agent, by its own name; its session, by the title it gave.
                label: caller
                    .agent_session()
                    .map_or(group.label, |session| session.label),
                title: group.title.unwrap_or_default(),
            });
        let title = self
            .catalogue
            .get(app)
            .map(|definition| definition.title().to_owned())
            .ok_or_else(unknown)?;
        let layout = crate::member::desktop_route(named);
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
            cluster_id: named.to_owned(),
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
        let workspace = self
            .workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| commit_failed("the workspace was not joined".into(), true))?;
        // The layout as the session's replica holds it, read even before anything landed here
        // this session (an agent's first call is often an open).
        let engine = Arc::clone(workspace.peer.engine());
        let Some(held) = workspace
            .records
            .get_or_insert_with(|| WorkspaceDocuments::new(named, engine))
            .desktop()
        else {
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
