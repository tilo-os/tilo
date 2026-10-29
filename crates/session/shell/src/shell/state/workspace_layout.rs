//! Each workspace's layout, as the record every device showing it keeps.
//!
//! The record ([`tilo_desktop_model::shared_record`]) is the room's shared
//! desktop, the one browsers and agents in the room keep too, and it travels
//! on the room's record route like any app's record: signed, end-to-end
//! encrypted where the room's records are, and retained by the room's hosts
//! for devices that join later. This device writes its layout into its copy
//! as it changes, offline too ([`WorkspaceTree::capture_workspace_layout`]),
//! and folds in what others wrote once it arrives
//! (`WorkspaceTree::fold_workspace_layout`). The copy is saved with the
//! tree, so a change and the tree it was made in reach the disk together.
//!
//! What only this device decides (widths, maximization, the active tab,
//! focus history, which display shows a group) stays out of the record; see
//! [`tilo_desktop_model::shared_record::device`].

use std::collections::{BTreeSet, HashMap};

use automerge::transaction::CommitOptions;
use automerge::{AutoCommit, ChangeHash};
use collab_types::DocumentMutation;
use tilo_desktop_model::shared_record::{self, guard};
use tilo_desktop_model::{ownership, projection};
use ui::app::record_routes::Admission;

use super::{RoomId, StripId, WorkspaceTree, peer_salt_of};

/// What one look at a room's layout found nothing to write for: writing is a
/// function of exactly these, so while they hold the next look writes
/// nothing either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captured {
    /// The record's heads.
    heads: Vec<ChangeHash>,
    /// The layout as other devices are to see it, and the room's scope.
    layout: blake3::Hash,
}

/// How this device decides a write of a room's layout that arrived from
/// another device: by what it does to what someone else made
/// ([`guard`]). A room the native owner holds was decided by its governor
/// (or its relay) already; one of this device's own engine is decided here,
/// as every member decides it.
pub(crate) struct LayoutJudge {
    room: String,
    decides: bool,
    /// The room's policy, once its governance is here.
    policy: Option<tilo_action_policy::RoomPolicy>,
    members: Option<BTreeSet<String>>,
    /// The person each device the room knows acts for.
    persons: HashMap<[u8; 32], String>,
    /// The person each device this device authenticated acts for, by the
    /// peer it is.
    authenticated: HashMap<super::PeerOrigin, [u8; 32]>,
}

impl LayoutJudge {
    /// Merge `mutation`, a write `author` signed, into `doc` when the room
    /// lets its author do what it does, once `doc` holds everything it
    /// builds on; leave `doc` as it was otherwise.
    ///
    /// # Errors
    /// Why the write is refused.
    pub(crate) fn admit(
        &self,
        doc: &mut AutoCommit,
        author: [u8; 32],
        mutation: &DocumentMutation,
    ) -> Result<Admission, String> {
        if !self.decides {
            return merge(doc, mutation).map(Admission::Merged);
        }
        let person = self
            .persons
            .get(&author)
            .cloned()
            .or_else(|| {
                let endpoint = iroh::EndpointId::from_bytes(&author).ok()?;
                self.authenticated
                    .get(&super::PeerOrigin::peer(&endpoint))
                    .map(hex::encode)
            })
            .unwrap_or_default();
        let bound = tilo_action_policy::roles::foreign_device_bound();
        let authority = self
            .policy
            .as_ref()
            .filter(|_| !person.is_empty())
            .map(|policy| policy.person_on_device(&person, false, &bound));
        let cx = tilo_action_policy::Context::at_time(std::time::SystemTime::now());
        let allows = |act: &ownership::Touched| {
            // A device whose person is not known arranges what nobody made,
            // as any device may, and nothing a person made.
            let Some(authority) = &authority else {
                return act.owner.is_none();
            };
            authority
                .decide(
                    &ownership::act_node(&self.room, act),
                    tilo_action_policy::ActionKind::Undoable,
                    &cx,
                )
                .answer
                == tilo_action_policy::Answer::Allow
        };
        match guard::admit_complete(
            doc,
            mutation,
            &guard::Writer {
                person: &person,
                members: self.members.as_ref(),
                allows: &allows,
            },
        ) {
            Ok(guard::Admitted::Merged(changed)) => Ok(Admission::Merged(changed)),
            Ok(guard::Admitted::Waiting) => Ok(Admission::Waiting),
            Err(refusal) => Err(refusal.to_string()),
        }
    }
}

