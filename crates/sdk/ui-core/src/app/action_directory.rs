// The action directory: the revision a caller caches against, the cursor
// its next page continues from, the rows the page carries, and the awaited
// answers to a request for a resource still being read. Included by
// `app.rs` next to `app/actions.rs`, whose discovery and dispatch these
// answer.
impl App {
    fn action_directory_revision(&mut self) -> String {
        // Internal cursor/cache callers cannot turn an unavailable policy into
        // a reusable identity. Public query/resolve/delivery use the fallible
        // method and refuse before returning a cached result or invoking code.
        self.try_action_directory_revision()
            .unwrap_or_else(|_| format!("unavailable:{}", uuid::Uuid::new_v4()))
    }

    fn try_action_directory_revision(&mut self) -> Result<String, String> {
        let captured = (|| {
            let owners = crate::bound_dispatch::directory_witness(
                self.root_widget.as_ref(),
                &self.state.doc,
            )?;
            let permissions = self.permission_ctx.directory_witness()?;
            let reusable = owners.iter().all(|owner| owner.generation.is_some());
            // Described, not made: making a provider can build its index,
            // and this runs on every frame that publishes the surface.
            let directory_providers =
                crate::bound_dispatch::search_provider_descriptors(self.root_widget.as_ref());
            let provider_domains = directory_providers
                .iter()
                .map(|provider| provider.invocation_domain)
                .collect();
            let witness = action_directory_snapshot::DirectoryWitness {
                registry_generation: self.actions.action_registry_generation,
                root_domain: crate::resources::action_directory_invocation_domain(
                    self.root_widget.as_ref(),
                    &self.state.doc,
                ),
                provider_domains,
                key: self.resource_cache_key(),
                permissions,
                permission_generation: self.actions.permission_generation.get(),
                route: self.bound_broker_route().cloned(),
                owners,
                root_revision: crate::resources::action_directory_revision(
                    self.root_widget.as_ref(),
                    &self.state.doc,
                ),
                providers: directory_providers
                    .into_iter()
                    .map(|provider| {
                        format!(
                            "{}:{:?}:{:?}",
                            provider.name, provider.owner, provider.generation
                        )
                    })
                    .collect(),
            };
            Ok::<_, String>((reusable, witness))
        })();
        let (reusable, witness) = match captured {
            Ok(value) => value,
            Err(error) => {
                self.actions.action_directory_stamp = None;
                self.actions.action_directory_snapshot = None;
                self.actions.action_directory_cursors.clear();
                return Err(error);
            }
        };
        if reusable
            && let Some((old, revision)) = &self.actions.action_directory_stamp
            && old.as_ref() == &witness
        {
            return Ok(revision.clone());
        }
        // Generations are updated at mutation boundaries; reading Automerge
        // heads here would commit an open transaction and split an undo unit.
        self.actions.action_directory_snapshot = None;
        self.actions.action_directory_cursors.clear();
        let revision = format!("v5:{}", uuid::Uuid::new_v4());
        self.actions.action_directory_stamp = Some((std::rc::Rc::new(witness), revision.clone()));
        Ok(revision)
    }

    /// The descriptor a directory row carries. Directory pages have never
    /// carried a resource view: the broker proves a view's mount and range
    /// bindings against the actions on the same page, which a bounded page
    /// cannot promise, so views reach the shell through the published
    /// surface instead.
    fn exported_directory_action(
        descriptor: &tilo_actions::ActionDescriptor,
    ) -> runtime_collab::ExportedAction {
        runtime_collab::ExportedAction {
            state: tilo_actions::ActionState::default(),
            view: None,
            ..runtime_collab::ExportedAction::from(descriptor)
        }
    }

