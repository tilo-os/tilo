//! Records the workspace sends this agent only when it opens them.
//!
//! A public workspace — one listed in its host's directory, which anyone may
//! walk into — replays a newcomer only its governance and its shared desktop,
//! and sends every other record to a device once that device opens it,
//! saying so in the [`Event::HostReady`] that ends the replay. So a record
//! the agent does not hold there is not a record the workspace does not
//! have: it is asked for ([`Cmd::OpenRoute`](tilo_session_client::Cmd::OpenRoute), which asks the host
//! for it and delivers it as an ordinary change when it comes), and it is
//! the agent's to make only once the host answered it holds none of it
//! ([`Event::DocumentOpened`] with no frames). Without that, an agent
//! opening a record before it arrived founded one of its own, and the first
//! change it published forked the workspace's document.

use super::{AgentMember, DocumentRoute, Step, parts};
use tilo_session_client::{Cmd, Outbox};

impl AgentMember {
    /// Take the workspace's answer about a record this agent asked for: the
    /// host sent `frames` of it, and none means the workspace holds none of
    /// it, so it is the agent's to make ([`Self::document_on_its_way`]).
    pub(super) fn record_answered(
        &mut self,
        route: DocumentRoute,
        answer: tilo_session_client::DocumentAnswer,
    ) {
        // Answered once: it is not asked again, whatever the answer.
        if !self.asked_documents.contains(&route) {
            return;
        }
        let told = match answer {
            // Another person's own record: never this agent's, never one it founds, and what
            // asked for it is told so rather than left waiting.
            tilo_session_client::DocumentAnswer::NotYours => {
                ui::resource::WorkspaceAnswer::NotYours
            }
            tilo_session_client::DocumentAnswer::Sent { frames: 0 } => {
                ui::resource::WorkspaceAnswer::HoldsNone
            }
            tilo_session_client::DocumentAnswer::Sent { .. } => return,
        };
        tracing::debug!(
            app = %route.app_id,
            record = %route.record,
            ?told,
            "agent: the workspace answered a record it does not send"
        );
        // What a held record asked for of what it keeps beside it (a chain's head or part) is
        // told to it.
        if let Some(record) = ui::resource::DocumentId::parse(&route.record) {
            for (held, app) in &self.apps {
                if held.app_id == route.app_id
                    && let Err(error) = app.workspace_answered(&record, told)
                {
                    tracing::warn!(%error, "agent: a record was not told the workspace's answer");
                }
            }
        }
        match told {
            ui::resource::WorkspaceAnswer::NotYours => self.not_yours.insert(route),
            ui::resource::WorkspaceAnswer::HoldsNone => self.absent_documents.insert(route),
        };
    }

    /// Have the record held at `route` read what it lacks through the
    /// workspace, in a workspace that sends records when they are opened:
    /// a chain it holds no part of then asks for its head, and its writes
    /// wait for the answer, rather than starting a part beside the chain's
    /// newest ([`Self::ask_wanted`] asks for what it wants).
    pub(super) fn ask_workspace_through(&mut self, route: &DocumentRoute) {
        if !self.when_opened {
            return;
        }
        let Some(app) = self.apps.get(route) else {
            return;
        };
        if let Err(error) = app.ask_workspace_for_missing() {
            tracing::warn!(%error, "agent: a record does not read through the workspace");
        }
        self.ask_wanted();
    }

    /// Ask the host for every record a held record wants of the workspace
    /// (a chain's head, an earlier part), once each.
    pub(super) fn ask_wanted(&mut self) {
        if !self.when_opened {
            return;
        }
        let wanted: Vec<DocumentRoute> = self
            .apps
            .iter()
            .flat_map(|(held, app)| {
                app.wanted_resources()
                    .into_iter()
                    .map(|record| DocumentRoute {
                        app_id: held.app_id.clone(),
                        cluster_id: held.cluster_id.clone(),
                        record: record.route().to_owned(),
                    })
            })
            .collect();
        for route in wanted {
            if self.absent_documents.contains(&route) || !self.asked_documents.insert(route.clone())
            {
                continue;
            }
            if let Err(error) = self.peer.send(Cmd::OpenRoute(route)) {
                tracing::warn!(%error, "agent: a record the workspace may hold was not asked for");
            }
        }
    }

    /// Whether the record at `route` is one the workspace may still send this
    /// agent: it is not here, this workspace sends its records when they are
    /// opened, and its host has not answered that it holds none of it. The
    /// host is asked for it the first time. A caller takes the record as new
    /// only when this is `false`.
    pub(super) fn document_on_its_way(&mut self, route: &DocumentRoute) -> bool {
        if self.apps.contains_key(route) || self.unopened.changed(route).is_some() {
            return false;
        }
        if !self.when_opened
            || self.absent_documents.contains(route)
            || self.not_yours.contains(route)
        {
            return false;
        }
        if self.asked_documents.insert(route.clone())
            && let Err(error) = self.peer.send(Cmd::OpenRoute(route.clone()))
        {
            tracing::warn!(%error, "agent: a record the workspace may hold was not asked for");
        }
        true
    }

    /// Whether `step` acts on a record the workspace may still send this
    /// agent ([`Self::document_on_its_way`]): it is asked for, and the step
    /// waits for it, so what it writes lands in the workspace's document
    /// rather than in one founded here. A step naming no record waits on an
    /// app that keeps one record per workspace
    /// ([`ui::application::ApplicationDefinition::keeps_workspace_resource`]),
    /// whose record the workspace may already hold.
    pub(super) fn step_record_arriving(&mut self, step: &Step) -> bool {
        let record = match (&step.record, self.keeps_workspace_resource(&step.app)) {
            (Some(record), _) if !parts::is_part(record) => record.clone(),
            (None, true) => String::new(),
            _ => return false,
        };
        let route = DocumentRoute {
            app_id: step.app.clone(),
            cluster_id: self.workspace.clone(),
            record,
        };
        self.document_on_its_way(&route)
    }

    /// Whether a record of an app `task` names is one the workspace may still
    /// send this agent ([`Self::document_on_its_way`]), asked for now: the task
    /// waits for it, so it is planned with what the workspace holds. Only an
    /// app that keeps one record per workspace is asked about: what other
    /// apps' records are is a listing, and a task naming none of them makes
    /// its own.
    pub(super) fn task_documents_arriving(&mut self, task: &str) -> bool {
        let apps: Vec<String> = self
            .catalogue
            .iter()
            .filter(|definition| definition.keeps_workspace_resource())
            .map(|definition| definition.app_id.to_string())
            .filter(|app| self.identity.delegation.permits_app(app))
            .filter(|app| crate::model::task_names_app(task, app))
            .collect();
        let mut arriving = false;
        for app in apps {
            let route = DocumentRoute {
                app_id: app,
                cluster_id: self.workspace.clone(),
                record: String::new(),
            };
            arriving = self.document_on_its_way(&route) || arriving;
        }
        arriving
    }

    /// Whether `app` keeps one record per workspace, which the workspace may
    /// already hold.
    pub(super) fn keeps_workspace_resource(&self, app: &str) -> bool {
        self.catalogue
            .get(app)
            .is_some_and(ui::application::ApplicationDefinition::keeps_workspace_resource)
    }
}
