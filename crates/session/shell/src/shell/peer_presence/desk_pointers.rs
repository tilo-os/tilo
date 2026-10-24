//! Where peer pointers land on this desktop: a tab-anchored pointer over
//! its tab's tile, and a live member's pointer over the empty desktop
//! through the same [`PointerLayout`] a browser tab places it with.

use std::collections::HashMap;
use std::time::Instant;

use tilo_lock::MutexRecover as _;

use tilo_relay_protocol::live::{LivePointer, LiveRecord, PointerLayout, ShownStrip};
use ui::collaboration::PresencePayload;
use ui::num::f32_of_f64;
use ui::presence::{PresenceCtx, PresenceKey};

use super::{
    AnchoredPointers, POINTER_STAYS, PeerLabels, PointerAnchor, TabTiles, WindowRecord,
    place_pointer,
};

/// A peer pointer as it was heard: anchored to a room's tab (a desktop's,
/// over the room's presence channel), or as a live member said it over the
/// empty desktop, beside a window or on a strip.
#[derive(Clone, Debug, PartialEq)]
pub enum HeardPointer {
    Tab(PointerAnchor),
    Live(LivePointer),
}

/// What this desktop shows of `room` that a live pointer can be over: the
/// shown windows of `room` by the records they show (named as the live
/// plane names them), and `strip`.
/// The same [`PointerLayout`] a browser tab builds, so a pointer lands
/// beside the same window on both.
pub fn desk_layout<S: std::hash::BuildHasher>(
    room: &str,
    tiles: &TabTiles,
    records: &HashMap<u64, Vec<WindowRecord>, S>,
    strip: Option<ShownStrip>,
) -> PointerLayout {
    let windows = tiles
        .iter()
        .flat_map(|(tab, rect)| {
            let rect = [
                f32_of_f64(rect.x0),
                f32_of_f64(rect.y0),
                f32_of_f64(rect.x1),
                f32_of_f64(rect.y1),
            ];
            records
                .get(tab)
                .into_iter()
                .flatten()
                .filter(|shown| shown.room == room)
                .map(move |shown| {
                    let record = LiveRecord {
                        app: shown.app.clone(),
                        record: crate::network::wire_record::wire_record_id(
                            room,
                            &shown.app,
                            &shown.record,
                        ),
                    };
                    (record, rect)
                })
        })
        .collect();
    PointerLayout { windows, strip }
}

/// Fold live members' pointers over the empty desktop into `anchored`,
/// naming each sender in `presence`. Returns the ids of those that arrived,
/// and whether any went: one that moved over a window (that window's app
/// draws it there) or away leaves `anchored` and `presence`. A pointer
/// anchored to a tab stays, being the room's presence channel's to retire.
pub fn fold_live_desk_pointers(
    entries: Vec<crate::shell::live_presence::LiveDeskPointer>,
    anchored: &mut AnchoredPointers,
    presence: &mut PresenceCtx,
    labels: &dyn Fn(&str) -> PeerLabels,
    member_of: &dyn Fn(&str) -> bool,
    now: Instant,
) -> (Vec<String>, bool) {
    let mut arrived = Vec::new();
    let mut cleared = false;
    for entry in entries {
        if !member_of(&entry.room) {
            continue;
        }
        let user = super::presence_id(entry.peer);
        let Some(pointer) = entry.pointer else {
            if anchored.get(&user).is_some_and(|(room, heard, _)| {
                *room == entry.room && matches!(heard, HeardPointer::Live(_))
            }) {
                anchored.remove(&user);
                arrived.retain(|fresh| *fresh != user);
                cleared |= presence.clear_pointer(&user);
            }
            continue;
        };
        if let Some(name) = labels(&entry.room).get(&entry.peer) {
            presence.set_display_name(ui::permissions::UserId(user.clone()), name.clone());
        }
        anchored.insert(user.clone(), (entry.room, HeardPointer::Live(pointer), now));
        if !arrived.contains(&user) {
            arrived.push(user);
        }
    }
    (arrived, cleared)
}

