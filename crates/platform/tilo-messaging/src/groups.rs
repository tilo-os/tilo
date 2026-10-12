//! Private service/account group inspection, never a workspace membership list.
use crate::{SendError, ServiceAccount};
use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc};

pub const MAX_GROUPS: usize = 256;
pub const MAX_GROUP_MEMBERS: usize = 2048;
const MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct GroupSummary {
    pub id: String,
    pub title: String,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GroupRole {
    Member,
    Admin,
    SuperAdmin,
}
#[derive(Clone, PartialEq, Eq)]
pub struct GroupMember {
    /// The service's canonical identity; not a display name or inferred contact.
    pub address: String,
    pub role: GroupRole,
}
pub struct GroupList {
    pub account: ServiceAccount,
    pub groups: Vec<GroupSummary>,
}
pub struct GroupMembership {
    pub account: ServiceAccount,
    pub group: GroupSummary,
    pub members: Vec<GroupMember>,
    pub announcement: bool,
    /// Exact roster identity of the authenticated account owner.
    pub self_member: String,
    /// Opaque, short-lived review bound to this account's exact service roster.
    pub review: String,
}
pub enum GroupChange {
    Add(tilo_contact_directory::routing::Route),
    Remove(String),
    Leave,
}
pub struct GroupChangeRequest {
    pub group: String,
    pub review: String,
    pub operation: String,
    pub change: GroupChange,
}
pub struct GroupSendRequest {
    pub group: String,
    pub review: String,
    pub operation: String,
    pub text: zeroize::Zeroizing<String>,
}
pub struct GroupReceipt {
    pub account: ServiceAccount,
    pub group: String,
    pub operation: String,
}
pub struct GroupHistory {
    pub account: ServiceAccount,
    pub group: String,
    pub messages: Vec<GroupMessage>,
}
pub struct GroupMessage {
    pub author: String,
    pub message: crate::Message,
}
pub type GroupActionFuture = Pin<Box<dyn Future<Output = Result<GroupReceipt, SendError>> + Send>>;
pub type GroupHistoryFuture = Pin<Box<dyn Future<Output = Result<GroupHistory, SendError>> + Send>>;
pub type GroupsFuture = Pin<Box<dyn Future<Output = Result<GroupList, SendError>> + Send>>;
pub type MembershipFuture =
    Pin<Box<dyn Future<Output = Result<GroupMembership, SendError>> + Send>>;

/// Supplied by the original account host. Reads must recheck credential/network
/// custody and authenticated self membership before and after service I/O.
/// A snapshot grants neither permission to send nor membership in a workspace.
pub trait GroupHost: Send + Sync {
    fn list(&self) -> GroupsFuture;
    fn read(&self, group: String) -> MembershipFuture;
    fn change(&self, _: GroupChangeRequest) -> GroupActionFuture {
        Box::pin(std::future::ready(Err(SendError::Unavailable)))
    }
    fn send(&self, _: GroupSendRequest) -> GroupActionFuture {
        Box::pin(std::future::ready(Err(SendError::Unavailable)))
    }
    fn history(&self, _: String) -> GroupHistoryFuture {
        Box::pin(std::future::ready(Err(SendError::Unavailable)))
    }
}
#[derive(Clone)]
pub struct GroupClient {
    principal: String,
    account: ServiceAccount,
    host: Arc<dyn GroupHost>,
}
impl GroupClient {
    pub fn new(principal: String, account: ServiceAccount, host: Arc<dyn GroupHost>) -> Self {
        Self {
            principal,
            account,
            host,
        }
    }
    fn check_identity(&self) -> Result<(), SendError> {
        if self.account.service != "whatsapp"
            || self.account.account.is_empty()
            || self.account.account.len() > 512
            || self
                .account
                .account
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            || self.principal.is_empty()
            || self.principal.len() > 512
            || self.principal.chars().any(char::is_control)
        {
            return Err(SendError::InvalidRequest);
        }
        Ok(())
    }
    pub fn principal(&self) -> &str {
        &self.principal
    }
    pub fn account(&self) -> &ServiceAccount {
        &self.account
    }
    /// # Errors
    /// Refuses a foreign account, duplicate/invalid identities or oversized page.
    pub async fn list(&self) -> Result<GroupList, SendError> {
        self.check_identity()?;
        let page = self.host.list().await?;
        if page.account != self.account || page.groups.len() > MAX_GROUPS {
            return Err(SendError::Revoked);
        }
        let mut ids = BTreeSet::new();
        let mut bytes = 0usize;
        for group in &page.groups {
            validate_group(group)?;
            bytes = bytes
                .saturating_add(group.id.len())
                .saturating_add(group.title.len());
            if !ids.insert(&group.id) || bytes > MAX_BYTES {
                return Err(SendError::InvalidRequest);
            }
        }
        Ok(page)
    }
    /// # Errors
    /// Refuses another account/group, ambiguous members and oversized responses.
    pub async fn read(&self, group: String) -> Result<GroupMembership, SendError> {
        self.check_identity()?;
        if !valid_group(&group) {
            return Err(SendError::InvalidRequest);
        }
        let page = self.host.read(group.clone()).await?;
        if page.account != self.account || page.group.id != group {
            return Err(SendError::Revoked);
        }
        if !valid_token(&page.review) {
            return Err(SendError::InvalidRequest);
        }
        validate_group(&page.group)?;
        if page.members.is_empty() || page.members.len() > MAX_GROUP_MEMBERS {
            return Err(SendError::InvalidRequest);
        }
        let mut ids = BTreeSet::new();
        let mut bytes = page.group.id.len().saturating_add(page.group.title.len());
        for member in &page.members {
            bytes = bytes.saturating_add(member.address.len());
            if !valid_member(&member.address) || !ids.insert(&member.address) || bytes > MAX_BYTES {
                return Err(SendError::InvalidRequest);
            }
        }
        if !ids.contains(&page.self_member) {
            return Err(SendError::Revoked);
        }
        Ok(page)
    }
    /// # Errors
    /// Refuses invalid review/operation ids, another sender or malformed targets.
    pub async fn change(&self, request: GroupChangeRequest) -> Result<GroupReceipt, SendError> {
        self.operation(&request.group, &request.review, &request.operation)?;
        match &request.change {
            GroupChange::Add(route)
                if route.service != self.account.service
                    || route.sender.as_str() != self.account.account
                    || !valid_member(&route.recipient)
                    || crate::history::validate_route(route).is_err() =>
            {
                return Err(SendError::InvalidRequest);
            }
            GroupChange::Remove(member) if !valid_member(member) => {
                return Err(SendError::InvalidRequest);
            }
            _ => {}
        }
        let (group, operation) = (request.group.clone(), request.operation.clone());
        let receipt = self.host.change(request).await?;
        self.receipt(receipt, &group, &operation)
    }
    /// # Errors
    /// Refuses invalid text/review/operation ids and mismatched SDK receipts.
    pub async fn send(&self, request: GroupSendRequest) -> Result<GroupReceipt, SendError> {
        self.operation(&request.group, &request.review, &request.operation)?;
        if request.text.trim().is_empty() || request.text.len() > 32 * 1024 {
            return Err(SendError::InvalidRequest);
        }
        let (group, operation) = (request.group.clone(), request.operation.clone());
        let receipt = self.host.send(request).await?;
        self.receipt(receipt, &group, &operation)
    }
    /// # Errors
    /// Refuses another account/group or malformed private history projection.
    pub async fn history(&self, group: String) -> Result<GroupHistory, SendError> {
        self.check_identity()?;
        if !valid_group(&group) {
            return Err(SendError::InvalidRequest);
        }
        let page = self.host.history(group.clone()).await?;
        if page.account != self.account
            || page.group != group
            || page.messages.len() > crate::MAX_HISTORY_MESSAGES
        {
            return Err(SendError::Revoked);
        }
        let mut ids = BTreeSet::new();
        let mut bytes = 0usize;
        for row in &page.messages {
            bytes = bytes
                .saturating_add(row.message.text.len())
                .saturating_add(row.author.len())
                .saturating_add(row.message.id.len());
            if !valid_member(&row.author)
                || row.message.id.is_empty()
                || row.message.id.len() > 512
                || !ids.insert(&row.message.id)
                || row.message.text.is_empty()
                || row.message.text.len() > 32 * 1024
                || row.message.timestamp_ms == 0
                || bytes > MAX_BYTES
            {
                return Err(SendError::InvalidRequest);
            }
        }
        Ok(page)
    }
    fn operation(&self, group: &str, review: &str, operation: &str) -> Result<(), SendError> {
        self.check_identity()?;
        if !valid_group(group) || [review, operation].iter().any(|id| !valid_token(id)) {
            return Err(SendError::InvalidRequest);
        }
        Ok(())
    }
    fn receipt(
        &self,
        receipt: GroupReceipt,
        group: &str,
        operation: &str,
    ) -> Result<GroupReceipt, SendError> {
        if receipt.account != self.account
            || receipt.group != group
            || receipt.operation != operation
        {
            return Err(SendError::Uncertain);
        }
        Ok(receipt)
    }
}
fn validate_group(group: &GroupSummary) -> Result<(), SendError> {
    if !valid_group(&group.id)
        || group.title.len() > 4096
        || group.title.chars().any(char::is_control)
    {
        return Err(SendError::InvalidRequest);
    }
    Ok(())
}
fn valid_group(value: &str) -> bool {
    value.len() <= 256
        && value.strip_suffix("@g.us").is_some_and(|user| {
            let parts: Vec<_> = user.split('-').collect();
            parts.len() <= 2
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
}
fn valid_member(value: &str) -> bool {
    value.len() <= 256
        && value.split_once('@').is_some_and(|(user, server)| {
            ["lid", "s.whatsapp.net"].contains(&server)
                && !user.is_empty()
                && user.bytes().all(|b| b.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests;

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
