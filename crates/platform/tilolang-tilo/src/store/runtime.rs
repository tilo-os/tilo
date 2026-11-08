//! What the runtime asks of the store: the [`Store`](tilolang_runtime::store::Store) trait over
//! the mirror, the indexes and the outbox.

use super::*;

impl Store for TiloStore {
    fn reads(&self) -> tilolang_runtime::Reads {
        let stats = self.read_stats();
        tilolang_runtime::Reads {
            record: stats
                .doc
                .located
                .saturating_add(stats.doc.fields)
                .saturating_add(stats.doc.listings)
                .saturating_add(stats.doc.entries),
            index: stats
                .index
                .dirs
                .saturating_add(stats.index.pages)
                .saturating_add(stats.index.fields),
            evaluated: stats.evaluated,
        }
    }

    /// The store holds only the records its reader was sent: a resource of a package here
    /// reads by what it is called now when the store holds it, as not shared when it does not.
    fn reading(
        &self,
        reference: &tilolang_runtime::rich::Reference,
    ) -> tilolang_runtime::rich::Reading {
        crate::reading::reading(self, reference)
    }

    /// The store answers from every chunk the record names once it has read them all: until
    /// then what it lists is the part it has read, and a count of it is not the workspace's.
    fn settled(&self) -> bool {
        self.index_whole()
    }

    fn fresh_id(&mut self, prefix: &str) -> String {
        self.changing().fresh(prefix)
    }

    fn create(&mut self, record: &NewRecord) -> Result<ResourceRef, StoreError> {
        let id = self.changing().fresh("r");
        self.create_with_id(record, &id)
    }

    fn create_with_id(&mut self, record: &NewRecord, id: &str) -> Result<ResourceRef, StoreError> {
        Self::create_with_id(self, record, id)
    }

    fn create_in(
        &mut self,
        txn: Txn,
        record: &NewRecord,
        id: &str,
    ) -> Result<ResourceRef, StoreError> {
        Self::create_in(self, txn, record, id)
    }

    fn type_of(&self, at: &ResourceRef) -> Result<TypeId, StoreError> {
        let _ = self.load_at(at);
        let inner = self.held();
        let (app, ty) = inner.locate(at)?;
        if let Some(view) = inner.view(&app) {
            return view
                .type_of(&at.id_string())
                .map_err(backend)?
                .ok_or_else(|| not_found(format!("resource {}", at.id_string())));
        }
        let mirror = inner.held_doc(&at.doc)?;
        let located = locate_ref(mirror, Some(&inner.chunks), (&app, &ty), at, &inner.nested)?;
        let source = inner.chunks.mirror(located.chunk).unwrap_or(mirror);
        Ok(located
            .obj
            .as_ref()
            .and_then(|obj| type_at(source, obj))
            .unwrap_or_else(|| located.schema.id.clone()))
    }

    /// What links to `at`, read from each package's index: the one list its links are kept in
    /// ([`linked_list`]), as the store's open writes leave it. A resource gone from its index
    /// links to nothing.
    fn linked(&self, at: &ResourceRef) -> Result<Vec<Link>, StoreError> {
        let inner = self.held();
        let target = at.id_string();
        let list = linked_list(&target);
        let mut out: Vec<Link> = Vec::new();
        for held in inner.apps.values().filter(|held| held.answers()) {
            let snapshot = &held.snapshot;
            let pages = snapshot.pages(&list).map_err(backend)?;
            let mut entries: Vec<Entry> = (0..pages.len())
                .filter_map(|at| pages.entry(at))
                .map(|(key, id)| (key, id.to_string()))
                .collect();
            match inner.overlay.list(&target, &list) {
                None => {}
                Some(ListState::Whole(written)) => entries = written,
                Some(ListState::Delta { added, removed }) => {
                    entries.retain(|entry| !removed.contains(entry));
                    entries.extend(added);
                }
            }
            for (key, source) in entries {
                let Some((how, _)) = split_link(&key) else {
                    continue;
                };
                let ty = match inner.overlay.text(&source, idx::TYPE) {
                    Some(ty) => ty,
                    None => snapshot.text(&source, idx::TYPE).map_err(backend)?,
                };
                // A resource the open writes removed, or that its index no longer holds, links
                // to nothing.
                let Some(ty) = ty else {
                    continue;
                };
                let Some(of) = ResourceRef::parse_id(&source, ty) else {
                    continue;
                };
                out.push(Link { of, how });
            }
        }
        out.sort_by_key(Link::order);
        out.dedup_by(|a, b| a.of.id_string() == b.of.id_string() && a.how == b.how);
        Ok(out)
    }