/// Merge `mutation` into `doc`, whole or not at all.
fn merge(doc: &mut AutoCommit, mutation: &DocumentMutation) -> Result<bool, String> {
    let before = doc.get_heads();
    let bytes = match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    };
    let mut next = doc.clone();
    tilo_automerge::ApplyIncoming::load_incoming(&mut next, bytes)
        .map_err(|error| error.to_string())?;
    let changed = next.get_heads() != before;
    *doc = next;
    Ok(changed)
}

impl WorkspaceTree {
    /// The rooms whose workspaces this device shows and holds, each with a
    /// layout record: not a person's own room, and not one still being
    /// joined.
    pub(crate) fn workspace_layout_rooms(&self) -> Vec<RoomId> {
        self.desktop
            .workspaces
            .iter()
            .map(super::Workspace::room)
            .filter(|room| {
                !room.is_nil() && !super::is_local_person_room(*room) && self.holds_room(*room)
            })
            .collect()
    }

    /// Write this device's layout of `room` into the room's layout record,
    /// changing only what differs from what the record holds. In a room
    /// whose layout is personal, take this device's windows out of it
    /// instead. Returns whether it wrote; cheap when nothing changed since
    /// the last look.
    pub fn capture_workspace_layout(&mut self, room: RoomId) -> bool {
        let local_salt = self.local_salt();
        if local_salt == 0 {
            return false;
        }
        let Some(workspace) = self.workspace_in_room(room) else {
            return false;
        };
        let personal = self.room_layout(room).is_personal();
        let mut published = workspace.clone();
        if let Some(hosting) = &self.window_hosting {
            published.strips = hosting.published(&workspace.strips, &self.package_launches);
        }
        let layout = serde_json::to_vec(&(&published.strips, &published.arrangement, personal))
            .map_or_else(|_| blake3::hash(&[]), |bytes| blake3::hash(&bytes));
        let doc = self.layout_docs.entry(room).or_default();
        let looked_at = Captured {
            heads: doc.get_heads(),
            layout,
        };
        if self.layout_captured.get(&room) == Some(&looked_at) {
            return false;
        }
        // A joined room's workspace is made for the room when it is joined,
        // so every window of this device's in it is the room's.
        let mine = |id: u64| peer_salt_of(id) == local_salt;
        let person = self.local_person.map(hex::encode);
        let former = self
            .room_docs
            .get(&room)
            .map(shared_record::device::former_ids)
            .unwrap_or_default();
        let strips_to_retain = &self.strips_to_retain;
        let retained = |id: u64| strips_to_retain.contains(&StripId(id));
        // In a room whose layout is personal every member takes its own
        // windows out; a device governs no room (its host does), so none
        // takes out anyone else's.
        let doc = self.layout_docs.entry(room).or_default();
        let written = if personal {
            shared_record::device::withdraw(doc, &mine)
        } else {
            shared_record::device::write(
                doc,
                room,
                &published,
                &shared_record::device::Writer {
                    person: person.as_deref(),
                    mine: &mine,
                    retained: &retained,
                    carried: &|id| former.contains(&id),
                },
            )
        };
        let wrote = match written {
            Ok(wrote) => wrote,
            Err(error) => {
                tracing::error!(%room, %error, "the workspace's layout was not written");
                doc.rollback();
                return false;
            }
        };
        if wrote {
            doc.commit_with(
                CommitOptions::default()
                    .with_message(shared_record::change_message(person.as_deref())),
            );
        }
        if !personal {
            // In the record now: a strip kept while empty no longer waits.
            for strip in &published.strips {
                self.strips_to_retain.remove(&strip.id);
            }
        }
        let heads = self.layout_docs.entry(room).or_default().get_heads();
        self.layout_captured
            .insert(room, Captured { heads, layout });
        wrote
    }

