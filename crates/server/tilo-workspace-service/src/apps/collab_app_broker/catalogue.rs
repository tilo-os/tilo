#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

/// The catalogue one audience may see, each surface's actions shared by its
/// followers.
pub(super) type CatalogueRows = Vec<(runtime_collab::CatalogueSurfaceId, Arc<[PublishedAction]>)>;

pub(super) type ActionCatalogueSubscriber = (String, BrokerActor, Weak<CatalogueFollower>);

pub(super) fn catalogue_rows(surfaces: Vec<runtime_collab::BrokerActionSurface>) -> CatalogueRows {
    surfaces
        .into_iter()
        .map(|surface| (surface.id(), Arc::from(surface.actions)))
        .collect()
}

/// One connection following the catalogue: what it holds, and the change
/// it is sent next. Changes made while it has not read compose into one.
pub(super) struct CatalogueFollower {
    pub(super) following: Mutex<CatalogueFollowing>,
    pub(super) changed: tokio::sync::Notify,
}

#[derive(Default)]
pub(super) struct CatalogueFollowing {
    pub(super) publication: runtime_collab::CataloguePublication,
    pub(super) pending: Option<runtime_collab::CatalogueChange>,
    /// Its audience's authorization failed: it is sent nothing more.
    pub(super) ended: bool,
}

impl CatalogueFollower {
    /// A follower that is sent `rows` whole first.
    pub(super) fn new(rows: &CatalogueRows) -> Arc<Self> {
        let follower = Arc::new(Self {
            following: Mutex::new(CatalogueFollowing::default()),
            changed: tokio::sync::Notify::new(),
        });
        follower.follow(rows);
        follower
    }

    /// Queue the change from what it holds to `rows`.
    pub(super) fn follow(&self, rows: &CatalogueRows) {
        let mut following = self.following.lock_recover();
        let change = following.publication.next(rows);
        if change.is_empty() {
            return;
        }
        following.pending = Some(match following.pending.take() {
            Some(pending) => pending.then(change),
            None => change,
        });
        drop(following);
        self.changed.notify_one();
    }

    /// Send nothing more, not even a change not read yet: a failed
    /// authorization must never let a slow reader consume stale discovery.
    pub(super) fn end(&self) {
        let mut following = self.following.lock_recover();
        following.ended = true;
        following.pending = None;
        drop(following);
        self.changed.notify_one();
    }

    /// The next change, once there is one; `Err` once it has ended.
    pub(super) async fn next(&self) -> Result<runtime_collab::CatalogueChange, ()> {
        loop {
            {
                let mut following = self.following.lock_recover();
                if following.ended {
                    return Err(());
                }
                if let Some(change) = following.pending.take() {
                    return Ok(change);
                }
            }
            self.changed.notified().await;
        }
    }
}

pub(super) struct ActionCatalogueStreamServer {
    pub(super) follower: Arc<CatalogueFollower>,
}

impl ActionCatalogueStreamServer {
    /// The whole catalogue, then each change to it, as JSON.
    #[allow(
        clippy::future_not_send,
        reason = "the stream is served on the connection's single-threaded local set"
    )]
    pub(super) async fn next(&self) -> Result<String, BrokerError> {
        let change = self
            .follower
            .next()
            .await
            .map_err(|()| BrokerError::disconnected("action catalogue stream closed".into()))?;
        let json = serde_json::to_string(&change)
            .map_err(|error| BrokerError::failed(error.to_string()))?;
        if change.reset {
            // The session's normal tracing filter passes only errors. Its
            // flight recorder captures stderr; report numeric metadata
            // without exporting any authorized catalogue contents.
            eprintln!(
                "collab: initial authorized action catalogue snapshot event_kind=ActionCatalogue serialized_bytes={}",
                json.len()
            );
        }
        Ok(json)
    }
}