    fn begin(&mut self) -> Result<Txn, StoreError> {
        self.load_record()?;
        let mut inner = self.mutate();
        let id = inner.next_txn;
        inner.next_txn = inner.next_txn.wrapping_add(1);
        let start = inner.local.len();
        inner.txns.insert(
            id,
            TxnState {
                start,
                ..TxnState::default()
            },
        );
        Ok(Txn(id))
    }

    fn apply(&mut self, txn: Txn, at: &ResourceRef, m: &Mutation) -> Result<Applied, StoreError> {
        // A write into a chunk the store was not sent loads it first: planning reads the
        // resource's map, and writing where its chunk is not held would start the chunk empty
        // and lose what it holds.
        self.load_at(at)?;
        let mut inner = self.mutate();
        if !inner.txns.contains_key(&txn.0) {
            return Err(invalid("no such transaction"));
        }
        let (app, ty) = inner.locate(at)?;
        let planned = inner.plan(at, m)?;
        let needed = planned_unheld(&inner, &planned);
        let planned = if needed.is_empty() {
            planned
        } else {
            drop(inner);
            for chunk in needed {
                self.load_chunk(chunk)?;
            }
            inner = self.mutate();
            inner.plan(at, m)?
        };
        let Planned {
            edits: dyn_edits,
            item,
            touched,
            items,
            placed,
        } = planned;
        let seed = format!("{}:{}", txn.0, inner.counter).into_bytes();
        let summary = touched.clone();
        let collections = collections_by_doc(items.iter().map(|(key, _)| key));
        let listings = inner.write(&app, dyn_edits.clone(), &seed, &touched, (items, placed))?;
        let kind = lookup(&ty).map(|schema| format!("tl.{app}.{}", schema.name));
        let state = inner
            .txns
            .get_mut(&txn.0)
            .ok_or_else(|| invalid("no such transaction"))?;
        // A change writes one app's resources. The host's are the workspace's own — a notice
        // rides in the change that told the person, whichever app's action it was
        // ([`crate::notices`]) — so they join any app's change; the change takes the app of
        // what else it wrote, and the host's only when that is all it wrote.
        let own = app.as_str() == tilolang::HOST_APP;
        if state
            .app
            .as_deref()
            .is_some_and(|held| held != app && held != tilolang::HOST_APP && !own)
        {
            return Err(invalid("a transaction cannot write resources of two apps"));
        }
        let joining =
            state.app.is_none() || (state.app.as_deref() == Some(tilolang::HOST_APP) && !own);
        if joining {
            state.app = Some(app);
            state.kind = kind;
        }
        // What its origin derives lives in the store's unsaved layer; the rest is the record's.
        if lookup(&ty).is_some_and(crate::schema::TypeSchema::derived) {
            state.derived = state.derived.saturating_add(1);
        } else {
            state.saved = state.saved.saturating_add(1);
        }
        state.edits.extend(dyn_edits);
        for change in listings.into_values().flatten() {
            record_splices(&mut state.listings, &change.key, change.splices);
        }
        let touched = state.touched.entry(at.doc.clone()).or_default();
        touched.extend(summary);
        for (doc, keys) in collections {
            state.collections.entry(doc).or_default().extend(keys);
        }
        Ok(item.map_or(Applied::Done, Applied::Item))
    }

    fn container(&self, at: &ResourceRef) -> Result<Option<(ResourceRef, FieldId)>, StoreError> {
        self.held().container(at)
    }