    /// Fold `room`'s layout record into the workspace that addresses it: its
    /// strips, groups and windows where the record says, with what this
    /// device decides alone kept ([`shared_record::device::fold`]). Nothing
    /// in a room whose layout is personal: there each member's layout is
    /// their own. Reports whether anything changed.
    pub(crate) fn fold_workspace_layout(&mut self, room: RoomId) -> bool {
        let local_salt = self.local_salt();
        if local_salt == 0 || self.room_layout(room).is_personal() {
            return false;
        }
        let Some(doc) = self.layout_docs.get(&room) else {
            return false;
        };
        let device = self
            .window_hosting
            .as_ref()
            .map(|hosting| hosting.device.clone())
            .unwrap_or_default();
        let mine = |id: u64| peer_salt_of(id) == local_salt;
        // A joined room's workspace is made for the room when it is joined:
        // no window this device had open before is in it.
        let private = |_: u64| false;
        let desktop = &mut self.desktop;
        let Some(ws) = desktop.workspaces.iter_mut().find(|ws| ws.room() == room) else {
            return false;
        };
        let held: Vec<StripId> = ws.strips.iter().map(|strip| strip.id).collect();
        let changed = shared_record::device::fold(
            doc,
            ws,
            &device,
            &shared_record::device::Kept {
                mine: &mine,
                private: &private,
            },
        );
        // Removed rather than repointed: resolvers already fall back to the
        // first strip or group with no explicit selection.
        let ws_id = ws.id;
        let live: Vec<StripId> = ws.strips.iter().map(|strip| strip.id).collect();
        if desktop
            .active_strip
            .get(&ws_id)
            .is_some_and(|active| !live.contains(active))
        {
            desktop.active_strip.remove(&ws_id);
        }
        for strip in held.iter().filter(|strip| !live.contains(strip)) {
            desktop.active_group.remove(strip);
        }
        if changed {
            // A window that arrived is in the tree now but not on screen
            // yet: only the frame thread splices the widget tree, on chrome
            // revision bumps.
            self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
            self.seed_doc_from_cache();
        }
        changed
    }

