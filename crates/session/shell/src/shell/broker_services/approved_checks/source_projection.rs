//! Trusted source projections create fresh candidates before the existing approved check.
mod filesystem;
mod snapshots;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use tilo_action_broker::collab_broker_capnp::{ApprovedCheckContext, SourceBinding};
use ush::approved_checks::{ApprovedCheck, ApprovedChecks};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition {
    pub check_id: String,
    baseline_sha256: String,
    sources: Vec<Mapping>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    path: String,
    source: SourceBinding,
    file_id: String,
}
impl Definition {
    pub(super) fn validate(&self, check: &ApprovedCheck) -> Result<(), String> {
        if self.check_id != check.id
            || self.sources.is_empty()
            || self.sources.len() > 128
            || self.baseline_sha256.len() != 64
        {
            return Err("source projection registration is malformed or oversized".into());
        }
        let entries = filesystem::inventory(&check.checkout)?;
        if filesystem::identity(&entries)? != self.baseline_sha256 {
            return Err(
                "source projection baseline differs from the owner-reviewed closure".into(),
            );
        }
        let mut paths = BTreeSet::new();
        let mut resources = BTreeSet::new();
        for mapping in &self.sources {
            filesystem::relative(&mapping.path)?;
            if !entries.contains_key(&mapping.path)
                || !paths.insert(mapping.path.clone())
                || mapping.source.workspace != check.binding.workspace.to_string()
                || !mapping.source.resource_identity.is_valid()
                || mapping.file_id.is_empty()
                || mapping.file_id.len() > 256
                || !resources.insert((
                    mapping.source.app_id.clone(),
                    mapping.source.record.clone(),
                    mapping.source.resource_identity.kind.clone(),
                    mapping.source.resource_identity.key.clone(),
                ))
            {
                return Err(
                    "source projection mapping is foreign, duplicated or outside baseline".into(),
                );
            }
        }
        Ok(())
    }
    pub(super) fn prepare(
        &self,
        context: &ApprovedCheckContext,
        template: &ApprovedCheck,
        artifacts: &Path,
    ) -> Result<Prepared, String> {
        self.validate(template)?;
        context.recheck()?;
        let mut heads = BTreeMap::new();
        let mut captures = Vec::new();
        let mut replacements = BTreeMap::new();
        let mut total = 0_usize;
        for mapping in &self.sources {
            let key = (
                mapping.source.app_id.clone(),
                mapping.source.workspace.clone(),
                mapping.source.record.clone(),
            );
            let capture = snapshots::collect(
                context,
                &mapping.source,
                &mapping.file_id,
                heads.get(&key).map(Vec::as_slice),
            )?;
            total = total
                .checked_add(capture.bytes.len())
                .ok_or("candidate source byte count overflow")?;
            if total > 24 * 1024 * 1024 {
                return Err("candidate source aggregate exceeds bound".into());
            }
            heads.insert(key, capture.heads.clone());
            replacements.insert(mapping.path.clone(), capture.bytes.clone());
            captures.push(capture);
        }
        context.recheck()?;
        for capture in &captures {
            capture.recheck()?;
        }
        let root = artifacts.join(format!("candidate-{}", context.invocation()));
        // Exclusive: an uncertain original invocation never overwrites or reuses a candidate.
        std::fs::create_dir(&root).map_err(|error| error.to_string())?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let source = root.join("source");
        let inventory = filesystem::materialize(
            &template.checkout,
            &self.baseline_sha256,
            &source,
            &replacements,
        )?;
        let source_sha256 = filesystem::identity(&inventory)?;
        let mut check = template.clone();
        check.checkout.clone_from(&source);
        check.build = root.join("build");
        check.scratch = root.join("scratch");
        for directory in [&check.build, &check.scratch] {
            private_fs::directory(directory).map_err(|error| error.to_string())?;
        }
        let provenance = serde_json::json!({
            "version": 1, "parent_invocation": context.invocation(),
            "baseline_sha256": self.baseline_sha256, "candidate_source_sha256": source_sha256,
            "inventory": inventory, "sources": captures.iter().map(|capture| &capture.observations).collect::<Vec<_>>(),
            "heads_semantics": "projection-time committed heads; no portable authority or original admission heads asserted",
        });
        let checks = ApprovedChecks::new(check.binding.clone(), vec![check])?;
        let result = Prepared {
            checks,
            captures,
            provenance,
            source,
            source_sha256,
        };
        result.recheck()?;
        Ok(result)
    }
}
pub(super) struct Prepared {
    pub checks: ApprovedChecks,
    captures: Vec<snapshots::Capture>,
    pub provenance: serde_json::Value,
    source: std::path::PathBuf,
    pub source_sha256: String,
}
impl Prepared {
    pub(super) fn verify_source(&self) -> Result<(), String> {
        filesystem::verify(&self.source, &self.source_sha256)
    }
    pub(super) fn recheck(&self) -> Result<(), String> {
        for capture in &self.captures {
            capture.recheck()?;
        }
        Ok(())
    }
}