    fn directory_resource_row(
        &self,
        resource: &tilo_actions::ResourceSpec,
        parent_resource: &str,
        may_descend: bool,
    ) -> Option<runtime_collab::ActionDirectoryRow> {
        // Preserve the explicit-or-View disclosure rule, finding only the
        // needed descriptor in this bounded provider result, not surveying a
        // complete action catalogue for each displayed row.
        fn visibility(
            resource: &tilo_actions::ResourceSpec,
            explicit: Option<&tilo_actions::ActionAddress>,
        ) -> Option<tilo_actions::ActionDescriptor> {
            resource
                .actions
                .iter()
                .filter(|action| match explicit {
                    Some(address) => {
                        address.resource == resource.id.as_str() && address.action == action.id
                    }
                    None => action.kind == tilo_actions::OperationKind::View,
                })
                // A document read discloses the resource without borrowing
                // the publisher's private render, focus or navigation state.
                // An explicit disclosure action still takes precedence.
                .min_by_key(|action| !action.contract.reads_document())
                .map(|action| App::directory_descriptor(resource, action))
                .or_else(|| {
                    resource
                        .children
                        .iter()
                        .find_map(|child| visibility(child, explicit))
                })
        }
        let explicit = self.directory_explicit_visibility(resource);
        let visibility = visibility(resource, explicit.as_ref())?;
        Some(runtime_collab::ActionDirectoryRow {
            parent_resource: parent_resource.to_owned(),
            resource: resource.id.as_str().to_owned(),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            presentation: resource
                .presentation
                .clone()
                .filter(tilo_actions::ResourcePresentation::is_valid),
            action: None,
            descriptor: None,
            has_children: may_descend
                && (!resource.children.is_empty() || !resource.actions.is_empty()),
            visibility: Some(tilo_actions::remote::RowVisibility {
                action: visibility.address.action,
                contract: visibility.contract,
                governing_kind: Some(visibility.kind),
            }),
        })
    }

