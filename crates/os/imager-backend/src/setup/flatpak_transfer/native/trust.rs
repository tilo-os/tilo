use crate::setup::migration::PinnedDirectory;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use std::{
    io::{self, Read},
    path::Path,
};

fn read(path: &Path) -> io::Result<String> {
    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| io::Error::other("Missing parent"))?,
    )?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = cap_primitives::fs::open(
        &parent.file,
        Path::new(
            path.file_name()
                .ok_or_else(|| io::Error::other("Missing name"))?,
        ),
        &options,
    )?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(
            "Repository configuration is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    parent.validate()?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(io::Error::other("Repository configuration exceeds bounds"));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}
fn value<'a>(text: &'a str, section: &str, key: &str) -> Result<Option<&'a str>, String> {
    let mut active = false;
    let mut result = None;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            active = line == section;
        } else if active
            && let Some((name, value)) = line.split_once('=')
            && name.trim() == key
        {
            if result.is_some() {
                return Err("Ambiguous Flatpak repository definition".into());
            }
            result = Some(value.trim());
        }
    }
    Ok(result)
}

/// `--if-not-exists` only reuses a target name after comparing its URL and exact
/// signing-key packet stream. A name collision can never silently substitute
/// another repository or disable verification during retry.
pub(super) fn verify_remote_reuse(home: &Path, arguments: &[String]) -> Result<(), String> {
    let definition = Path::new(
        arguments
            .last()
            .ok_or("Missing Flatpak repository definition")?,
    );
    let name = arguments
        .get(
            arguments
                .len()
                .checked_sub(2)
                .ok_or("Missing Flatpak repository name")?,
        )
        .ok_or("Missing Flatpak repository name")?;
    let text = read(definition).map_err(|error| error.to_string())?;
    let approved_url = value(&text, "[Flatpak Repo]", "Url")?.ok_or("Repository URL is missing")?;
    let approved_key =
        value(&text, "[Flatpak Repo]", "GPGKey")?.ok_or("Repository signing key is missing")?;
    let key = STANDARD
        .decode(approved_key)
        .map_err(|_| "Invalid repository signing key")?;
    if key.is_empty() {
        return Err("Repository signing key is empty".into());
    }
    let root = home.join(".local/share/flatpak");
    let config = match read(&root.join("repo/config")) {
        Ok(config) => config,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let section = format!("[remote \"{name}\"]");
    let Some(url) = value(&config, &section, "url")? else {
        return Ok(());
    };
    if url != approved_url || value(&config, &section, "gpg-verify")? != Some("true") {
        return Err("Target Flatpak repository conflicts with the reviewed source".into());
    }
    let remote = super::super::Remote {
        installation: super::super::Installation::User,
        name: name.clone(),
        url: url.into(),
        options: String::new(),
    };
    let definition = super::super::extract_remote_definition(&remote, &root)?;
    let definition = String::from_utf8(definition).map_err(|error| error.to_string())?;
    if value(&definition, "[Flatpak Repo]", "GPGKey")? != Some(approved_key) {
        return Err(
            "Target Flatpak repository signing key conflicts with the reviewed source".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_reuses_only_exact_verified_repository_url_and_key() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let repo = home.join(".local/share/flatpak/repo");
        std::fs::create_dir_all(&repo).unwrap();
        let approved = root.join("approved.flatpakrepo");
        std::fs::write(
            &approved,
            format!(
                "[Flatpak Repo]\nUrl=https://example.org/repo/\nGPGKey={}\n",
                STANDARD.encode(b"existing-signing-key")
            ),
        )
        .unwrap();
        std::fs::write(
            repo.join("trusted.trustedkeys.gpg"),
            b"existing-signing-key",
        )
        .unwrap();
        let config = "[remote \"trusted\"]\nurl=https://example.org/repo/\ngpg-verify=true\n";
        std::fs::write(repo.join("config"), config).unwrap();
        let arguments = vec![
            "remote-add".into(),
            "--user".into(),
            "--if-not-exists".into(),
            "--from".into(),
            "trusted".into(),
            approved.to_str().unwrap().into(),
        ];
        verify_remote_reuse(&home, &arguments).unwrap();
        std::fs::write(
            repo.join("config"),
            config.replace("gpg-verify=true", "gpg-verify=false"),
        )
        .unwrap();
        assert!(verify_remote_reuse(&home, &arguments).is_err());
        std::fs::write(
            repo.join("config"),
            config.replace("example.org", "other.example.org"),
        )
        .unwrap();
        assert!(verify_remote_reuse(&home, &arguments).is_err());
        std::fs::write(repo.join("config"), config).unwrap();
        std::fs::write(repo.join("trusted.trustedkeys.gpg"), b"other-signing-key").unwrap();
        assert!(verify_remote_reuse(&home, &arguments).is_err());
    }
}