impl AppBrokerState {
    /// Snapshot every action surface currently published by a connected app.
    pub fn published_surfaces(&self) -> Vec<PublishedSurface> {
        let live = self
            .publishers
            .surfaces
            .lock()
            .map(|surfaces| surfaces.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let live_keys = live
            .iter()
            .map(|surface| {
                (
                    surface.app_id.clone(),
                    surface.route.cluster_id.clone(),
                    surface.route.document_id.clone(),
                )
            })
            .collect::<std::collections::HashSet<_>>();
        let mut result = live;
        result.extend(self.launchable.surfaces().into_iter().filter(|surface| {
            !live_keys.contains(&(
                surface.app_id.clone(),
                surface.route.cluster_id.clone(),
                surface.route.document_id.clone(),
            ))
        }));
        result
    }
    /// Resolve against the current selected source, independently of the
    /// asynchronous search snapshot. A live source shadows its retained
    /// launchable catalogue even when it no longer offers the selected action.
    pub fn published_action(
        &self,
        route: &CrdtRoute,
        resource: &str,
        action: &str,
    ) -> Option<(PublishedSurface, PublishedAction)> {
        let surfaces = self.publishers.surfaces.lock().ok()?;
        let mut live_route = false;
        for surface in surfaces
            .values()
            .filter(|surface| surface.app_id == route.app_id && surface.route == *route)
        {
            live_route = true;
            if let Some(selected) = surface.action_projection(resource, action) {
                return Some(selected);
            }
        }
        let selected = if live_route {
            None
        } else {
            self.launchable.action_for(route, resource, action)
        };
        // Keep live-source shadowing coherent with the archive lookup; a
        // publisher must not install a live replacement between those reads.
        drop(surfaces);
        selected
    }
    /// Check one live action route without cloning the published catalogue.
    pub fn has_live_published_action(
        &self,
        app_id: &str,
        cluster_id: &str,
        document_id: &str,
        resource: &str,
        action: &str,
    ) -> bool {
        self.publishers.surfaces.lock().is_ok_and(|surfaces| {
            surfaces.values().any(|surface| {
                surface.live
                    && surface.app_id == app_id
                    && surface.route.cluster_id == cluster_id
                    && surface.route.document_id == document_id
                    && surface.actions.iter().any(|descriptor| {
                        descriptor.resource == resource && descriptor.action == action
                    })
            })
        })
    }
    /// Generation of the eager action-surface source. Reading this never
    /// clones the potentially large application action inventory.
    #[must_use]
    pub fn published_surfaces_generation(&self) -> u64 {
        *self.publishers.changes.borrow()
    }
    #[cfg(test)]
    pub(super) fn action_catalogue(
        &self,
        cluster_id: &str,
        actor: &str,
    ) -> Result<Vec<runtime_collab::BrokerActionSurface>, String> {
        self.action_catalogue_for(cluster_id, &BrokerActor::person(actor))
    }
    #[allow(
        clippy::too_many_lines,
        reason = "catalogue projection and policy filtering remain adjacent so a published action cannot bypass the actor check"
    )]
    pub(super) fn action_catalogue_for(
        &self,
        cluster_id: &str,
        actor: &BrokerActor,
    ) -> Result<Vec<runtime_collab::BrokerActionSurface>, String> {
        let now = self.policy_now_ns()? / 1_000_000_000;
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the action broker has no workspace authority".to_string())?;
        // Ordinary directed invocation needs a live publisher. Only native
        // task ownership can activate an inert retained route; its catalogue
        // keeps stable providers discoverable without inventing a publisher.
        let live = self
            .published_surfaces()
            .into_iter()
            .filter(|surface| surface.live)
            .collect::<Vec<_>>();
        let routes = live
            .iter()
            .map(|surface| surface.route.clone())
            .collect::<Vec<_>>();
        let retained =
            self.launchable.surfaces().into_iter().filter(|surface| {
                self.confines_agents(cluster_id) && !routes.contains(&surface.route)
            });
        let published = live
            .into_iter()
            .chain(retained)
            .filter(|surface| surface.route.cluster_id == cluster_id)
            .collect::<Vec<_>>();
        let applications = self
            .services
            .launcher
            .as_ref()
            .map(|service| service.applications())
            .unwrap_or_default();
        // A step of an agent session's own (the programs it writes) is listed to an agent while
        // it works in a session, and taken only there. A session step (for turns) is listed to
        // every caller the grants allow, as an app action is.
        let in_session = actor.session().is_some();
        let task_steps = self
            .services
            .host_steps
            .iter()
            .filter(|steps| steps.listed())
            .filter(|steps| steps.for_turns() || (in_session && steps.for_agent_sessions()))
            .map(|steps| {
                let actions = steps.descriptors().into_iter().collect::<Vec<_>>();
                (steps.provider(), steps.app_id().to_owned(), actions)
            })
            .collect::<Vec<_>>();
        // One policy sample decides every row; serialization runs after the
        // committed authority is released.
        let (published, applications, task_steps) =
            membership.decide_in_workspace(cluster_id, |workspace| {
                let workspace = workspace
                    .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
                let published = published
                    .into_iter()
                    .map(|mut surface| {
                        if !surface.live
                            && !actor.may_in(
                                workspace,
                                &app_node(workspace, &surface.app_id),
                                ActionKind::Undoable,
                                now,
                            )
                        {
                            surface.actions.clear();
                        }
                        // Listed when the actor may take it, or would be asked
                        // first.
                        surface.actions.retain(|action| {
                            actor.action_answer_in(
                                workspace,
                                &surface.app_id,
                                (&action.resource_kind, &action.resource, &action.action),
                                &action.contract.need(),
                                now,
                            ) != Answer::Deny
                        });
                        surface
                    })
                    .collect::<Vec<_>>();
                let applications = applications
                    .into_iter()
                    .filter(|app| {
                        actor.may_in(
                            workspace,
                            &app_node(workspace, &app.package_id),
                            ActionKind::Undoable,
                            now,
                        )
                    })
                    .collect::<Vec<_>>();
                let task_steps = task_steps
                    .into_iter()
                    .map(|(provider, app_id, mut actions)| {
                        actions.retain(|descriptor| {
                            actor.action_answer_in(
                                workspace,
                                &app_id,
                                (
                                    &descriptor.resource_kind,
                                    &descriptor.address.resource,
                                    &descriptor.address.action,
                                ),
                                &descriptor.contract.need(),
                                now,
                            ) != Answer::Deny
                        });
                        (provider, app_id, actions)
                    })
                    .collect::<Vec<_>>();
                Ok::<_, String>((published, applications, task_steps))
            })??;
        // A row's display state is no follower's: a change of it alone sends nothing.
        let stateless = |mut action: PublishedAction| {
            action.state = tilo_actions::ActionState::default();
            action
        };
        let mut surfaces: Vec<runtime_collab::BrokerActionSurface> = Vec::new();
        let mut offer = |surface: runtime_collab::BrokerActionSurface| {
            if !surface.actions.is_empty() {
                surfaces.push(surface);
            }
        };
        for surface in published {
            let provider = (!surface.live).then(|| launchable::launchable_provider(&surface));
            offer(runtime_collab::BrokerActionSurface {
                peer_id: surface.peer_id,
                provider,
                app_id: surface.app_id,
                cluster_id: surface.route.cluster_id,
                document_id: surface.route.document_id,
                live: surface.live,
                actions: surface.actions.into_iter().map(stateless).collect(),
            });
        }
        for (provider, app_id, actions) in task_steps {
            offer(runtime_collab::BrokerActionSurface {
                peer_id: 0,
                provider: Some(provider.to_owned()),
                app_id,
                cluster_id: cluster_id.to_owned(),
                document_id: String::new(),
                live: true,
                actions: actions
                    .iter()
                    .map(|descriptor| stateless(PublishedAction::from(descriptor)))
                    .collect(),
            });
        }
        // A running app is listed to open too: opening it brings its window forward, as the
        // person's own launch of it does.
        for app in applications {
            offer(runtime_collab::BrokerActionSurface {
                peer_id: 0,
                provider: Some(tilo_actions::remote::workspace_provider(cluster_id)),
                app_id: app.package_id.clone(),
                cluster_id: cluster_id.to_owned(),
                document_id: String::new(),
                live: true,
                actions: vec![stateless(PublishedAction::from(
                    &super::action_provider::app_open_descriptor(&app),
                ))],
            });
        }
        Ok(surfaces)
    }
    /// Invalidate retained discovery only after a real service-surface mutation.
    pub(super) fn publish_changed_action_catalogue(&self) {
        self.surfaces_changed();
        self.catalogue_changed();
    }
    /// The host's own steps changed what they offer (the session's
    /// layout gained a place, a window opened): every connection
    /// following the catalogue is handed it again.
    pub fn host_steps_changed(&self) {
        self.catalogue_changed();
    }
    pub(super) fn publish_action_catalogue(&self) {
        let subscribers = self
            .action_catalogue_subscribers
            .lock()
            .map(|subscribers| {
                subscribers
                    .iter()
                    .map(|(peer, (cluster, actor, follower))| {
                        (*peer, cluster.clone(), actor.clone(), follower.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut audiences: HashMap<(String, BrokerActor), Option<CatalogueRows>> = HashMap::new();
        let mut disconnected = Vec::new();
        for (peer, cluster, actor, follower) in subscribers {
            let Some(follower) = follower.upgrade() else {
                disconnected.push(peer);
                continue;
            };
            let rows = audiences
                .entry((cluster.clone(), actor.clone()))
                .or_insert_with(|| {
                    self.action_catalogue_for(&cluster, &actor)
                        .ok()
                        .map(catalogue_rows)
                });
            if let Some(rows) = rows {
                follower.follow(rows);
            } else {
                follower.end();
                disconnected.push(peer);
            }
        }
        if !disconnected.is_empty()
            && let Ok(mut subscribers) = self.action_catalogue_subscribers.lock()
        {
            for peer in disconnected {
                subscribers.remove(&peer);
            }
        }
    }
}