    /// Note in `room`'s layout record how each window this device runs for
    /// its person resumes, and which window this device is on
    /// ([`shared_record::continuation`]): what the person's other devices
    /// read to continue here. A window's note changes only when this device's
    /// own state of it changed since it last noted it, so two devices running
    /// one window do not take turns rewriting it. With app continuations off
    /// (no [`Self::continuation_device`]), this device's notes are taken out.
    /// `file_reference` names the record a surface shows, from the session's
    /// open documents, which live above this layer. Returns whether it wrote.
    pub(crate) fn capture_continuations(
        &mut self,
        room: RoomId,
        file_reference: impl Fn(u64) -> Option<crate::shell::continuation::LaunchIntent>,
    ) -> bool {
        use crate::shell::continuation::AppInstanceState;
        use shared_record::continuation::{self as notes, Focus};

        let salt = self.local_salt();
        let (Some(person), true) = (self.local_person.map(hex::encode), salt != 0) else {
            return false;
        };
        if !self.layout_docs.contains_key(&room) || self.room_layout(room).is_personal() {
            return false;
        }
        let device = self.continuation_device.clone();
        let doc = self.layout_docs.entry(room).or_default();
        let held: HashMap<u64, AppInstanceState> = notes::windows(doc)
            .into_iter()
            .filter_map(|(tab, text)| Some((tab, AppInstanceState::from_record(tab, &text)?)))
            .collect();
        let shared = shared_record::read(&*doc, None).unwrap_or_default();
        let in_record = |tab: u64| {
            shared
                .tree
                .workspaces
                .iter()
                .flat_map(|ws| &ws.strips)
                .flat_map(|strip| &strip.window_groups)
                .any(|group| group.tabs.iter().any(|t| t.id == tab))
        };
        let mut writes: Vec<(u64, Option<String>)> = Vec::new();
        let mut written: Vec<(u64, AppInstanceState)> = Vec::new();
        let mut acknowledged = Vec::new();
        let mut focus = None;
        let mut claims = 0;
        if let Some(device) = device.as_ref() {
            let Some(running) =
                self.note_running_windows(room, &person, device, &held, &shared, &file_reference)
            else {
                return false;
            };
            claims = running.claims;
            writes = running.writes;
            written = running.written;
            acknowledged = running.acknowledged;
            focus = self
                .focused_window(room)
                .filter(|tab| in_record(*tab))
                .map(|window| Focus {
                    window,
                    author: person.clone(),
                });
        } else {
            let own = |state: &AppInstanceState| {
                shared.authors.get(&state.id.0) == Some(&person)
                    && device_of_this_tree(&self.continuation_written, room, state)
            };
            for (tab, _) in held.iter().filter(|(_, state)| own(state)) {
                writes.push((*tab, None));
            }
        }
        let doc = self.layout_docs.entry(room).or_default();
        let mut wrote = false;
        let mut failed = None;
        for (tab, text) in &writes {
            match notes::write_window(doc, *tab, text.as_deref()) {
                Ok(changed) => wrote |= changed,
                Err(error) => failed = Some(error),
            }
        }
        match notes::write_focus(doc, salt, focus.as_ref()) {
            Ok(changed) => wrote |= changed,
            Err(error) => failed = Some(error),
        }
        if let Some(error) = failed {
            tracing::error!(%room, %error, "the workspace's continuations were not written");
            doc.rollback();
            return false;
        }
        if wrote {
            doc.commit_with(
                CommitOptions::default().with_message(shared_record::change_message(Some(&person))),
            );
        }
        self.continuation_written
            .retain(|(held, _), _| *held != room);
        if device.is_some() {
            self.continuation_written
                .extend(written.into_iter().map(|(tab, state)| ((room, tab), state)));
        }
        if !acknowledged.is_empty()
            && let Ok(mut registry) = crate::shell::continuation_registry::global_registry().lock()
        {
            registry.acknowledge_owner_claims(claims, room, &acknowledged);
        }
        wrote
    }

    /// How each window this device runs for `person` in `room` resumes, as
    /// notes to write: a window whose state is unchanged since this device
    /// last noted it, or that adds nothing to another device's note, is left
    /// as it is. `None` while the continuation registry is unusable.
    fn note_running_windows(
        &self,
        room: RoomId,
        person: &str,
        device: &tilo_network_actor::fabric::DeviceId,
        held: &HashMap<u64, crate::shell::continuation::AppInstanceState>,
        shared: &ownership::SharedDesktop,
        file_reference: &impl Fn(u64) -> Option<crate::shell::continuation::LaunchIntent>,
    ) -> Option<RunningNotes> {
        use crate::shell::continuation::{AppInstanceId, AppInstanceState, ContinuityStamp};

        let registry = crate::shell::continuation_registry::global_registry();
        let registry = registry.lock().ok()?;
        let mut running = RunningNotes {
            claims: registry.owner_claim_revision(),
            ..RunningNotes::default()
        };
        let mut counter = held
            .values()
            .map(|state| state.stamp.counter)
            .max()
            .unwrap_or(0);
        let tabs = self
            .workspace_in_room(room)
            .into_iter()
            .flat_map(|ws| &ws.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs);
        for tab in tabs {
            if shared.authors.get(&tab.id).map(String::as_str) != Some(person) {
                continue;
            }
            let Some(launch) = crate::shell::continuation::launch_for_tab(tab, file_reference)
            else {
                continue;
            };
            let id = AppInstanceId(tab.id);
            let mut state = AppInstanceState::new(
                id,
                launch.clone(),
                registry.resume_for_tab(room, id, &launch),
                ContinuityStamp {
                    counter: 0,
                    device: device.clone(),
                },
            );
            state.record_instance = registry.record_instance_for_tab(room, id, &launch);
            let claimed = registry.owner_claimed(room, &state);
            let noted = self.continuation_written.get(&(room, tab.id));
            let held_here = held.get(&tab.id);
            if !claimed {
                if let Some(noted) = noted.filter(|noted| noted.same_as(&state)) {
                    running.written.push((tab.id, noted.clone()));
                    continue;
                }
                // Nothing to add to what another device noted: a window
                // opened here has not reported its state yet, and the
                // record it shows is the one read from that note.
                if let Some(held_here) = held_here
                    && held_here.launch == state.launch
                    && state.resume.is_none()
                    && state
                        .record_instance
                        .is_none_or(|instance| held_here.record_instance.unwrap_or(id) == instance)
                {
                    continue;
                }
            }
            counter = counter.saturating_add(1);
            state.stamp.counter = counter;
            running.writes.push((tab.id, state.to_record()));
            if claimed {
                running.acknowledged.push(state.clone());
            }
            running.written.push((tab.id, state));
        }
        drop(registry);
        Some(running)
    }

