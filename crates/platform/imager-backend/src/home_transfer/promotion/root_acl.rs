//! Opt-in Home-root ACL intent and exact completion witness in the adoption journal.
use super::*;
use crate::portable_transfer::acl;
type Policy = [Option<Vec<u8>>; 2];
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Intent {
    expected: Policy,
    before: Policy,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    quarantine: durable::Identity,
    home: durable::Identity,
    policy_sha256: [u8; 32],
}
impl Intent {
    pub(super) fn review(q: &Arc<PinnedDirectory>, h: &Arc<PinnedDirectory>) -> io::Result<Self> {
        let expected = acl::policy(&durable::readable_directory(&q.file)?)?;
        let before = acl::policy(&durable::readable_directory(&h.file)?)?;
        acl::verify_private_root(&durable::readable_directory(&q.file)?, &expected)?;
        for (existing, selected) in before.iter().zip(&expected) {
            if existing.is_some() && existing != selected {
                return Err(changed(
                    "The current Home has a different root ACL policy; existing permissions were retained",
                ));
            }
        }
        let result = Self { expected, before };
        result.validate()?;
        Ok(result)
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        acl::validate_policy(&self.expected)?;
        acl::validate_policy(&self.before)?;
        for (before, after) in self.before.iter().zip(&self.expected) {
            if before.is_some() && before != after {
                return Err(changed(
                    "Root ACL intent would replace an existing Home policy",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn receipt(&self, plan: &Plan) -> io::Result<Receipt> {
        Ok(Receipt {
            quarantine: plan.quarantine.clone(),
            home: plan.home.clone(),
            policy_sha256: Sha256::digest(serde_json::to_vec(&self.expected)?).into(),
        })
    }
    pub(super) fn verify(
        &self,
        q: &Arc<PinnedDirectory>,
        h: &Arc<PinnedDirectory>,
        completed: bool,
    ) -> io::Result<()> {
        self.validate()?;
        let source = durable::readable_directory(&q.file)?;
        acl::verify_private_root(&source, &self.expected)?;
        let target = durable::readable_directory(&h.file)?;
        let actual = acl::policy(&target)?;
        if target.metadata()?.permissions().mode() & 0o777 != 0o700 {
            return Err(changed("The active Home root is no longer private"));
        }
        for (index, value) in actual.iter().enumerate() {
            if value != &self.expected[index] && (completed || value != &self.before[index]) {
                return Err(changed(
                    "Home root ACL differs from its durable adoption intent",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn apply(
        &self,
        q: &Arc<PinnedDirectory>,
        h: &Arc<PinnedDirectory>,
    ) -> io::Result<()> {
        self.verify(q, h, false)?;
        let source = durable::readable_directory(&q.file)?;
        let target = durable::readable_directory(&h.file)?;
        acl::copy(&source, &target, false, 0o700)?;
        durable::sync_file(&target)?;
        durable::sync_directory(&h.file)?;
        q.validate_destination()?;
        h.validate_destination()?;
        self.verify(q, h, true)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl PromotionReview {
    pub fn apply_root_acl_before_checkpoint_for_test(&self) -> io::Result<()> {
        let (q, h, j) = roots(&self.quarantine, &self.home, &self.journal)?;
        let log = Log::create(
            &j,
            self.journal
                .file_name()
                .ok_or_else(|| changed("Missing journal"))?,
            self.plan.clone(),
        )?;
        self.plan
            .root_acl
            .as_ref()
            .ok_or_else(|| changed("Missing root ACL intent"))?
            .apply(&q, &h)?;
        assert!(log.root_receipt.is_none());
        Ok(())
    }
}
