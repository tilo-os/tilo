//! Collision metadata only; neither variant grants access to source contents.
use super::*;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MountedGeometry {
    schema: u32,
    operation: String,
    mounted_entry: EntryBinding,
}
pub enum SourceGeometry {
    Home(Plan),
    Mounted {
        operation: String,
        entry: EntryBinding,
    },
}
impl SourceGeometry {
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > MAX_BYTES {
            return Err(refusal("Source geometry exceeds bound"));
        }
        let root: serde_json::Value = serde_json::from_slice(bytes)?;
        if root.get("mounted_entry").is_none() {
            return Ok(Self::Home(Plan::parse(bytes)?));
        }
        let mounted: MountedGeometry = serde_json::from_slice(bytes)?;
        mounted.mounted_entry.validate()?;
        if mounted.schema != 1
            || !uuid::Uuid::parse_str(&mounted.operation)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == mounted.operation)
        {
            return Err(refusal("Mounted source geometry operation differs"));
        }
        Ok(Self::Mounted {
            operation: mounted.operation,
            entry: mounted.mounted_entry,
        })
    }
    pub fn volume(&self) -> &SourceVolume {
        match self {
            Self::Home(plan) => &plan.home.volume,
            Self::Mounted { entry, .. } => entry.volume(),
        }
    }
}
/// Only actual protected mounted records feed this helper in production. The
/// bytes bind the existing output collision digest; they grant no read/mount.
pub fn append_mounted_geometry(
    plans: &mut Vec<Vec<u8>>,
    operation: &str,
    entries: &[EntryBinding],
) -> io::Result<()> {
    let mut volumes = plans
        .iter()
        .map(|bytes| SourceGeometry::parse(bytes).map(|geometry| geometry.volume().clone()))
        .collect::<io::Result<Vec<_>>>()?;
    let mut additions = Vec::new();
    for entry in entries {
        entry.validate()?;
        if volumes.contains(entry.volume()) {
            continue;
        }
        volumes.push(entry.volume().clone());
        additions.push(serde_json::to_vec(&MountedGeometry {
            schema: 1,
            operation: operation.into(),
            mounted_entry: entry.clone(),
        })?);
    }
    additions.sort();
    for bytes in &additions {
        SourceGeometry::parse(bytes)?;
    }
    plans.extend(additions);
    if plans.len() > 128
        || plans
            .iter()
            .try_fold(0usize, |n, bytes| n.checked_add(bytes.len()))
            .is_none_or(|n| n > 9 * 1024 * 1024)
    {
        return Err(refusal("Source geometry exceeds complete-volume bound"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_mounted_geometry_preserves_file_and_filesystem_root_without_fake_home() {
        let mut plan = super::super::tests::sample();
        plan.operation_id = uuid::Uuid::new_v4().to_string();
        let mut entry = EntryBinding {
            home: plan.home.clone(),
            kind: EntryKind::Directory,
            group: 1002,
            mode: libc::S_IFDIR | 0o750,
        };
        entry.home.relative = NativePath::encode(Path::new(""));
        entry.validate().unwrap();
        let mut plans = vec![plan.bytes().unwrap()];
        // Exact same-volume entries need no additional physical collision rule.
        append_mounted_geometry(&mut plans, &plan.operation_id, &[entry.clone()]).unwrap();
        assert_eq!(plans, vec![plan.bytes().unwrap()]);
        entry.home.volume.partition_guid = uuid::Uuid::new_v4().to_string();
        entry.home.volume.filesystem_uuid = uuid::Uuid::new_v4().to_string();
        append_mounted_geometry(&mut plans, &plan.operation_id, &[entry.clone()]).unwrap();
        match SourceGeometry::parse(&plans[1]).unwrap() {
            SourceGeometry::Mounted {
                operation,
                entry: observed,
            } => {
                assert_eq!(operation, plan.operation_id);
                assert_eq!(observed, entry);
            }
            SourceGeometry::Home(_) => {
                panic!("A filesystem-root participant must not become a synthetic Home")
            }
        }
        assert!(Plan::parse(&plans[1]).is_err());
        entry.kind = EntryKind::File;
        entry.mode = libc::S_IFREG | 0o640;
        entry.home.relative = NativePath::encode(Path::new("real-file"));
        let bytes = serde_json::to_vec(&MountedGeometry {
            schema: 1,
            operation: plan.operation_id.clone(),
            mounted_entry: entry,
        })
        .unwrap();
        assert!(
            matches!(SourceGeometry::parse(&bytes).unwrap(), SourceGeometry::Mounted {entry,..} if entry.kind()==EntryKind::File)
        );
        for field in ["path", "uid", "grant"] {
            let mut extra: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            extra[field] = serde_json::json!("untrusted");
            assert!(SourceGeometry::parse(&serde_json::to_vec(&extra).unwrap()).is_err());
        }
    }
}
