use std::{fs, io, path::Path};
fn refusal(reason: &str) -> io::Error {
    io::Error::other(reason)
}
pub(super) fn no_holders(path: &Path, whole_disk: bool) -> io::Result<()> {
    if fs::read_dir(path.join("holders"))?
        .next()
        .transpose()?
        .is_some()
    {
        return Err(refusal("Busy: native device has holders"));
    }
    if whole_disk
        && fs::read_dir(path.join("slaves"))?
            .next()
            .transpose()?
            .is_some()
    {
        return Err(refusal("unsupported stacked device"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::no_holders;
    use std::fs;
    #[test]
    fn physical_partition_has_holders_but_parent_also_requires_slaves() {
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("disk");
        let partition = parent.join("partition");
        fs::create_dir_all(parent.join("holders")).unwrap();
        fs::create_dir_all(parent.join("slaves")).unwrap();
        fs::create_dir_all(partition.join("holders")).unwrap();
        no_holders(&parent, true).unwrap();
        no_holders(&partition, false).unwrap();
        fs::write(partition.join("holders/alias"), []).unwrap();
        assert!(no_holders(&partition, false).is_err());
        fs::remove_file(partition.join("holders/alias")).unwrap();
        fs::remove_dir(partition.join("holders")).unwrap();
        assert!(no_holders(&partition, false).is_err());
        fs::remove_dir(parent.join("slaves")).unwrap();
        assert!(no_holders(&parent, true).is_err());
    }
}
