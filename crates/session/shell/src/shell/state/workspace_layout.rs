//! Each workspace's layout, as the record every device showing it keeps.
//!
//! The record ([`tilo_desktop_model::shared_record`]) is the room's shared
//! desktop, the one browsers and agents in the room keep too, and it travels
//! on the room's record route like any app's record: signed, end-to-end
//! encrypted where the room's records are, and retained by the room's hosts
//! for devices that join later. This device writes its layout into its copy
//! as it changes, offline too ([`WorkspaceTree::capture_workspace_layout`]),
//! and folds in what others wrote once it arrives
//! ([`WorkspaceTree::fold_workspace_layout`]). The copy is saved with the
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

use super::{RoomId, StripId, WorkspaceTree, counter_of, peer_salt_of};

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
        let layout = serde_json::to_vec(&(&published.strips, personal))
            .map_or_else(|_| blake3::hash(&[]), |bytes| blake3::hash(&bytes));
        let doc = self.layout_docs.entry(room).or_default();
        let looked_at = Captured {
            heads: doc.get_heads(),
            layout,
        };
        if self.layout_captured.get(&room) == Some(&looked_at) {
            return false;
        }
        let share_floor = self.room_share_floor.get(&room).copied();
        let mine = |id: u64| {
            peer_salt_of(id) == local_salt
                && share_floor.is_none_or(|floor| counter_of(id) >= floor)
        };
        let person = self.local_person.map(hex::encode);
        let former = self
            .room_docs
            .get(&room)
            .map(shared_record::device::former_ids)
            .unwrap_or_default();
        let strips_to_retain = &self.strips_to_retain;
        let retained = |id: u64| strips_to_retain.contains(&StripId(id));
        // In a room whose layout is personal every member takes its own
        // windows out; its governor takes out everyone's, so a room shared
        // again starts from what each member shows then.
        let governs = self.governs_room(room) || self.native_authority.governed_here(room);
        let doc = self.layout_docs.entry(room).or_default();
        let written = if personal && governs {
            shared_record::device::withdraw(doc, &|_| true)
        } else if personal {
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
        let share_floor = self.room_share_floor.get(&room).copied();
        let mine = |id: u64| peer_salt_of(id) == local_salt;
        // Windows this device had open before it joined stay its own.
        let private = |id: u64| mine(id) && share_floor.is_some_and(|floor| counter_of(id) < floor);
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
        let opened = self.requires_native_owner(room) && self.open_governed_placeholders_here(room);
        if changed || opened {
            // A window that arrived is in the tree now but not on screen
            // yet: only the frame thread splices the widget tree, on chrome
            // revision bumps.
            self.desktop.room_epoch = self.desktop.room_epoch.wrapping_add(1);
            self.seed_doc_from_cache();
        }
        changed || opened
    }

    /// How this device decides writes of `room`'s layout that other devices
    /// made ([`LayoutJudge`]).
    pub(crate) fn layout_judge(&self, room: RoomId) -> LayoutJudge {
        let room_doc = self.room_docs.get(&room);
        let mut persons: HashMap<[u8; 32], String> = room_doc
            .map(|_| self.room_peer_identities_in_doc(room))
            .unwrap_or_default()
            .into_values()
            .map(|identity| {
                (
                    identity.device_cert.device_public_key,
                    hex::encode(identity.device_cert.user_public_key),
                )
            })
            .collect();
        if let (Some(device), Some(person)) = (
            self.window_hosting
                .as_ref()
                .and_then(|hosting| hex::decode(&hosting.device).ok())
                .and_then(|key| <[u8; 32]>::try_from(key).ok()),
            self.local_person,
        ) {
            persons.insert(device, hex::encode(person));
        }
        LayoutJudge {
            room: room.to_string(),
            decides: !self.requires_native_owner(room),
            policy: room_doc.map(|doc| projection::room_policy_of(doc, &room.to_string())),
            members: room_doc.and_then(projection::room_members),
            persons,
            authenticated: self.authenticated_persons.clone(),
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

    /// The record `room`'s layout names for `tab`, if any.
    pub(crate) fn room_tab_record(&self, room: RoomId, tab: super::TabId) -> Option<String> {
        tilo_desktop_model::apps::record_routes(self.layout_docs.get(&room)?).remove(&tab)
    }
}

#[cfg(test)]
#[path = "workspace_layout_tests.rs"]
mod tests;
