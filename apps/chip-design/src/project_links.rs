//! Bounded references to original workspace actions, never execution state.
use serde::{Deserialize, Serialize};
use ui::tilo_actions::remote::{CanonicalActionRoute, ViewResourceQuery};
use ui::tilo_actions::{ActionAddress, InvocationId, ResourceSemanticIdentity};

pub const MAX_PROJECTS: usize = 16;
pub const MAX_LINK_BYTES: usize = 32768;
pub const PREFIX: &str = "experiment:";
const REFERENCE_PREFIX: &str = "experiment-reference:";
const MAX_REFERENCES: usize = MAX_PROJECTS * 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionLink {
    /// Original advertised publisher token; descriptive, never an access grant.
    /// Legacy links remain readable but require selection again before execution.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    pub route: CanonicalActionRoute,
    pub address: ActionAddress,
    pub resource: ViewResourceQuery,
    pub semantic_identity: ResourceSemanticIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Experiment {
    pub name: String,
    pub source: ActionLink,
    pub check: ActionLink,
    pub check_id: String,
    #[serde(default)]
    pub invocations: Vec<InvocationLink>,
    /// Computed from reported references, never stored as execution state.
    #[serde(default)]
    pub references_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationLink {
    pub parent: InvocationId,
    pub invocation: InvocationId,
    pub action: ActionLink,
    pub resources: Vec<String>,
}

impl Experiment {
    pub fn parse(json: &str) -> Result<Self, String> {
        if json.len() > MAX_LINK_BYTES {
            return Err("experiment references exceed bounds".into());
        }
        let experiment: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        experiment.validate()?;
        if experiment.references_truncated {
            return Err("binding cannot supply reference truncation metadata".into());
        }
        if serde_json::to_vec(&experiment)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_LINK_BYTES
        {
            return Err("experiment references exceed bounds".into());
        }
        Ok(experiment)
    }
    pub fn validate(&self) -> Result<(), String> {
        for value in [&self.name, &self.check_id] {
            if value.trim().is_empty() || value.len() > 256 {
                return Err("experiment name or check identity is invalid".into());
            }
        }
        self.source.validate(
            "so.tilo.code-editor",
            "code_editor.file",
            "read-text-snapshot",
        )?;
        self.check.validate(
            "so.tilo.git-client",
            "git_client.repo",
            "run-approved-check",
        )?;
        if self.source.route.room != self.check.route.room {
            return Err("experiment actions must share a workspace".into());
        }
        if self.invocations.len() > 16 {
            return Err("experiment invocation reference limit reached".into());
        }
        for link in &self.invocations {
            if link.action != self.source && link.action != self.check {
                return Err("invocation does not reference a project action".into());
            }
            if link.resources.len() > 16 || link.resources.iter().any(|value| value.len() > 4096) {
                return Err("original resource references exceed bounds".into());
            }
        }
        Ok(())
    }
}
impl ActionLink {
    fn validate(&self, app: &str, kind: &str, action: &str) -> Result<(), String> {
        if self.provider.len() > 1024
            || self.provider.trim() != self.provider
            || self.route.app_id != app
            || self.route.record_id.is_empty()
            || self.route.room.is_empty()
            || self.route.room.len() > 1024
            || self.route.record_id.len() > 4096
            || self.resource.id.is_empty()
            || self.resource.id.len() > 4096
            || self.resource.kind != kind
            || self.address.resource.is_empty()
            || self.address.resource.len() > 4096
            || self.address.action != action
            || self.semantic_identity.kind != kind
            || !self.semantic_identity.is_valid()
        {
            return Err(
                "original action needs an exact typed workspace record and semantic identity"
                    .into(),
            );
        }
        Ok(())
    }
}

fn reference_prefix(name: &str) -> String {
    format!(
        "{REFERENCE_PREFIX}{}:",
        blake3::hash(name.as_bytes()).to_hex()
    )
}

pub fn read(doc: &ui::tilo_resource::Doc, json: &str) -> Result<Experiment, String> {
    use ui::automerge::{ROOT, ReadDoc};
    let mut experiment = Experiment::parse(json)?;
    if !experiment.invocations.is_empty() {
        return Err("binding contains invocation references".into());
    }
    let prefix = reference_prefix(&experiment.name);
    for (index, key) in doc
        .keys(ROOT)
        .filter(|key| key.starts_with(&prefix))
        .enumerate()
    {
        if index == 16 {
            experiment.references_truncated = true;
            break;
        }
        let Some((value, _)) = doc.get(ROOT, &key).map_err(|error| error.to_string())? else {
            continue;
        };
        let raw = value.as_str().ok_or("invalid invocation reference")?;
        if raw.len() > MAX_LINK_BYTES {
            experiment.references_truncated = true;
            break;
        }
        let link: InvocationLink = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        if key != format!("{prefix}{}", link.invocation.0) {
            return Err("original invocation key mismatch".into());
        }
        experiment.invocations.push(link);
        if serde_json::to_vec(&experiment)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_LINK_BYTES
        {
            experiment.invocations.pop();
            experiment.references_truncated = true;
            break;
        }
    }
    experiment.validate()?;
    Ok(experiment)
}

pub fn retain(
    doc: &mut ui::tilo_resource::Doc,
    name: &str,
    json: &str,
) -> Result<(), ui::tilo_actions::ActionError> {
    use ui::automerge::{ROOT, ReadDoc, transaction::Transactable};
    use ui::tilo_actions::ActionError;
    if json.len() > MAX_LINK_BYTES {
        return Err(ActionError::Refused(
            "invocation reference exceeds bounds".into(),
        ));
    }
    let key = format!("{PREFIX}{name}");
    let Some((value, _)) = doc.get(ROOT, &key)? else {
        return Err(ActionError::Refused("experiment no longer exists".into()));
    };
    let mut experiment = read(
        doc,
        value
            .as_str()
            .ok_or_else(|| ActionError::Refused("invalid experiment".into()))?,
    )
    .map_err(ActionError::Refused)?;
    if experiment.references_truncated {
        return Err(ActionError::Refused(
            "reported references exceed display bounds; inspect original Jobs".into(),
        ));
    }
    let link: InvocationLink =
        serde_json::from_str(json).map_err(|error| ActionError::Refused(error.to_string()))?;
    let reference_key = format!("{}{}", reference_prefix(name), link.invocation.0);
    if doc.get(ROOT, &reference_key)?.is_none()
        && doc
            .keys(ROOT)
            .filter(|key| key.starts_with(REFERENCE_PREFIX))
            .count()
            >= MAX_REFERENCES
    {
        return Err(ActionError::Refused(
            "project invocation reference limit reached".into(),
        ));
    }

    if let Some(existing) = experiment
        .invocations
        .iter_mut()
        .find(|existing| existing.invocation == link.invocation)
    {
        if existing.parent != link.parent
            || existing.action != link.action
            || (!existing.resources.is_empty() && existing.resources != link.resources)
        {
            return Err(ActionError::Refused(
                "original invocation identity cannot be replaced".into(),
            ));
        }
        *existing = link;
    } else {
        experiment.invocations.push(link);
    }
    experiment.validate().map_err(ActionError::Refused)?;
    if serde_json::to_vec(&experiment)
        .map_err(|error| ActionError::Refused(error.to_string()))?
        .len()
        > MAX_LINK_BYTES
    {
        return Err(ActionError::Refused(
            "experiment references exceed bounds".into(),
        ));
    }
    // Independent CRDT keys preserve concurrent references instead of replacing
    // a serialized project list. These are reported links, never host authority.
    doc.put(ROOT, reference_key, json)?;
    Ok(())
}

pub fn remove(
    doc: &mut ui::tilo_resource::Doc,
    name: &str,
) -> Result<(), ui::tilo_actions::ActionError> {
    use ui::automerge::{ROOT, ReadDoc, transaction::Transactable};
    doc.delete(ROOT, format!("{PREFIX}{name}"))?;
    let prefix = reference_prefix(name);
    let references: Vec<_> = doc
        .keys(ROOT)
        .filter(|key| key.starts_with(&prefix))
        .collect();
    for reference in references {
        doc.delete(ROOT, reference)?;
    }
    Ok(())
}

pub fn has_references(doc: &ui::tilo_resource::Doc, name: &str) -> bool {
    use ui::automerge::{ROOT, ReadDoc};
    let prefix = reference_prefix(name);
    doc.keys(ROOT).any(|key| key.starts_with(&prefix))
}
