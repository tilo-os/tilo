use std::collections::HashMap;
use crate::permissions::UserId;

mod key;
pub use key::{PresenceKey, PathSegment, SelectionRange};

#[derive(Debug, Clone)]
pub struct RemoteCursor {
    pub user_id: String,
    pub color: [f32; 4],
    pub position: [f32; 2], // scene-space coords
    pub label: String,
}

#[derive(Debug, Default, Clone)]
pub struct PresenceCtx {
    pub here: HashMap<PresenceKey, Vec<UserId>>,
    pub pointers: HashMap<PresenceKey, Vec<(UserId, (f32, f32))>>,
    pub selections: HashMap<PresenceKey, Vec<(UserId, SelectionRange)>>,
    pub carets: HashMap<PresenceKey, Vec<(UserId, usize)>>,
    pub remote_cursors: Vec<RemoteCursor>,
}

impl PresenceCtx {
    pub fn empty() -> Self { Self::default() }

    pub fn register_presence(&mut self, key: &PresenceKey, user: UserId) {
        let list = self.here.entry(key.clone()).or_default();
        if !list.iter().any(|u| u == &user) {
            list.push(user);
        }
    }

    pub fn remove_presence(&mut self, key: &PresenceKey, user: &UserId) {
        if let Some(list) = self.here.get_mut(key) {
            list.retain(|u| u != user);
        }
    }

    pub fn set_pointer(&mut self, key: &PresenceKey, user: UserId, pos: (f32, f32)) {
        let list = self.pointers.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user, pos));
        }
    }

    pub fn set_caret(&mut self, key: &PresenceKey, user: UserId, pos: usize) {
        let list = self.carets.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = pos;
        } else {
            list.push((user, pos));
        }
    }

    pub fn set_selection(&mut self, key: &PresenceKey, user: UserId, range: SelectionRange) {
        let list = self.selections.entry(key.clone()).or_default();
        if let Some(entry) = list.iter_mut().find(|(u, _)| *u == user) {
            entry.1 = range;
        } else {
            list.push((user, range));
        }
    }

    pub fn peers_on(&self, key: &PresenceKey) -> &[UserId] {
        self.here.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn pointer_for(&self, key: &PresenceKey, user: &UserId) -> Option<(f32, f32)> {
        self.pointers.get(key)?.iter().find(|(u, _)| u == user).map(|(_, p)| *p)
    }

    pub fn selection_for(&self, key: &PresenceKey, user: &UserId) -> Option<SelectionRange> {
        self.selections.get(key)?.iter().find(|(u, _)| u == user).map(|(_, r)| *r)
    }

    pub fn caret_for(&self, key: &PresenceKey, user: &UserId) -> Option<usize> {
        self.carets.get(key)?.iter().find(|(u, _)| u == user).map(|(_, c)| *c)
    }
}