    /// The window this device is on in `room`, if its active workspace is
    /// that room's.
    fn focused_window(&self, room: RoomId) -> Option<u64> {
        let desktop = &self.desktop;
        desktop
            .active_workspace()
            .filter(|ws| ws.room() == room)
            .and_then(|ws| {
                let strip = desktop.active_strip.get(&ws.id)?;
                let group = desktop.active_group.get(strip)?;
                ws.strips
                    .iter()
                    .find(|s| s.id == *strip)?
                    .window_groups
                    .iter()
                    .find(|g| g.id == *group)
                    .map(|g| g.active_tab)
            })
    }

    /// Read what this person's other devices noted in `room`'s layout record
    /// about resuming its windows: an app's resume state waits for the window
    /// to open here, and a document card opens the document it names. Nothing
    /// while app continuations are off.
    pub(crate) fn apply_continuations(&mut self, room: RoomId) {
        use crate::shell::continuation::{AppInstanceId, AppInstanceState, LaunchIntent};

        let tabs_here: BTreeSet<u64> = self
            .workspace_in_room(room)
            .into_iter()
            .flat_map(|ws| &ws.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect();
        self.continuation_file_references
            .retain(|tab, _| !tabs_here.contains(tab));
        let mut offered = BTreeSet::new();
        let registry = crate::shell::continuation_registry::global_registry();
        let Ok(mut registry) = registry.lock() else {
            return;
        };
        if let (Some(device), Some(person), Some(doc)) = (
            self.continuation_device.as_ref(),
            self.local_person.map(hex::encode),
            self.layout_docs.get(&room),
        ) {
            let authors = shared_record::read(doc, None)
                .map(|shared| shared.authors)
                .unwrap_or_default();
            for (tab, text) in shared_record::continuation::windows(doc) {
                if authors.get(&tab) != Some(&person) || !tabs_here.contains(&tab) {
                    continue;
                }
                let Some(state) = AppInstanceState::from_record(tab, &text) else {
                    continue;
                };
                if &state.stamp.device == device {
                    continue;
                }
                let record_instance = state.record_instance.unwrap_or(AppInstanceId(tab));
                match state.launch {
                    LaunchIntent::App { app_id } => {
                        match state.resume {
                            Some(resume) => registry.store_remote_resume(
                                room,
                                AppInstanceId(tab),
                                app_id,
                                resume,
                                record_instance,
                            ),
                            None => registry.store_remote_record(
                                room,
                                AppInstanceId(tab),
                                app_id,
                                record_instance,
                            ),
                        }
                        offered.insert(AppInstanceId(tab));
                    }
                    reference @ LaunchIntent::FileReference { .. } => {
                        self.continuation_file_references.insert(tab, reference);
                    }
                }
            }
        }
        registry.retain_remote_in_room(room, &offered);
    }

    /// How this device decides writes of `room`'s layout that other devices
    /// made ([`LayoutJudge`]).
    pub(crate) fn layout_judge(&self, room: RoomId) -> LayoutJudge {
        let room_doc = self.room_docs.get(&room);
        // Every room is held by the native owner and governed by its host,
        // whose doors decide each write of its layout record
        // (`tilo_room_host::rooms::desktop_writes`) before any device
        // receives it: what arrives was decided already.
        LayoutJudge {
            room: room.to_string(),
            decides: false,
            policy: room_doc.map(|doc| projection::room_policy_of(doc, &room.to_string())),
            members: room_doc.and_then(projection::room_members),
            persons: HashMap::new(),
            authenticated: HashMap::new(),
        }
    }

    /// Take in what `from` holds of `room`'s layout record, signed by
    /// `author`, as its record route would deliver it: this device writes
    /// its own layout first, decides the write as the room does, and folds
    /// in the result.
    ///
    /// # Errors
    /// Why the room refuses the write.
    #[cfg(any(test, feature = "hosted-test-control"))]
    pub fn hear_layout(
        &mut self,
        from: &mut Self,
        room: RoomId,
        author: [u8; 32],
    ) -> Result<(), String> {
        from.capture_workspace_layout(room);
        let sent = DocumentMutation::Snapshot(from.layout_docs.entry(room).or_default().save());
        self.receive_layout(room, author, &sent)
    }

    /// Take in `mutation`, a write of `room`'s layout record `author`
    /// signed, as its record route would deliver it ([`Self::hear_layout`]).
    ///
    /// # Errors
    /// Why the room refuses the write.
    #[cfg(any(test, feature = "hosted-test-control"))]
    pub fn receive_layout(
        &mut self,
        room: RoomId,
        author: [u8; 32],
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        self.capture_workspace_layout(room);
        let judge = self.layout_judge(room);
        let mut doc = self.layout_docs.remove(&room).unwrap_or_default();
        let admitted = judge.admit(&mut doc, author, mutation);
        self.layout_docs.insert(room, doc);
        if admitted? == Admission::Waiting {
            return Err("it waits for the changes it builds on".into());
        }
        self.fold_workspace_layout(room);
        Ok(())
    }

    /// The record `room`'s layout names for `tab`, if any, as a route: its
    /// tab's binding, else what the window shared as it shows.
    pub(crate) fn room_tab_record(&self, room: RoomId, tab: super::TabId) -> Option<String> {
        let bound = self.layout_docs.get(&room);
        (bound.and_then(|doc| tilo_desktop_model::apps::record_routes(doc).remove(&tab)))
            .or_else(|| self.shared_window_record(room, tab))
    }
}

/// The notes [`WorkspaceTree::capture_continuations`] writes for the windows
/// this device runs: the registry's claim revision they answer, the record
/// writes, what this device then holds as noted, and the claims they settle.
#[derive(Default)]
struct RunningNotes {
    claims: u64,
    writes: Vec<(u64, Option<String>)>,
    written: Vec<(u64, crate::shell::continuation::AppInstanceState)>,
    acknowledged: Vec<crate::shell::continuation::AppInstanceState>,
}

/// Whether `state` is a note this device wrote: one it still remembers
/// writing for its window in `room`.
fn device_of_this_tree(
    written: &HashMap<(RoomId, u64), crate::shell::continuation::AppInstanceState>,
    room: RoomId,
    state: &crate::shell::continuation::AppInstanceState,
) -> bool {
    written
        .get(&(room, state.id.0))
        .is_some_and(|noted| noted.stamp.device == state.stamp.device)
}

#[cfg(test)]
#[path = "workspace_layout_tests.rs"]
mod tests;