    fn commit(&mut self, txn: Txn, note: &ChangeNote) -> Result<(), StoreError> {
        let mut inner = self.mutate();
        let mut state = inner
            .txns
            .remove(&txn.0)
            .ok_or_else(|| invalid("no such transaction"))?;
        // A migration that found nothing to change still ran: its marker is a change of the
        // record like any other, and the next open reads it instead of asking again.
        let migration = inner
            .migrating
            .clone()
            .filter(|(migration, _)| *migration == note.action);
        // A trigger's run records what it did for its resource in the same change: only the
        // invocation of the app by itself is one.
        let triggering = inner.triggering.clone().and_then(|triggering| {
            let (app, _) = inner.index.get(&triggering.doc)?;
            (note.invoker == crate::authority::trigger_principal(app).0).then_some(triggering)
        });
        // A migration or a trigger that changed nothing still ran: its mark is a change of the
        // record like any other, and the next open or wake reads it instead of asking again.
        // A trigger whose own record is not among what it wrote — one that only told someone
        // something, say — ran too, and records it, so it runs no second time for that instant.
        let own = triggering
            .as_ref()
            .is_some_and(|triggering| !state.touched.contains_key(&triggering.doc));
        let mut marked = false;
        if (state.edits.is_empty() || own)
            && let Some((doc, edit)) = mark_of(migration.as_ref(), triggering.as_ref())
        {
            if !inner.mark(&mut state, txn, &doc, edit)? {
                return Ok(());
            }
            marked = true;
        }
        let (Some(app), Some(kind)) = (state.app.clone(), state.kind.clone()) else {
            return Ok(());
        };
        if state.edits.is_empty() {
            return Ok(());
        }
        // What its origin derives is the store's unsaved layer: it was applied to the mirror's
        // open transaction and is read through the overlay, and nothing of it is committed,
        // sent or versioned. It stays there until the record moves on, which leaves it in the
        // mirror ([`Inner::drop_committed_local`]). An action writes one kind or the other;
        // one that tried both is a fault, and its derived writes still stay unsaved.
        if unsaved::commit_derived(&mut inner, &mut state, &note.invoker) {
            return Ok(());
        }
        if state.derived > 0 {
            tracing::error!(
                app,
                saved = state.saved,
                derived = state.derived,
                "an action wrote both the record's resources and derived ones: the derived ones stay unsaved"
            );
        }
        // The change records, with what it did, that a migration or a trigger ran on its
        // resource.
        if !marked && let Some((doc, edit)) = mark_of(migration.as_ref(), triggering.as_ref()) {
            inner.mark(&mut state, txn, &doc, edit)?;
        }
        let versioned = app.clone();
        let (action_id, args) = published_name(note);
        // A change is one app's, and the record stores each edit under the app its resource is
        // of: what the change wrote of another — a notice of the host's, riding in the change
        // that told its person — is addressed to that app, as the store that wrote it read it.
        let edits: Vec<DynEdit> = state
            .edits
            .into_iter()
            .map(|edit| match inner.index.get(edit.resource()) {
                Some((held, _)) if *held != app => edit.in_app(held),
                _ => edit,
            })
            .collect();
        let action = DynAction {
            app,
            kind,
            action_id,
            args,
            authorship: Authorship {
                invoker: note.invoker.clone(),
                chain: note.chain.clone(),
                module: note.module.clone(),
                at: note.at.clone(),
                from: note.from.clone(),
                suggested_by: note.suggested_by.clone(),
                count: note.count.max(1),
                invocation: note.invocation.clone(),
            },
            edits,
        };
        let made = action.made();
        inner.enqueue(action);
        let mut touched = state.touched;
        for (doc, keys) in inner.provisional_versions(&versioned, state.start, note, &made) {
            touched.entry(doc).or_default().extend(keys);
        }
        // What changed is told to those who read the mirror once, when the record brings the
        // change back and the indexes hold what it derives ([`TiloStore::sync`]): told
        // here, it would be a second note of the same step, and one lacking what the record's
        // derivation adds. Until then the write is the mirror's open transaction, read through
        // the overlay like any other.
        for doc in touched.into_keys() {
            inner.notes.entry(doc).or_default().push(note.clone());
        }
        Ok(())
    }

