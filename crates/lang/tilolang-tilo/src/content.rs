//! What a host serves a view of the workspace's content: the limits its owners set on what
//! members post there, and the content a limit stopped it keeping.
//!
//! The reading is the Workspace package's `content` root (`workspace.content` of
//! `use tilo.workspace as workspace`), which every host serves from what it decides by as a
//! member — as it serves the workspace's members. The limits are the owners' setting every
//! member reads; a refusal is the poster's to read, and its owners', and the host's person's,
//! so a member who did not post it sees none of it. A refusal goes away when its limit is
//! raised and the content is kept, which is a new reading, and the views that read it redraw.

use std::rc::Rc;

use serde_json::{Value, json};

use crate::host::Host;

/// Where a host reads its workspace's content: what it serves as `workspace.content` each time
/// the runtime asks ([`Host::follow_content`]).
pub type ContentSource = Rc<dyn Fn() -> Option<WorkspaceContent>>;

/// Content a limit stopped a host keeping: what the member who posted it reads, naming the
/// limit, what it allowed and who can raise it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RefusedContent {
    /// The address of the record whose content it is.
    pub record: String,
    /// The content's address, lowercase BLAKE3 hex.
    pub hash: String,
    /// How many bytes it is.
    pub bytes: u64,
    /// The limit it went over: `file`, `member`, `hourly`, `workspace` or `host`.
    pub limit: String,
    /// What that limit allowed, in bytes.
    pub allowed: u64,
    /// Who posted it, by person id.
    pub member: String,
    /// When it was refused, milliseconds since the Unix epoch.
    pub at: u64,
    /// Who can raise the limit: `owners` for what members post, `host` for what the host keeps.
    pub raise: String,
}

/// What a host serves of its workspace's content: its owners' limits and what a limit refused.
/// A size is in bytes; 0 is no limit at all. It crosses to a window's process as the JSON the
/// daemon's device reads from its workspaces ([`crate::content::ContentSource`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceContent {
    /// The largest file a host keeps.
    pub file_bytes: u64,
    /// What one member's files may take together.
    pub member_bytes: u64,
    /// What one member's files may take in any hour.
    pub hourly_bytes: u64,
    /// The content a limit refused.
    pub refusals: Vec<RefusedContent>,
    /// The host's person, whose refusals it reads too: none on a device that hosts nothing.
    pub host_person: Option<String>,
}

impl WorkspaceContent {
    /// The refusal `refusal` as the reading carries it.
    fn refusal_json(refusal: &RefusedContent) -> Value {
        json!({
            "record": refusal.record,
            "hash": refusal.hash,
            "bytes": refusal.bytes,
            "limit": refusal.limit,
            "allowed": refusal.allowed,
            "member": refusal.member,
            "at": refusal.at,
            "raise": refusal.raise,
        })
    }
}

impl Host {
    /// Read the workspace's content from `source` from now on, when the runtime asks
    /// ([`Self::follow_content`]): what an embedder knows of its workspace and a view does not.
    pub fn set_content_source(&self, source: ContentSource) {
        *self.inner.content_source.borrow_mut() = Some(source);
    }

    /// Give the runtime the workspace's content as `source` has it now, when it changed: whether
    /// a view that reads `workspace.content` reads something else now.
    pub fn follow_content(&self) -> bool {
        let source = self.inner.content_source.borrow().clone();
        let Some(source) = source else {
            return false;
        };
        let Some(content) = source() else {
            return false;
        };
        self.serve_content(&content)
    }

    /// Give the runtime the workspace's content as this host serves it: the owners' limits,
    /// and the refusals the person at this host reads — their own, their workspace's owners'
    /// and, on the device that hosts it, the host's person's ([`crate::platform_apps::workspace_content`]).
    pub fn give_content(&self, content: &WorkspaceContent) {
        let _ = self.serve_content(content);
    }

    /// [`Self::give_content`], with whether a view that reads the root reads something else now.
    fn serve_content(&self, content: &WorkspaceContent) -> bool {
        let me = self
            .viewer()
            .member_id()
            .and_then(|member| member.person_id())
            .map(str::to_owned);
        let access = self.authority().access();
        let owner = |person: &str| {
            access
                .as_ref()
                .is_some_and(|held| held.policy.is_owner(person))
        };
        let refusals: Vec<Value> = content
            .refusals
            .iter()
            .filter(|refusal| {
                me.as_deref() == Some(refusal.member.as_str())
                    || me.as_deref().is_some_and(owner)
                    || (content.host_person.as_deref() == me.as_deref() && me.is_some())
            })
            .map(WorkspaceContent::refusal_json)
            .collect();
        let reading = json!({
            "file_bytes": content.file_bytes,
            "member_bytes": content.member_bytes,
            "hourly_bytes": content.hourly_bytes,
            "refusals": refusals,
        });
        match self.runtime().serve_reading(
            &crate::platform_apps::workspace_content(),
            &reading.to_string(),
        ) {
            Ok(changed) => changed,
            Err(error) => {
                tracing::error!(%error, "the workspace's content was refused as a reading");
                false
            }
        }
    }
}
