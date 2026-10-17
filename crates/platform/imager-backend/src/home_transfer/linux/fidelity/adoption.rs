//! Verify completed fidelity at each journal-witnessed retained or moved inode.
use super::super::super::promotion::AdoptionWitness;
use super::*;
pub fn verify(review: &HomeReview, witness: &AdoptionWitness) -> io::Result<CopyReport> {
    verify_with_applications(review, witness, None)
}
pub fn verify_applications(
    review: &HomeReview,
    witness: &AdoptionWitness,
) -> io::Result<CopyReport> {
    verify_with_applications(review, witness, Some(Path::new(".local/share/flatpak")))
}
pub fn verify_flatpak(review: &HomeReview, witness: &AdoptionWitness) -> io::Result<CopyReport> {
    verify_with_applications(review, witness, Some(Path::new("")))
}
fn verify_with_applications(
    review: &HomeReview,
    witness: &AdoptionWitness,
    installation: Option<&Path>,
) -> io::Result<CopyReport> {
    let source = review.validate_source()?;
    let log = Log::open(&review.journal.with_extension("home-finalization"))?;
    if log.manifest.version != 1
        || log.manifest.revision != review.revision
        || log.manifest.source != review.source_identity
        || directory_identity(&witness.quarantine)? != log.manifest.root
        || log.completed.len() != log.manifest.entries.len()
    {
        return Err(changed(
            "Adoption requires completed original fidelity receipts",
        ));
    }
    let mut report = CopyReport {
        scan_completed: true,
        ..CopyReport::default()
    };
    let cancel = AtomicBool::new(false);
    for (index, entry) in log.manifest.entries.iter().enumerate() {
        let path = decode(&entry.path)?;
        if installation.is_some()
            && matches!(entry.kind, Kind::Directory { .. })
            && installation.is_some_and(|root| application_directory(&path, root))
        {
            verify_application_directory(entry, &source, &witness.target(&path)?, &mut report)?;
        } else if installation
            .is_some_and(|root| path == root.join("repo/config") || path == root.join(".changed"))
        {
            verify_mutable_entry(
                entry,
                &source,
                witness,
                installation.ok_or_else(|| changed("Missing application installation"))?,
            )?;
        } else {
            verify_entry(index, entry, &log, &source, witness, &mut report, &cancel)?;
        }
    }
    let expected = log
        .manifest
        .entries
        .iter()
        .filter(|entry| !matches!(entry.kind, Kind::RuntimeSocket))
        .map(|entry| decode(&entry.path))
        .collect::<io::Result<std::collections::BTreeSet<_>>>()?;
    let expected = expected
        .into_iter()
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    witness
        .verify_inventory(&expected, installation)
        .map_err(|error| changed(&error))?;
    review.validate_source()?;
    Ok(report)
}
fn verify_mutable_entry(
    entry: &Entry,
    source: &Arc<PinnedDirectory>,
    witness: &AdoptionWitness,
    installation: &Path,
) -> io::Result<()> {
    let path = decode(&entry.path)?;
    let container = path
        .parent()
        .ok_or_else(|| changed("Missing app metadata parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| changed("Missing app metadata name"))?;
    let from = parent(source, container)?;
    validate_source(entry, &metadata_nofollow(&from.file, Path::new(name))?)?;
    if path == installation.join(".changed") {
        if !matches!(entry.kind, Kind::File { .. }) || entry.source.size() != 0 {
            return Err(changed("Original nonempty application marker is user data"));
        }
        let to = parent(&witness.target(&path)?, container)?;
        if regular(&to, name)?.metadata()?.len() != 0 {
            return Err(changed("Application marker contains unexpected data"));
        }
    }
    // Mapped configuration/signing policy is checked by the app controller.
    Ok(())
}
fn verify_entry(
    index: usize,
    entry: &Entry,
    log: &Log,
    source: &Arc<PinnedDirectory>,
    witness: &AdoptionWitness,
    report: &mut CopyReport,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let path = decode(&entry.path)?;
    let target = witness.target(&path)?;
    if let Kind::Directory { destination } = &entry.kind {
        let from = parent(source, &path)?;
        let to = parent(&target, &path)?;
        validate_source(entry, &Metadata::from_file(&from.file)?)?;
        let from_file = durability::readable_directory(&from.file)?;
        let to_file = durability::readable_directory(&to.file)?;
        let actual = Metadata::from_file(&to_file)?;
        let mode = if path.as_os_str().is_empty() {
            0o700
        } else {
            entry.mode & 0o777
        };
        if directory_identity(&to)? != *destination
            || actual.mode() & 0o777 != mode
            || (!witness.changed_parent(&path)?
                && actual.modified()?.into_std()
                    != Metadata::from_file(&from_file)?.modified()?.into_std())
        {
            return Err(changed("Adopted directory fidelity changed"));
        }
        copy_xattrs(&from_file, &to_file, &path, report, true)?;
        return Ok(());
    }
    let container = path
        .parent()
        .ok_or_else(|| changed("Missing adoption parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| changed("Missing adoption name"))?;
    let from = parent(source, container)?;
    let to = parent(&target, container)?;
    let metadata = metadata_nofollow(&from.file, Path::new(name))?;
    validate_source(entry, &metadata)?;
    match &entry.kind {
        Kind::File {
            digest: expected, ..
        } => {
            let original = regular(&from, name)?;
            let imported = regular(&to, name)?;
            let actual = Metadata::from_file(&imported)?;
            if log.completed.get(&index).and_then(Option::as_ref)
                != Some(&durability::identity(&imported)?)
                || digest(&original, cancel)? != *expected
                || digest(&imported, cancel)? != *expected
                || actual.mode() & 0o777 != entry.mode & 0o777
                || actual.modified()?.into_std()
                    != Metadata::from_file(&original)?.modified()?.into_std()
            {
                return Err(changed("Adopted file receipt or fidelity changed"));
            }
            copy_xattrs(&original, &imported, &path, report, true)?;
            report.verified_files.push(path);
        }
        Kind::Link { target: expected } => {
            let literal = decode(expected)?;
            if cap_primitives::fs::read_link_contents(&from.file, Path::new(name))? != literal
                || cap_primitives::fs::read_link_contents(&to.file, Path::new(name))? != literal
                || log.completed.get(&index).and_then(Option::as_ref)
                    != Some(&durability::link_identity(&to.file, name)?)
            {
                return Err(changed("Adopted literal link receipt changed"));
            }
            report.verified_files.push(path);
        }
        Kind::RuntimeSocket => {
            if log.completed.get(&index) != Some(&None)
                || !super::super::super::super::migration_review::runtime_socket(&metadata)
            {
                return Err(changed("Runtime endpoint omission receipt changed"));
            }
            match metadata_nofollow(&to.file, Path::new(name)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                _ => return Err(changed("Omitted runtime endpoint was replaced")),
            }
        }
        Kind::Special => return Err(changed("Unsupported original metadata cannot be adopted")),
        Kind::Directory { .. } => return Err(changed("Invalid directory adoption branch")),
    }
    Ok(())
}