    fn rollback(&mut self, txn: Txn) {
        let mut inner = self.mutate();
        if let Some(state) = inner.txns.remove(&txn.0) {
            inner.drop_local_from(state.start);
            inner.reapply();
            // What it made is not in the record the mirror was put back to, and the index is not
            // rebuilt from the mirror: it is taken out here.
            for id in &state.created {
                inner.index.remove(id);
            }
            inner.rebuild_overlay();
            inner.changed();
        }
    }

    fn begin_run(&mut self, run: &str) {
        if self.load_record().is_ok() {
            self.mutate().begin_run(run);
        }
    }

    fn end_run(&mut self, run: &str) {
        self.mutate().end_run(run);
    }

    fn abort_run(&mut self, run: &str) {
        self.mutate().abort_run(run);
    }

    fn read(&self, at: &ResourceRef, field: &FieldSpec) -> Result<FieldValue, StoreError> {
        // The value the index answers for the field: a field of a row it derived, or the items
        // of a collection. What it does not hold is read from the chunk the resource's map is
        // in, which the store reads when it was not sent it.
        let indexed: Option<FieldValue> = {
            let inner = self.held();
            let (app, _) = inner.locate(at)?;
            match inner.view(&app) {
                None => None,
                Some(view) => {
                    let resource = at.id_string();
                    let Some(stored) = view.type_of(&resource).map_err(backend)? else {
                        // A record the index cannot name yet: an origin's write is derived, and the
                        // record it makes (or moves on) is read here before the next derivation
                        // names it. The overlay the write made holds its fields, so a field it
                        // wrote reads from there rather than as if the record were gone.
                        if let Ok(Some(written)) = view.field(&resource, &field.id) {
                            return Ok(written);
                        }
                        return Err(not_found(format!("resource {resource}")));
                    };
                    let schema =
                        lookup(&stored).ok_or_else(|| not_found(format!("type {stored}")))?;
                    let spec = schema
                        .field(&field.id)
                        .ok_or_else(|| not_found(format!("field {}", field.id)))?;
                    match spec.kind {
                        MergeKind::Collection => {
                            Some(FieldValue::Items(inner.listed(at, &field.id, None)?.ids()))
                        }
                        // Not a field the index holds: read from the record.
                        MergeKind::Record => None,
                        _ => match view.field(&resource, &spec.id) {
                            Ok(value) => Some(value.unwrap_or_else(|| default_value(spec))),
                            // Never read from a corrupt index: the record says, until the index
                            // is built anew.
                            Err(_) if inner.chunks.has_record() => None,
                            Err(error) => return Err(backend(error)),
                        },
                    }
                }
            }
        };
        if let Some(value) = indexed {
            return Ok(value);
        }
        // The resource's map is in its own chunk: one the store was not sent is read here.
        self.load_at(at)?;
        let inner = self.held();
        let (app, ty) = inner.locate(at)?;
        inner.read_mirror((&app, &ty), at, &field.id)
    }

    fn listing(&self, at: &ResourceRef, field: &FieldSpec) -> Result<Rc<Listing>, StoreError> {
        self.held().listed(at, &field.id, None)
    }

    fn relatives(
        &self,
        at: &ResourceRef,
        field: &FieldId,
        parent: &FieldId,
        node: &ResourceRef,
        down: bool,
    ) -> Result<Option<Vec<(ItemId, TypeId)>>, StoreError> {
        self.held().relatives(at, field, parent, node, down)
    }

    fn listing_where(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: &Pred,
    ) -> Result<Rc<Listing>, StoreError> {
        self.refresh(item);
        self.held().listed(at, &field.id, Some((item, pred)))
    }

    fn listing_query(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        item: &TypeId,
        pred: Option<&Pred>,
        order: Option<&tilolang_runtime::query::Order>,
    ) -> Result<Rc<Listing>, StoreError> {
        self.refresh(item);
        self.held()
            .listed_in(at, &field.id, Some((item, pred)), order)
    }