/// Put every pointer in `anchored` where its tab's tile is on this device
/// (`tiles`), filed under its room the way the overlay paints them, and
/// take off screen any whose tab this device doesn't show. `fresh` peers
/// are restated even when they haven't moved, so a keepalive keeps them
/// current. Each is stamped with when it was last *heard*, not placed, so
/// a pointer riding a scrolling tile still goes quiet on time: one not
/// heard for [`ui::presence::POINTER_IDLE`] is retired
/// ([`PresenceCtx::prune_pointers`]) and not placed again until it is.
/// The same stamp is what tells the presence store a tile carrying the
/// pointer from the peer moving it: only a newly heard position glides
/// ([`PresenceCtx::pointer_position_at`]).
/// Returns whether anything landed, moved, went, or is fading or gliding.
pub fn place_pointers(
    anchored: &AnchoredPointers,
    fresh: &[String],
    presence: &mut PresenceCtx,
    tiles: &TabTiles,
    now: Instant,
) -> bool {
    place_heard_pointers(
        anchored,
        fresh,
        presence,
        tiles,
        &|_| PointerLayout::default(),
        now,
    )
}

/// [`place_pointers`], placing a live member's pointer over the empty
/// desktop in `layout` of its room ([`desk_layout`]): beside the window of
/// the record it was beside, or on the strip it was on, and hidden while
/// this desktop shows neither.
pub fn place_heard_pointers(
    anchored: &AnchoredPointers,
    fresh: &[String],
    presence: &mut PresenceCtx,
    tiles: &TabTiles,
    layout: &dyn Fn(&str) -> PointerLayout,
    now: Instant,
) -> bool {
    use ui::presence::POINTER_FADE;
    let mut changed = false;
    let mut layouts: HashMap<&str, PointerLayout> = HashMap::new();
    for (user, (room, anchor, heard)) in anchored {
        let placed = match anchor {
            HeardPointer::Tab(anchor) => place_pointer(tiles, *anchor),
            HeardPointer::Live(pointer) => layouts
                .entry(room.as_str())
                .or_insert_with(|| layout(room))
                .place(pointer)
                .map(|[x, y]| (f64::from(x), f64::from(y))),
        };
        let Some((x, y)) = placed else {
            changed |= presence.clear_pointer(user);
            continue;
        };
        let key = PresenceKey::Workspace { id: room.clone() };
        let at = (f32_of_f64(x), f32_of_f64(y));
        let placed = presence
            .pointers
            .get(&key)
            .and_then(|list| list.iter().find(|(u, _)| u.0 == *user))
            .map(|(_, at)| *at);
        // Restated where it already is (a late joiner's welcome, or an older
        // build's keepalive): heard again, so it does not fade, but nothing
        // on screen moves unless it had started fading.
        let restated = placed == Some(at);
        if restated && !fresh.contains(user) {
            continue;
        }
        let fading = restated
            && presence.pointer_opacity(
                &key,
                &ui::permissions::UserId(user.clone()),
                now,
                POINTER_STAYS,
                POINTER_FADE,
            ) < 1.0;
        // Stamped with when it was heard rather than now: moving with a
        // tile is not the peer saying anything.
        presence.apply_remote_presence_at(
            user.clone(),
            key,
            PresencePayload::Pointer { x: at.0, y: at.1 },
            *heard,
        );
        changed |= !restated || fading;
    }
    changed |= presence.prune_pointers(now, POINTER_STAYS);
    changed
        || presence.pointers_fading(now, POINTER_STAYS, POINTER_FADE)
        || presence.pointers_easing(now)
        || presence.pointer_names_fading(now, POINTER_FADE)
}

impl super::PeerPresence {
    /// The records each shown window (`tiles`) shows: those its app holds,
    /// found through the window instance the launcher started it for, and
    /// the one it last said it points in.
    pub(super) fn shown_records(&self, tiles: &TabTiles) -> HashMap<u64, Vec<WindowRecord>> {
        let held = (self.window_instances)();
        let mut shown: HashMap<u64, Vec<WindowRecord>> = HashMap::new();
        if !held.is_empty() {
            let tree = self.roster_tree.read();
            for (tab, _) in tiles {
                let Some((room, app, instance)) = tree.local_app_window(*tab) else {
                    continue;
                };
                let room = room.to_string();
                let records = held
                    .iter()
                    .filter(|(at, route)| {
                        *at == instance && route.app_id == app && route.cluster_id == room
                    })
                    .map(|(_, route)| WindowRecord {
                        room: room.clone(),
                        app: route.app_id.clone(),
                        record: route.record_id.clone(),
                    });
                shown.entry(*tab).or_default().extend(records);
            }
            drop(tree);
        }
        for (tab, record) in self.window_records.lock_recover().iter() {
            let records = shown.entry(*tab).or_default();
            if !records.contains(record) {
                records.push(record.clone());
            }
        }
        shown
    }
}
