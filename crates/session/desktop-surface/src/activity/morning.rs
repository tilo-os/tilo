//! Device-private selections of existing workspace resources. No URL fetching,
//! provider enrollment, permission grant or inferred source selection lives here.
use serde::{Deserialize, Serialize};

pub const MAX_SOURCES: usize = 8;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MorningPreferences {
    pub workspace: String,
    pub sources: Vec<MorningSource>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct MorningSource {
    pub app: String,
    pub resource: String,
}
impl MorningSource {
    pub fn key(&self) -> String {
        hex::encode([self.app.as_bytes(), &[0], self.resource.as_bytes()].concat())
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
    /// Empty by default; a recycled workspace id never reuses another workspace's choices.
    pub fn for_workspace(&self, workspace: &str) -> &[MorningSource] {
        if self.workspace == workspace {
            self.sources.get(..MAX_SOURCES).unwrap_or(&self.sources)
        } else {
            &[]
        }
    }
    pub fn toggle(&mut self, workspace: &str, source: MorningSource) -> bool {
        if self.workspace != workspace {
            *self = Self {
                workspace: workspace.into(),
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

/// Bounded provider-owned observation, read through the currently admitted catalogue.
/// The provider owns its payload/source/time/freshness; this is never inferred from history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MorningPayload {
    pub label: String,
    pub presentation: tilo_actions::ResourcePresentation,
    pub refreshable: bool,
}
impl MorningPayload {
    pub fn text(&self) -> String {
        [
            self.presentation.preview.as_deref(),
            self.presentation.detail.as_deref(),
            self.presentation.badge.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MorningCommand {
    Open,
    Refresh,
    DraftRefresh,
    NextSource,
}
impl MorningCommand {
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Open => "activity-morning-open",
            Self::Refresh => "activity-morning-refresh",
            Self::DraftRefresh => "activity-morning-draft",
            Self::NextSource => "activity-morning-next",
        }
    }
}