    /// When the items the query compared with the clock next change what they answer: the value
    /// closest after it, which the view wakes at ([`crate::index::clock`]).
    fn next_clock_change(
        &self,
        ty: &TypeId,
        fields: &[tilolang_runtime::query::ClockField],
    ) -> Result<tilolang_runtime::query::NextChange, StoreError> {
        self.next_change(ty, fields)
    }

    fn select(
        &self,
        ty: &TypeId,
        pred: &Pred,
        order: Option<&tilolang_runtime::query::Order>,
    ) -> Result<Vec<ResourceRef>, StoreError> {
        self.refresh(ty);
        let inner = self.held();
        let schema = lookup(ty).ok_or_else(|| not_found(format!("type {ty}")))?;
        let Some(view) = inner.view(&schema.app) else {
            return Err(StoreError::Backend(format!(
                "the index of {} is being built",
                schema.app
            )));
        };
        let paths = view.select(schema, pred, order, &inner.evaluated)?;
        Ok(paths
            .into_iter()
            .filter_map(|path| {
                let held = view.type_of(&path).ok().flatten()?;
                ResourceRef::parse_id(&path, held)
            })
            .collect())
    }

    fn first(
        &self,
        ty: &TypeId,
        pred: &Pred,
        order: Option<&tilolang_runtime::query::Order>,
    ) -> Result<Option<ResourceRef>, StoreError> {
        self.refresh(ty);
        let inner = self.held();
        let schema = lookup(ty).ok_or_else(|| not_found(format!("type {ty}")))?;
        let Some(view) = inner.view(&schema.app) else {
            return Err(StoreError::Backend(format!(
                "the index of {} is being built",
                schema.app
            )));
        };
        let path = view.first(schema, pred, order, &inner.evaluated)?;
        Ok(path.and_then(|path| {
            let held = view.type_of(&path).ok().flatten()?;
            ResourceRef::parse_id(&path, held)
        }))
    }

    fn versions(&self, at: &ResourceRef) -> Result<Vec<Version>, StoreError> {
        Ok(self
            .built_versions()
            .get(&at.id_string())
            .cloned()
            .unwrap_or_default())
    }

    fn versions_within(&self, doc: &DocId) -> Result<BTreeMap<String, Vec<Version>>, StoreError> {
        let prefix = format!("{doc}/");
        Ok(self
            .built_versions()
            .iter()
            .filter(|(resource, _)| *resource == doc || resource.starts_with(&prefix))
            .map(|(resource, versions)| (resource.clone(), versions.clone()))
            .collect())
    }

    fn newest(&self, at: &ResourceRef) -> Result<Option<Version>, StoreError> {
        versions::newest(self, at)
    }

    fn newest_page(
        &self,
        doc: &DocId,
        page: usize,
    ) -> Result<(Vec<(String, Version)>, bool), StoreError> {
        versions::newest_page(self, doc, page)
    }

    fn removed(
        &self,
        holder: &ResourceRef,
        field: &str,
        item: &TypeId,
    ) -> Result<Vec<(String, Version)>, StoreError> {
        versions::removed_in(self, holder, field, item)
    }

