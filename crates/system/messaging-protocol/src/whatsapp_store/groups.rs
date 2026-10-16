//! Group reviews, one-shot writes and private text share original SDK custody.
use super::{Change, Deserialize, EncryptedBackend, Result, Serialize, key, unavailable};
use zeroize::Zeroize;
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Review {
    pub group: String,
    pub identity: (String, Option<String>),
    pub title: String,
    pub members: Vec<(String, String)>,
    pub announcement: bool,
    pub restricted: bool,
    pub expires: u64,
}
impl Review {
    pub fn same_roster(&self, other: &Self) -> bool {
        self.group == other.group
            && self.identity == other.identity
            && self.title == other.title
            && self.members == other.members
            && self.announcement == other.announcement
            && self.restricted == other.restricted
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operation {
    pub group: String,
    pub author: String,
    pub kind: String,
    pub target: String,
    pub text: String,
    pub timestamp_ms: u64,
    pub acknowledged: bool,
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.text.zeroize();
    }
}
pub(crate) use crate::signal_broker::history::groups::GroupIncoming as Incoming;
impl EncryptedBackend {
    pub(crate) async fn review_group(&self, review: Review) -> Result<String> {
        let token = uuid::Uuid::new_v4().to_string();
        let saved = token.clone();
        self.run(move |disk| {
            let now = tilo_clock::try_unix_now().ok_or_else(unavailable)?;
            let mut changes = Vec::new();
            let mut active = 0usize;
            for (key, old) in disk.rows::<Review>("tilo-group-review", &[])? {
                if old.expires <= now || old.group == review.group {
                    changes.push(Change::delete(key));
                } else {
                    active = active.saturating_add(1);
                }
            }
            if active >= 256 {
                return Err(unavailable());
            }
            changes.push(Change::put(key("tilo-group-review", &[&saved]), &review)?);
            disk.commit(changes)
        })
        .await?;
        Ok(token)
    }
    pub(crate) async fn group_review(&self, token: &str) -> Result<Option<Review>> {
        self.get(key("tilo-group-review", &[token])).await
    }
    pub(crate) async fn reserve_group_operation(
        &self,
        id: &str,
        review: &str,
        op: Operation,
    ) -> Result<bool> {
        let id = id.to_owned();
        let review = review.to_owned();
        self.run(move |disk| {
            let slot = key("tilo-group-operation", &[&id]);
            if disk.get::<Operation>(&slot)?.is_some() {
                return Ok(false);
            }
            let witness = key("tilo-group-review", &[&review]);
            let reviewed = disk.get::<Review>(&witness)?.ok_or_else(unavailable)?;
            if reviewed.group != op.group
                || reviewed.expires <= tilo_clock::try_unix_now().ok_or_else(unavailable)?
            {
                return Err(unavailable());
            }
            disk.commit(vec![Change::put(slot, &op)?, Change::delete(witness)])?;
            Ok(true)
        })
        .await
    }
    pub(crate) async fn acknowledge_group_operation(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.run(move |disk| {
            let slot = key("tilo-group-operation", &[&id]);
            let mut op = disk.get::<Operation>(&slot)?.ok_or_else(unavailable)?;
            op.acknowledged = true;
            disk.commit(vec![Change::put(slot, &op)?])
        })
        .await
    }
    pub(crate) async fn group_operations(&self, group: &str) -> Result<Vec<(String, Operation)>> {
        let group = group.to_owned();
        self.run(move |disk| {
            disk.rows::<Operation>("tilo-group-operation", &[])?
                .into_iter()
                .filter(|(_, op)| op.group == group && op.kind == "message")
                .map(|(key, op)| {
                    key.parts
                        .first()
                        .cloned()
                        .map(|id| (id, op))
                        .ok_or_else(unavailable)
                })
                .collect()
        })
        .await
    }
}
