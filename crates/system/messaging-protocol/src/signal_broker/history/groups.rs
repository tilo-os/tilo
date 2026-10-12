//! Direct and group inbox rows share one encrypted all-or-nothing commit.
#[cfg(feature = "whatsapp-sdk")]
use super::{BTreeMap, History, MAX_ROWS};
use super::{Deserialize, SendError, Serialize, Zeroize};
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupIncoming {
    pub group: String,
    pub author: String,
    pub id: String,
    pub text: String,
    pub timestamp_ms: u64,
    pub outgoing: bool,
}
impl Drop for GroupIncoming {
    fn drop(&mut self) {
        self.text.zeroize();
    }
}
impl GroupIncoming {
    pub(super) fn key(&self) -> Result<String, SendError> {
        serde_json::to_string(&(&self.group, &self.author, &self.id))
            .map_err(|_| SendError::Unavailable)
    }
    pub(super) fn valid(&self) -> bool {
        self.group.len() <= 256
            && self.group.strip_suffix("@g.us").is_some_and(|id| {
                id.split('-').count() <= 2
                    && id
                        .split('-')
                        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            })
            && self.author.len() <= 256
            && self.author.split_once('@').is_some_and(|(id, server)| {
                ["lid", "s.whatsapp.net"].contains(&server)
                    && !id.is_empty()
                    && id.bytes().all(|b| b.is_ascii_digit())
            })
            && crate::identifier(&self.id)
            && !self.text.trim().is_empty()
            && self.text.len() <= 32 * 1024
            && self.timestamp_ms > 0
            && serde_json::to_string(&(&self.author, &self.id)).is_ok_and(|id| id.len() <= 512)
    }
}
#[cfg(feature = "whatsapp-sdk")]
impl History {
    #[cfg(feature = "whatsapp-sdk")]
    pub(crate) fn incoming_mixed(
        &mut self,
        direct: &[super::IncomingRecord<'_>],
        groups: &[GroupIncoming],
    ) -> Result<usize, SendError> {
        let count = direct.len().saturating_add(groups.len());
        if !self.usable
            || count == 0
            || count > 64
            || (!groups.is_empty() && self.owner().service != crate::Service::Whatsapp)
        {
            return Err(SendError::Unavailable);
        }
        let bytes = direct
            .iter()
            .map(|r| r.text.len())
            .chain(groups.iter().map(|r| {
                r.text
                    .len()
                    .saturating_add(r.author.len())
                    .saturating_add(r.group.len())
                    .saturating_add(r.id.len())
            }))
            .fold(0usize, usize::saturating_add);
        if bytes > 256 * 1024 {
            return Err(SendError::InvalidRequest);
        }
        let mut direct = self.stage_direct(direct)?;
        let mut staged = BTreeMap::new();
        for row in groups {
            if !row.valid() {
                return Err(SendError::InvalidRequest);
            }
            let key = row.key()?;
            if let Some(prior) = self.ledger.groups.get(&key).or_else(|| staged.get(&key)) {
                if prior != row {
                    return Err(SendError::InvalidRequest);
                }
            } else {
                staged.insert(key, row.clone());
            }
        }
        let count = direct.len().saturating_add(staged.len());
        if self
            .ledger
            .rows
            .len()
            .saturating_add(self.ledger.groups.len())
            .saturating_add(count)
            > MAX_ROWS
        {
            return Err(SendError::Unavailable);
        }
        if count == 0 {
            return Ok(0);
        }
        let direct_keys: Vec<_> = direct.keys().cloned().collect();
        let group_keys: Vec<_> = staged.keys().cloned().collect();
        self.ledger.rows.append(&mut direct);
        self.ledger.groups.append(&mut staged);
        if let Err(error) = self.persist() {
            for key in direct_keys {
                self.ledger.rows.remove(&key);
            }
            for key in group_keys {
                self.ledger.groups.remove(&key);
            }
            return Err(error);
        }
        Ok(count)
    }
    #[cfg(feature = "whatsapp-sdk")]
    pub(crate) fn group_inbox(&self, group: &str) -> Result<Vec<GroupIncoming>, SendError> {
        if !self.usable || self.owner().service != crate::Service::Whatsapp {
            return Err(SendError::Unavailable);
        }
        Ok(self
            .ledger
            .groups
            .values()
            .filter(|r| r.group == group)
            .cloned()
            .collect())
    }
}
