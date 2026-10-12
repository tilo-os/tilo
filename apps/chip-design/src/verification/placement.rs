//! Bounded reported macro footprints, without layout authentication or signoff.
use super::VerificationReceipt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub database_artifact: String,
    pub observation_artifact: String,
    pub dbu_per_micron: u32,
    pub die_dbu: [i32; 4],
    pub total_instances: u32,
    pub omitted_instances: u32,
    pub macros: Vec<Macro>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Macro {
    pub id: String,
    pub name: String,
    pub master: String,
    pub orientation: Orientation,
    pub status: PlacementStatus,
    pub bounds_dbu: [i32; 4],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    R0,
    R90,
    R180,
    R270,
    MX,
    MY,
    MXR90,
    MYR90,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementStatus {
    Placed,
    Locked,
    Firm,
    Cover,
}

fn rectangle(bounds: &[i32; 4]) -> bool {
    bounds
        .iter()
        .all(|coordinate| coordinate.abs_diff(0) <= 1_000_000_000)
        && bounds[0] < bounds[2]
        && bounds[1] < bounds[3]
}

fn name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

impl Placement {
    pub fn validate(&self, receipt: &VerificationReceipt) -> Result<(), String> {
        if !(1..=1_000_000).contains(&self.dbu_per_micron)
            || !rectangle(&self.die_dbu)
            || self.macros.is_empty()
            || self.macros.len() > 256
            || self.total_instances > 1_000_000
            || self
                .omitted_instances
                .checked_add(u32::try_from(self.macros.len()).unwrap_or(u32::MAX))
                != Some(self.total_instances)
            || !receipt
                .artifact_sha256
                .contains_key(&self.database_artifact)
            || !receipt
                .artifact_sha256
                .contains_key(&self.observation_artifact)
        {
            return Err("invalid reported macro placement envelope".into());
        }
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for item in &self.macros {
            if !name(&item.name)
                || !name(&item.master)
                || item.id.is_empty()
                || item.id.len() > 128
                || !item
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                || !ids.insert(&item.id)
                || !names.insert(&item.name)
                || !rectangle(&item.bounds_dbu)
                || item.bounds_dbu[0] < self.die_dbu[0]
                || item.bounds_dbu[1] < self.die_dbu[1]
                || item.bounds_dbu[2] > self.die_dbu[2]
                || item.bounds_dbu[3] > self.die_dbu[3]
            {
                return Err("invalid reported macro identity or bounds".into());
            }
        }
        Ok(())
    }
}