    fn directory_action_row(
        resource: &tilo_actions::ResourceSpec,
        action_id: &str,
    ) -> Option<runtime_collab::ActionDirectoryRow> {
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == action_id)?;
        let descriptor = Self::directory_descriptor(resource, action);
        let exported = Self::exported_directory_action(&descriptor);
        Some(runtime_collab::ActionDirectoryRow {
            parent_resource: resource.id.as_str().to_owned(),
            resource: resource.id.as_str().to_owned(),
            resource_kind: exported.resource_kind.clone(),
            resource_label: exported.resource_label.clone(),
            presentation: None,
            action: Some(action_id.to_owned()),
            visibility: Some(tilo_actions::remote::RowVisibility {
                action: action_id.to_owned(),
                contract: exported.contract.clone(),
                governing_kind: Some(exported.operation_kind),
            }),
            descriptor: Some(exported),
            has_children: false,
        })
    }

    fn action_directory_cursor_key(&mut self) -> String {
        let revision = self.action_directory_revision();
        format!("tilo-dir-{}-{revision}", uuid::Uuid::new_v4())
    }

    fn action_directory_next_cursor(&mut self, state: ActionDirectoryCursor) -> Vec<u8> {
        if self.actions.action_directory_cursors.len() >= ACTION_DIRECTORY_MAX_CURSORS {
            // Bound memory even when a remote peer starts many abandoned
            // searches. Old cursors become stale rather than holding a tree.
            self.actions.action_directory_cursors.clear();
        }
        let key = self.action_directory_cursor_key();
        self.actions
            .action_directory_cursors
            .insert(key.clone(), state);
        key.into_bytes()
    }

    #[allow(
        clippy::too_many_lines,
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "every directory arithmetic/index is guarded by the hard page budgets and loop bounds directly above it"
    )]
    /// The resources `query` names by kind or label, whole: at the top
    /// level, or among the children of its parent resource when it names
    /// one. A write drops the published snapshot until the next publication,
    /// and a listing in between walks the widget tree a budgeted page at a
    /// time: read from that, a name lookup would depend on where the resource
    /// falls in the tree. The resources about to be published answer it
    /// instead. A name under a parent costs the parent's children compared,
    /// never a page of them sent: an agent finds one note in a notebook of
    /// thousands in one answer. With a published snapshot, the children it
    /// holds answer; a child only the live resource extends it with is
    /// found by listing the parent.
    fn action_directory_named(
        &mut self,
        query: &runtime_collab::ActionDirectoryQuery,
        revision: String,
    ) -> runtime_collab::ActionDirectoryPage {
        let name = query.resource_name.as_str();
        let max_results = usize::try_from(query.max_results)
            .unwrap_or(ACTION_DIRECTORY_MAX_RESULTS)
            .clamp(1, ACTION_DIRECTORY_MAX_RESULTS);
        let max_bytes = usize::try_from(query.max_bytes)
            .unwrap_or(ACTION_DIRECTORY_MAX_BYTES)
            .clamp(1, ACTION_DIRECTORY_MAX_BYTES);
        let parent = query.parent_resource.as_str();
        let named = |kind: &str, label: &str| kind == name || label.eq_ignore_ascii_case(name);
        let found: Vec<_> = if let Some(snapshot) = &self.actions.action_directory_snapshot {
            let rows = if parent.is_empty() {
                Some(&snapshot.roots)
            } else {
                snapshot.nodes.get(parent).map(|node| &node.rows)
            };
            rows.into_iter()
                .flatten()
                .filter(|row| {
                    row.action.is_none() && named(&row.resource_kind, &row.resource_label)
                })
                .cloned()
                .collect()
        } else if parent.is_empty() {
            self.publication_resources()
                .iter()
                .filter(|resource| named(&resource.kind, &resource.label))
                .filter_map(|resource| self.directory_resource_row(resource, "", true))
                .collect()
        } else {
            fn find<'a>(
                resources: &'a [tilo_actions::ResourceSpec],
                id: &str,
            ) -> Option<&'a tilo_actions::ResourceSpec> {
                resources.iter().find_map(|resource| {
                    (resource.id.as_str() == id)
                        .then_some(resource)
                        .or_else(|| find(&resource.children, id))
                })
            }
            let published = self.publication_resources();
            let children = match find(&published, parent) {
                Some(resource) => resource.children.clone(),
                None => self
                    .resolve_publication_resource(parent, "")
                    .map(|resource| resource.children)
                    .unwrap_or_default(),
            };
            children
                .iter()
                .filter(|child| named(&child.kind, &child.label))
                .filter_map(|child| self.directory_resource_row(child, parent, true))
                .collect()
        };
        let mut rows = Vec::new();
        let mut bytes = DIRECTORY_PAGE_WIRE_BYTES.saturating_add(revision.len());
        for mut row in found.into_iter().take(max_results) {
            row.has_children &= query.max_depth > 0;
            bytes = bytes.saturating_add(runtime_collab::action_directory_row_wire_bytes(&row));
            if bytes > max_bytes {
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows: Vec::new(),
                    next_cursor: Vec::new(),
                    exhausted: false,
                    stale_cursor: false,
                    budget_exhausted: true,
                };
            }
            rows.push(row);
        }
        runtime_collab::ActionDirectoryPage {
            revision,
            rows,
            next_cursor: Vec::new(),
            exhausted: true,
            stale_cursor: false,
            budget_exhausted: false,
        }
    }

    fn action_directory_page(
        &mut self,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> runtime_collab::ActionDirectoryPage {
        let Ok(revision) = self.try_action_directory_revision() else {
            return runtime_collab::ActionDirectoryPage {
                revision: String::new(),
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: !query.revision.is_empty(),
                budget_exhausted: true,
            };
        };
        let requested_revision = query.revision.clone();
        if !requested_revision.is_empty() && requested_revision != revision {
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: true,
                budget_exhausted: false,
            };
        }
        if !query.resource_name.is_empty() {
            return self.action_directory_named(&query, revision);
        }
        let max_results = usize::try_from(query.max_results)
            .unwrap_or(ACTION_DIRECTORY_MAX_RESULTS)
            .clamp(1, ACTION_DIRECTORY_MAX_RESULTS);
        let max_work = usize::try_from(query.max_work)
            .unwrap_or(ACTION_DIRECTORY_MAX_WORK)
            .clamp(1, ACTION_DIRECTORY_MAX_WORK);
        let max_bytes = usize::try_from(query.max_bytes)
            .unwrap_or(ACTION_DIRECTORY_MAX_BYTES)
            .clamp(1, ACTION_DIRECTORY_MAX_BYTES);
        let max_depth = query.max_depth.min(ACTION_DIRECTORY_MAX_DEPTH);
        let cursor_key = if query.cursor.is_empty() {
            None
        } else {
            match String::from_utf8(query.cursor) {
                Ok(cursor) => Some(cursor),
                Err(_) => {
                    return runtime_collab::ActionDirectoryPage {
                        revision,
                        rows: Vec::new(),
                        next_cursor: Vec::new(),
                        exhausted: false,
                        stale_cursor: true,
                        budget_exhausted: false,
                    };
                }
            }
        };
        let previous = cursor_key
            .as_deref()
            .and_then(|key| self.actions.action_directory_cursors.remove(key));
        if cursor_key.is_some() && previous.is_none() {
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: true,
                budget_exhausted: false,
            };
        }
        let mut state = previous.unwrap_or_else(|| ActionDirectoryCursor {
            revision: revision.clone(),
            needle: query.needle.clone(),
            parent_resource: query.parent_resource.clone(),
            provider_index: 0,
            provider_cursor: None,
            browse_offset: 0,
            root_widget_index: 0,
            root_provider_cursor: None,
        });
        if state.revision != revision
            || state.needle != query.needle
            || state.parent_resource != query.parent_resource
        {
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: true,
                budget_exhausted: false,
            };
        }

        let mut rows = Vec::new();
        let mut estimated_bytes = DIRECTORY_PAGE_WIRE_BYTES.saturating_add(revision.len());
        let add = |row: runtime_collab::ActionDirectoryRow,
                   rows: &mut Vec<runtime_collab::ActionDirectoryRow>,
                   bytes: &mut usize| {
            *bytes = bytes.saturating_add(runtime_collab::action_directory_row_wire_bytes(&row));
            rows.push(row);
        };

        if query.needle.is_empty() {
            if query.parent_resource.is_empty() {
                let mut widget_index = state.root_widget_index;
                let mut provider_cursor = state.root_provider_cursor.clone();
                let mut inspected = 0usize;
                let mut exhausted = false;
                let mut projected_offset = state.browse_offset;
                let projected_done = if let Some(snapshot) = &self.actions.action_directory_snapshot
                {
                    while let Some(row) = snapshot.roots.get(projected_offset) {
                        if rows.len() >= max_results || inspected >= max_work {
                            break;
                        }
                        let mut row = row.clone();
                        row.has_children &= max_depth > 0;
                        add(row, &mut rows, &mut estimated_bytes);
                        if estimated_bytes > max_bytes {
                            rows.pop();
                            break;
                        }
                        projected_offset = projected_offset.saturating_add(1);
                        inspected = inspected.saturating_add(1);
                    }
                    projected_offset >= snapshot.roots.len()
                } else {
                    true
                };
                while projected_done && rows.len() < max_results && inspected < max_work {
                    let Some(page) = crate::widget::action_directory_page_at(
                        self.root_widget.as_ref(),
                        &self.state.doc,
                        widget_index,
                        provider_cursor.as_deref().unwrap_or_default(),
                        1,
                    ) else {
                        exhausted = true;
                        break;
                    };
                    let Some((mut page, binding)) = page else {
                        widget_index = widget_index.saturating_add(1);
                        provider_cursor = None;
                        inspected = inspected.saturating_add(1);
                        continue;
                    };
                    if page.resources.len() > 1 || (!page.exhausted && page.next_cursor.is_empty())
                    {
                        // The declared page contract was violated. A hidden
                        // tail is worse than no remote hierarchy at all.
                        return runtime_collab::ActionDirectoryPage {
                            revision,
                            rows: Vec::new(),
                            next_cursor: Vec::new(),
                            exhausted: false,
                            stale_cursor: false,
                            budget_exhausted: true,
                        };
                    }
                    if let Some(binding) = binding {
                        if self.require_bound_broker_route(&binding).is_ok()
                            && crate::bound_dispatch::validate_bound_resources(
                                self.root_widget.as_mut(),
                                &binding,
                                &page.resources,
                            )
                            .unwrap_or(false)
                        {
                            page.resources = page
                                .resources
                                .into_iter()
                                .filter_map(|resource| {
                                    crate::bound_dispatch::filter_resource(&binding, resource)
                                })
                                .filter_map(crate::bound_dispatch::published_here)
                                .filter_map(|resource| {
                                    self.actions
                                        .bound_publication
                                        .project(&binding, resource)
                                        .ok()
                                })
                                .collect();
                        } else {
                            page.resources.clear();
                        }
                    } else {
                        page.resources.retain(|resource| {
                            !resource
                                .id
                                .as_str()
                                .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
                        });
                    }
                    let before_index = widget_index;
                    let before_cursor = provider_cursor.clone();
                    if let Some(resource) = page.resources.first()
                        && !self.actions.action_directory_snapshot.as_ref().is_some_and(
                            |snapshot| snapshot.nodes.contains_key(resource.id.as_str()),
                        )
                        && let Some(row) = self.directory_resource_row(resource, "", max_depth > 0)
                    {
                        add(row, &mut rows, &mut estimated_bytes);
                        if estimated_bytes > max_bytes {
                            rows.pop();
                            break;
                        }
                    }
                    if page.exhausted {
                        widget_index = widget_index.saturating_add(1);
                        provider_cursor = None;
                    } else {
                        provider_cursor = Some(page.next_cursor);
                    }
                    inspected = inspected.saturating_add(1);
                    if rows.len() >= max_results {
                        break;
                    }
                    // An empty non-exhausted source page may not advance
                    // indefinitely; its opaque cursor must still change.
                    if rows.is_empty()
                        && before_index == widget_index
                        && before_cursor == provider_cursor
                    {
                        return runtime_collab::ActionDirectoryPage {
                            revision,
                            rows: Vec::new(),
                            next_cursor: Vec::new(),
                            exhausted: false,
                            stale_cursor: false,
                            budget_exhausted: true,
                        };
                    }
                }
                let next_cursor = (!exhausted).then(|| {
                    self.action_directory_next_cursor(ActionDirectoryCursor {
                        browse_offset: projected_offset,
                        root_widget_index: widget_index,
                        root_provider_cursor: provider_cursor,
                        ..state
                    })
                });
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows,
                    next_cursor: next_cursor.unwrap_or_default(),
                    exhausted,
                    stale_cursor: false,
                    budget_exhausted: !exhausted,
                };
            }
            let mut offset = state.browse_offset;
            let mut inspected = 0usize;
            let mut projected_total = 0usize;
            if let Some(node) = self
                .actions
                .action_directory_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.nodes.get(&query.parent_resource))
            {
                projected_total = node.rows.len();
                while let Some(row) = node.rows.get(offset) {
                    if rows.len() >= max_results || inspected >= max_work {
                        break;
                    }
                    let mut row = row.clone();
                    row.has_children &= max_depth > 0
                        && node.depth.saturating_add(1) < usize::from(ACTION_DIRECTORY_MAX_DEPTH);
                    add(row, &mut rows, &mut estimated_bytes);
                    if estimated_bytes > max_bytes {
                        rows.pop();
                        break;
                    }
                    offset = offset.saturating_add(1);
                    inspected = inspected.saturating_add(1);
                }
            }
            if offset < projected_total || rows.len() >= max_results || inspected >= max_work {
                let next_cursor = self.action_directory_next_cursor(ActionDirectoryCursor {
                    browse_offset: offset,
                    ..state
                });
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows,
                    next_cursor,
                    exhausted: false,
                    stale_cursor: false,
                    budget_exhausted: true,
                };
            }
            let parent = self.resolve_publication_resource(&query.parent_resource, "");
            let parent_actions = parent.as_ref().map_or_else(Vec::new, |resource| {
                resource
                    .actions
                    .iter()
                    .map(|action| action.id.clone())
                    .collect()
            });
            let direct = parent
                .as_ref()
                .map_or_else(Vec::new, |resource| resource.children.clone());
            let total = projected_total
                .saturating_add(parent_actions.len())
                .saturating_add(direct.len());
            while offset < total && rows.len() < max_results && inspected < max_work {
                let provider_offset = offset.saturating_sub(projected_total);
                let row = if provider_offset < parent_actions.len() {
                    let action = &parent_actions[provider_offset];
                    let projected =
                        self.actions
                            .action_directory_snapshot
                            .as_ref()
                            .is_some_and(|snapshot| {
                                snapshot
                                    .actions
                                    .contains_key(&tilo_actions::ActionAddress::new(
                                        &query.parent_resource,
                                        action,
                                    ))
                            });
                    if projected {
                        None
                    } else {
                        parent
                            .as_ref()
                            .and_then(|resource| Self::directory_action_row(resource, action))
                    }
                } else {
                    let resource = &direct[provider_offset - parent_actions.len()];
                    if self
                        .actions
                        .action_directory_snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.nodes.contains_key(resource.id.as_str()))
                    {
                        None
                    } else {
                        self.directory_resource_row(resource, &query.parent_resource, max_depth > 0)
                    }
                };
                if let Some(row) = row {
                    add(row, &mut rows, &mut estimated_bytes);
                    if estimated_bytes > max_bytes {
                        rows.pop();
                        break;
                    }
                }
                offset += 1;
                inspected += 1;
            }
            let exhausted = offset >= total;
            let next_cursor = (!exhausted).then(|| {
                self.action_directory_next_cursor(ActionDirectoryCursor {
                    browse_offset: offset,
                    ..state
                })
            });
            return runtime_collab::ActionDirectoryPage {
                revision,
                rows,
                next_cursor: next_cursor.unwrap_or_default(),
                exhausted,
                stale_cursor: false,
                budget_exhausted: !exhausted,
            };
        }

        let providers = crate::bound_dispatch::search_providers(self.root_widget.as_ref())
            .into_iter()
            .filter(|provider| provider.action_directory_generation().is_some())
            .collect::<Vec<_>>();
        let mut provider_index = state.provider_index;
        let mut provider_cursor = state.provider_cursor;
        let mut work_left = max_work;
        let mut byte_exhausted = false;
        while provider_index < providers.len() && rows.len() < max_results && work_left > 0 {
            if provider_cursor.as_ref().is_some_and(|cursor| {
                !providers[provider_index].action_directory_cursor_is_current(cursor)
            }) {
                return runtime_collab::ActionDirectoryPage {
                    revision,
                    rows: Vec::new(),
                    next_cursor: Vec::new(),
                    exhausted: false,
                    stale_cursor: true,
                    budget_exhausted: false,
                };
            }
            // Request one row at a time.  The provider's opaque cursor is
            // after its returned page, so asking for a larger page and then
            // stopping at the byte ceiling would silently skip the unfit tail.
            // This preserves every canonical result while still bounding this
            // request to `max_results` provider steps.
            let room = 1;
            let page = providers[provider_index].search_page(
                &crate::search::SearchQuery::new(query.needle.as_str()),
                provider_cursor.as_ref(),
                work_left,
                room,
            );
            work_left = work_left.saturating_sub(page.inspected);
            for hit in page.results {
                let Some(address) = hit.action_address() else {
                    continue;
                };
                let binding = providers[provider_index].callback_binding();
                if binding
                    .as_ref()
                    .is_some_and(|binding| self.require_bound_broker_route(binding).is_err())
                {
                    continue;
                }
                let resource = providers[provider_index]
                    .resolve_action_resource(&address)
                    .or_else(|| {
                        if let Some(binding) = &binding {
                            crate::bound_dispatch::resolve_component_resource(
                                self.root_widget.as_mut(),
                                &self.state.doc,
                                binding,
                                &address.resource,
                                &address.action,
                            )
                            .ok()
                            .flatten()
                        } else {
                            crate::bound_dispatch::resolve_unbound_resource(
                                self.root_widget.as_ref(),
                                &self.state.doc,
                                &address.resource,
                            )
                        }
                    })
                    .and_then(|resource| {
                        binding.as_ref().map_or(Some(resource.clone()), |binding| {
                            crate::bound_dispatch::filter_resource(binding, resource)
                        })
                    });
                let Some(mut resource) = resource.and_then(crate::bound_dispatch::published_here)
                else {
                    continue;
                };
                if let Some(binding) = providers[provider_index].callback_binding() {
                    if !crate::bound_dispatch::validate_bound_resources(
                        self.root_widget.as_mut(),
                        &binding,
                        std::slice::from_ref(&resource),
                    )
                    .unwrap_or(false)
                    {
                        continue;
                    }
                    let Ok(projected) = self.actions.bound_publication.project(&binding, resource)
                    else {
                        continue;
                    };
                    resource = projected;
                } else if resource
                    .id
                    .as_str()
                    .starts_with(crate::bound_dispatch::PUBLICATION_PREFIX)
                {
                    continue;
                }
                let Some(row) = Self::directory_action_row(&resource, &address.action) else {
                    continue;
                };
                let before = estimated_bytes;
                add(row, &mut rows, &mut estimated_bytes);
                if estimated_bytes > max_bytes {
                    rows.pop();
                    estimated_bytes = before;
                    byte_exhausted = true;
                    break;
                }
            }
            if byte_exhausted {
                break;
            }
            if let Some(next) = page.next {
                provider_cursor = Some(next);
                if rows.len() < max_results && estimated_bytes < max_bytes && work_left > 0 {
                    continue;
                }
                break;
            }
            provider_index += 1;
            provider_cursor = None;
        }
        // Actions an app publishes through its resource tree rather than a
        // search provider are found by name too, in tree order, resuming at
        // the offset of the next unreturned match.
        let mut tree_exhausted = true;
        if !byte_exhausted && provider_index >= providers.len() {
            let needle = query.needle.to_lowercase();
            let mut matches = Vec::new();
            for resource in self.publication_resources() {
                collect_named_actions(
                    &resource,
                    &needle,
                    0,
                    ACTION_DIRECTORY_MAX_DEPTH,
                    &mut matches,
                );
            }
            let mut offset = state.browse_offset;
            while let Some((resource, action)) = matches.get(offset) {
                if rows.len() >= max_results {
                    break;
                }
                if let Some(row) = Self::directory_action_row(resource, action) {
                    add(row, &mut rows, &mut estimated_bytes);
                    if estimated_bytes > max_bytes {
                        rows.pop();
                        byte_exhausted = true;
                        break;
                    }
                }
                offset += 1;
            }
            tree_exhausted = offset >= matches.len();
            state.browse_offset = offset;
        }
        let exhausted = !byte_exhausted && provider_index >= providers.len() && tree_exhausted;
        // Keep the source cursor *before* an oversized row when earlier rows
        // fitted. A caller that raises maxBytes resumes exactly at that row;
        // dropping it would restart and duplicate the fitted prefix.
        let next_cursor = (!exhausted && (!byte_exhausted || !rows.is_empty())).then(|| {
            self.action_directory_next_cursor(ActionDirectoryCursor {
                provider_index,
                provider_cursor,
                ..state
            })
        });
        runtime_collab::ActionDirectoryPage {
            revision,
            rows,
            next_cursor: next_cursor.unwrap_or_default(),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }

    fn resolve_publication_resource(
        &mut self,
        resource_id: &str,
        action_id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        let address = tilo_actions::ActionAddress::new(resource_id, action_id);
        match self.actions.bound_publication.target(&address) {
            Ok(Some(target)) => {
                self.require_bound_broker_route(&target.binding).ok()?;
                let resource = crate::bound_dispatch::resolve_component_resource(
                    self.root_widget.as_mut(),
                    &self.state.doc,
                    &target.binding,
                    &target.address.address.resource,
                    action_id,
                )
                .ok()
                .flatten()?;
                if !crate::bound_dispatch::validate_bound_resources(
                    self.root_widget.as_mut(),
                    &target.binding,
                    std::slice::from_ref(&resource),
                )
                .unwrap_or(false)
                {
                    return None;
                }
                self.actions
                    .bound_publication
                    .project(&target.binding, resource)
                    .ok()
            }
            Err(_) => None,
            Ok(None) => crate::bound_dispatch::resolve_unbound_resource(
                self.root_widget.as_ref(),
                &self.state.doc,
                resource_id,
            )
            .or_else(|| {
                crate::bound_dispatch::search_providers(self.root_widget.as_ref())
                    .into_iter()
                    .filter(|provider| {
                        provider.callback_binding().is_none()
                            && provider.action_directory_generation().is_some()
                    })
                    .find_map(|provider| provider.resolve_action_resource(&address))
            }),
        }
    }

    /// Whether `resource_id`, which resolves to nothing now, may resolve
    /// once what its widget reads lands
    /// ([`crate::resources::PublishesResources::resource_pending`]).
    fn publication_resource_pending(&mut self, resource_id: &str, action_id: &str) -> bool {
        let address = tilo_actions::ActionAddress::new(resource_id, action_id);
        match self.actions.bound_publication.target(&address) {
            Ok(Some(target)) => crate::bound_dispatch::component_resource_pending(
                self.root_widget.as_mut(),
                &target.binding,
                &target.address.address.resource,
            ),
            Err(_) => false,
            Ok(None) => crate::bound_dispatch::unbound_resource_pending(
                self.root_widget.as_ref(),
                resource_id,
            ),
        }
    }

    fn resolve_action_directory(
        &mut self,
        revision: &str,
        resource_id: &str,
        action_id: &str,
    ) -> runtime_collab::ActionDirectoryResolution {
        let Ok(current) = self.try_action_directory_revision() else {
            return runtime_collab::ActionDirectoryResolution::Unavailable;
        };
        // No revision asks for the directory as it is now: a caller that
        // names an action it already knows (an agent's) chose it from no
        // page, and reading the revision first would cost it a round trip.
        // The answer names the revision it resolved against either way, so
        // the broker binds the admission to it and a host refuses to run
        // the action once the directory moved on.
        if !revision.is_empty() && revision != current {
            return runtime_collab::ActionDirectoryResolution::StaleRevision;
        }
        let found = |row| runtime_collab::ActionDirectoryResolution::Found {
            row: Box::new(row),
            revision: Some(current.clone()),
        };
        if let Some(row) = self
            .actions
            .action_directory_snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .actions
                    .get(&tilo_actions::ActionAddress::new(resource_id, action_id))
            })
        {
            return found(row.clone());
        }
        let resource = self.resolve_publication_resource(resource_id, action_id);
        resource
            .as_ref()
            .and_then(|resource| Self::directory_action_row(resource, action_id))
            .map_or(runtime_collab::ActionDirectoryResolution::NotFound, found)
    }

    fn published_action_directory_page(
        &mut self,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> runtime_collab::ActionDirectoryPage {
        if self.published_action_history_ready() {
            self.action_directory_page(query)
        } else {
            runtime_collab::ActionDirectoryPage {
                revision: String::new(),
                rows: Vec::new(),
                next_cursor: Vec::new(),
                exhausted: false,
                stale_cursor: !query.revision.is_empty(),
                budget_exhausted: true,
            }
        }
    }

    fn published_action_directory_resolution(
        &mut self,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> runtime_collab::ActionDirectoryResolution {
        if self.published_action_history_ready() {
            self.resolve_action_directory(revision, resource, action)
        } else {
            runtime_collab::ActionDirectoryResolution::Unavailable
        }
    }

    /// Answer the resolutions held for a resource that was still being read
    /// and no longer is: found, or not found once its read ended. One whose
    /// caller went away is dropped.
    fn answer_awaited_resolutions(&mut self) {
        let awaited = std::mem::take(&mut self.actions.awaited_resolutions);
        for awaited in awaited {
            if awaited.reply.is_closed() {
                continue;
            }
            if self.publication_resource_pending(&awaited.resource, &awaited.action) {
                self.actions.awaited_resolutions.push(awaited);
                continue;
            }
            let _ = awaited
                .reply
                .send(self.published_action_directory_resolution(
                    &awaited.revision,
                    &awaited.resource,
                    &awaited.action,
                ));
        }
    }

    /// Answer `asked` now, or once its resource is read when it is still
    /// being read: at the pump the read's landing wakes.
    fn resolve_or_await(&mut self, asked: AwaitedResolution) {
        let resolution = self.published_action_directory_resolution(
            &asked.revision,
            &asked.resource,
            &asked.action,
        );
        if matches!(
            resolution,
            runtime_collab::ActionDirectoryResolution::NotFound
        ) && self.publication_resource_pending(&asked.resource, &asked.action)
        {
            self.actions.awaited_resolutions.push(asked);
            return;
        }
        let _ = asked.reply.send(resolution);
    }

    fn pump_action_directory_requests(&mut self) {
        self.answer_awaited_resolutions();
        for request in self.collaboration.manager.take_action_directory_requests() {
            match request {
                ActionDirectoryRequest::ResolveLocalDocumentAction {
                    revision,
                    resource,
                    action,
                    reply,
                } => {
                    let _ = reply.send(
                        self.resolve_local_document_action_directory(&revision, &resource, &action),
                    );
                }
                ActionDirectoryRequest::Query { query, reply } => {
                    // A cancelled caller never receives this page. Do not consume
                    // its one-use continuation before admitting a live response.
                    if !reply.is_closed() {
                        let _ = reply.send(self.published_action_directory_page(query));
                    }
                }
                ActionDirectoryRequest::Resolve {
                    revision,
                    resource,
                    action,
                    reply,
                } => self.resolve_or_await(AwaitedResolution {
                    revision,
                    resource,
                    action,
                    reply,
                }),
            }
        }
    }
}
