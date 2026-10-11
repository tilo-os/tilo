//! Device-private selections of existing workspace resources. No URL fetching,
//! provider enrollment, permission grant or inferred source selection lives here.
use serde::{Deserialize, Serialize};

pub const MORNING_SOURCE: &str = "room.activity.morning.toggle";
pub const MORNING_OPEN: &str = "room.activity.morning.open.";
pub const MORNING_REMOVE: &str = "room.activity.morning.remove.";
pub const MAX_SOURCES: usize = 8;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MorningPreferences {
    pub room: String,
    pub sources: Vec<MorningSource>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MorningSource {
    pub app: String,
    pub resource: String,
}
impl MorningSource {
    pub fn key(&self) -> String {
        use std::fmt::Write as _;
        let mut key = String::new();
        for byte in self.app.bytes().chain([0]).chain(self.resource.bytes()) {
            let _ = write!(key, "{byte:02x}");
        }
        key
    }
    pub fn matches(&self, target: &ui::HistoryResourceTarget) -> bool {
        target.app_id.as_deref() == Some(self.app.as_str()) && target.id == self.resource
    }
    pub fn from_target(target: &ui::HistoryResourceTarget) -> Option<Self> {
        let app = target.app_id.as_ref()?;
        (!app.is_empty()
            && !target.id.is_empty()
            && app.len() <= 256
            && target.id.len() <= 2048
            && !app.contains('\0')
            && !target.id.contains('\0'))
        .then(|| Self {
            app: app.clone(),
            resource: target.id.clone(),
        })
    }
}
impl MorningPreferences {
    /// Empty by default; a recycled workspace id never reuses another room's choices.
    pub fn for_room(&self, room: &str) -> &[MorningSource] {
        if self.room == room {
            self.sources.get(..MAX_SOURCES).unwrap_or(&self.sources)
        } else {
            &[]
        }
    }
    pub fn toggle(&mut self, room: &str, source: MorningSource) -> bool {
        if self.room != room {
            *self = Self {
                room: room.into(),
                sources: Vec::new(),
            };
        }
        if let Some(index) = self.sources.iter().position(|old| old == &source) {
            self.sources.remove(index);
        } else if self.sources.len() < MAX_SOURCES {
            self.sources.push(source);
        } else {
            return false;
        }
        true
    }
}

/// Each queued navigation/removal stays bound to the room where it was offered.
pub fn decode_action<'a>(id: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let (room, key) = id.strip_prefix(prefix)?.split_once('.')?;
    uuid::Uuid::parse_str(room).ok()?;
    (!key.is_empty() && key.len() <= 4610 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some((room, key))
}