    fn run_changes(&self, run: &str) -> Result<Vec<Version>, StoreError> {
        let mut out: Vec<Version> = self
            .built_versions()
            .values()
            .flatten()
            .filter(|v| v.task.as_ref().is_some_and(|task| task.id == run))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.order()
                .cmp(&b.order())
                .then_with(|| a.resource.cmp(&b.resource))
        });
        Ok(out)
    }

    /// The field `field` of `at` as it was before the change `change`, read in place at the
    /// change's dependencies: the cost of the value, not of the record. A field whose previous
    /// value is a whole rich text is not read this way: a change's two sides of one are the
    /// paragraphs it typed into ([`Self::written`]).
    fn read_before(
        &self,
        at: &ResourceRef,
        field: &FieldSpec,
        change: &str,
    ) -> Result<FieldValue, StoreError> {
        let inner = self.held();
        let (app, ty) = inner.locate(at)?;
        let mirror = inner.held_doc(&at.doc)?;
        // The change is the item's: it is in the chunk that holds the item's map.
        let located = locate_ref(mirror, Some(&inner.chunks), (&app, &ty), at, &inner.nested)?;
        let source = inner.chunks.mirror(located.chunk).unwrap_or(mirror);
        let hash: ChangeHash = change
            .parse()
            .map_err(|_| not_found(format!("change {change}")))?;
        let committed = source.committed_document();
        let made = committed
            .get_change_by_hash(&hash)
            .ok_or_else(|| not_found(format!("change {change}")))?;
        let spec = located
            .schema
            .field(&field.id)
            .ok_or_else(|| not_found(format!("field {}", field.id)))?;
        let Some(obj) = located.obj.as_ref() else {
            return Ok(default_value(spec));
        };
        let held = tilo_resource::history::held_at(committed, obj, &field.id, made.deps())
            .map_err(backend)?;
        field_of_json(&held, spec)
    }

    fn written(
        &self,
        at: &ResourceRef,
        fields: &[FieldSpec],
        change: &str,
    ) -> Result<Vec<(FieldId, FieldValue, FieldValue)>, StoreError> {
        self.written_by(at, fields, change)
    }

    fn version_end(&self, at: &ResourceRef, last: bool) -> Result<Option<Version>, StoreError> {
        let answered = self.held().version_end(at, last);
        if let Some(end) = answered {
            return end;
        }
        // No index answers yet: the log does.
        let mut versions = self.versions(at)?;
        Ok(if last {
            versions.pop()
        } else {
            versions.into_iter().next()
        })
    }

    fn field_changed(&self, at: &ResourceRef, field: &str) -> Result<Option<Version>, StoreError> {
        let answered = self.held().field_changed(at, field);
        if let Some(end) = answered {
            return end;
        }
        // No index answers yet: the log does.
        Ok(tilolang_runtime::store::last_change_of(
            self.versions(at)?,
            field,
        ))
    }

    fn history(&self, doc: &DocId) -> Result<Vec<ChangeNote>, StoreError> {
        let inner = self.held();
        if !inner.index.contains_key(doc) {
            return Err(not_found(format!("resource {doc}")));
        }
        Ok(inner.notes.get(doc).cloned().unwrap_or_default())
    }

    fn subscribe(&mut self, doc: &DocId, f: Box<dyn FnMut(&ChangeSummary)>) -> SubId {
        let mut inner = self.changing();
        let id = SubId(inner.next_sub);
        inner.next_sub = inner.next_sub.wrapping_add(1);
        inner.subs.push(Subscription {
            id,
            doc: doc.clone(),
            callback: f,
        });
        id
    }

    fn unsubscribe(&mut self, sub: SubId) {
        self.changing().subs.retain(|held| held.id != sub);
    }

    /// The last `window` notes of the document, newest first, read in place.
    fn recent_authors(
        &self,
        at: &ResourceRef,
        limit: usize,
        window: usize,
    ) -> Result<Vec<String>, StoreError> {
        let inner = self.held();
        if !inner.index.contains_key(&at.doc) {
            return Err(not_found(format!("resource {}", at.doc)));
        }
        Ok(inner.notes.get(&at.doc).map_or_else(Vec::new, |notes| {
            tilolang_runtime::store::recent_authors_in(notes.iter().rev().take(window), at, limit)
        }))
    }

    fn outbox_pending(&self) -> Vec<OutboxEntry> {
        let inner = self.held();
        // A store without the record has queued nothing yet; what the record holds is carried
        // out once it is had.
        if !inner.chunks.has_record() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (doc, (app, _)) in &inner.index {
            let Some(mirror) = inner.chunks.doc(doc) else {
                continue;
            };
            for held in outbox_entries(mirror, app, doc) {
                if held.settled.is_none() && held.owner == inner.device {
                    out.push(OutboxEntry {
                        doc: doc.clone(),
                        state: held.outbox_state(),
                        id: held.entry,
                        capability: held.capability,
                        payload: held.payload,
                        held_until: held.held_until,
                    });
                }
            }
        }
        out
    }

    fn outbox_next_held(&self) -> Option<String> {
        let inner = self.held();
        if !inner.chunks.has_record() {
            return None;
        }
        let mut earliest: Option<String> = None;
        for (doc, (app, _)) in &inner.index {
            let Some(mirror) = inner.chunks.doc(doc) else {
                continue;
            };
            for held in outbox_entries(mirror, app, doc) {
                if held.settled.is_none()
                    && let Some(at) = held.held_until
                    && earliest.as_ref().is_none_or(|soonest| at < *soonest)
                {
                    earliest = Some(at);
                }
            }
        }
        earliest
    }

    fn outbox_withdraw(
        &mut self,
        doc: &DocId,
        reason: &str,
        note: &ChangeNote,
    ) -> Result<usize, StoreError> {
        let pending: Vec<String> = {
            let inner = self.held();
            let Some((app, _)) = inner.index.get(doc) else {
                return Ok(0);
            };
            let Some(mirror) = inner.chunks.doc(doc) else {
                return Ok(0);
            };
            outbox_entries(mirror, app, doc)
                .into_iter()
                .filter(|held| held.settled.is_none())
                .map(|held| held.entry)
                .collect()
        };
        for id in &pending {
            let entry = OutboxEntry {
                doc: doc.clone(),
                id: id.clone(),
                capability: String::new(),
                payload: Json::Null,
                held_until: None,
                state: tilolang_runtime::store::OutboxState::Queued,
            };
            self.outbox_settle(
                &entry,
                &tilolang_runtime::store::Settled::Withdrawn(reason.to_owned()),
                note,
            )?;
        }
        Ok(pending.len())
    }

    fn outbox_mark(
        &mut self,
        entry: &OutboxEntry,
        state: &tilolang_runtime::store::OutboxState,
        note: &ChangeNote,
    ) -> Result<(), StoreError> {
        use tilolang_runtime::store::OutboxState;
        let (word, detail) = match state {
            OutboxState::Queued => ("", ""),
            OutboxState::Sending { by } => ("sending", by.as_str()),
            OutboxState::Waiting { reason } => ("waiting", reason.as_str()),
        };
        let edit = DynEdit::OutboxMark {
            id: entry.doc.clone(),
            entry: entry.id.clone(),
            state: word.to_owned(),
            detail: detail.to_owned(),
        };
        let seed = format!("{word}:{}:{}:{detail}", entry.doc, entry.id).into_bytes();
        self.write_outbox(entry, edit, &seed, note)
    }

    fn outbox_settle(
        &mut self,
        entry: &OutboxEntry,
        outcome: &tilolang_runtime::store::Settled,
        note: &ChangeNote,
    ) -> Result<(), StoreError> {
        let edit = DynEdit::OutboxSettle {
            id: entry.doc.clone(),
            entry: entry.id.clone(),
            outcome: outcome.word().to_owned(),
            reason: outcome.reason().to_owned(),
        };
        let seed = format!("settle:{}:{}", entry.doc, entry.id).into_bytes();
        self.write_outbox(entry, edit, &seed, note)
    }

    fn outbox_states(&self) -> Vec<(DocId, tilolang_runtime::store::DeliveryState)> {
        let inner = self.held();
        if !inner.chunks.has_record() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (doc, (app, _)) in &inner.index {
            let Some(mirror) = inner.chunks.doc(doc) else {
                continue;
            };
            let entries = outbox_entries(mirror, app, doc);
            // The one not settled when there is one, else the last.
            let last = entries
                .iter()
                .find(|held| held.settled.is_none())
                .or_else(|| entries.last());
            if let Some(held) = last {
                out.push((
                    doc.clone(),
                    tilolang_runtime::store::DeliveryState::of_entry(
                        &held.outbox_state(),
                        held.settled
                            .as_deref()
                            .map(|outcome| (outcome, held.reason.as_str())),
                        held.held_until.as_deref(),
                    ),
                ));
            }
        }
        out
    }

    fn save_journal(&mut self, run: &RunId, journal: &Journal) -> Result<(), StoreError> {
        self.changing()
            .journals
            .insert(run.clone(), journal.clone());
        Ok(())
    }
    fn load_journal(&mut self, run: &RunId) -> Result<Option<Journal>, StoreError> {
        Ok(self.held().journals.get(run).cloned())
    }
}
