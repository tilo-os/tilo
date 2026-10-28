//! Bot entries in a registry catalogue: what the Store lists, the program a
//! version binds, and whether an installed bot is still listed.

use serde::{Deserialize, Serialize};

use registry_types::{BotArtifact, EntryKind, RegistryEntry, RegistryImageKind};

use crate::client::{RegistryError, VerifiedEntry, fetch_bounded};

/// One bot as the Store lists it: the newest signed version of a bot entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueBot {
    pub package_id: String,
    pub name: String,
    pub tagline: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub publisher_pubkey_hex: String,
    pub registry_name: String,
    /// The newest listed version.
    pub version: String,
    pub artifact: BotArtifact,
}

fn newest_bot_version(entry: &RegistryEntry) -> Option<(&str, &BotArtifact)> {
    entry
        .versions
        .iter()
        .filter_map(|version| version.bot().map(|bot| (version.version.as_str(), bot)))
        .max_by(|a, b| tilo_app_package::version::compare(a.0, b.0))
}

/// The newest version of a signed bot entry, as the Store lists it. Apps and
/// unsigned entries answer `None`.
#[must_use]
pub fn bot_offer(entry: &VerifiedEntry, registry_name: &str) -> Option<CatalogueBot> {
    if entry.entry.kind != EntryKind::Bot || !entry.trust.is_offerable() {
        return None;
    }
    let (version, artifact) = newest_bot_version(&entry.entry)?;
    let e = &entry.entry;
    let locale = crate::preferred_locale();
    Some(CatalogueBot {
        package_id: e.package_id.clone(),
        name: e.name.resolve(&locale)?.to_owned(),
        tagline: e.tagline.resolve(&locale)?.to_owned(),
        description: e.description.resolve(&locale)?.to_owned(),
        icon_url: e
            .images
            .resolve(&locale)
            .iter()
            .find(|image| image.kind == RegistryImageKind::Icon)
            .map(|image| image.url.clone()),
        publisher_pubkey_hex: e.publisher_pubkey_hex.clone(),
        registry_name: registry_name.to_owned(),
        version: version.to_owned(),
        artifact: artifact.clone(),
    })
}

/// `version` of the signed bot `package_id`, when the catalogue still lists it.
#[must_use]
pub fn listed_version<'a>(
    entries: &'a [VerifiedEntry],
    package_id: &str,
    version: &str,
) -> Option<(&'a RegistryEntry, &'a BotArtifact)> {
    let entry = entries.iter().find(|entry| {
        entry.entry.package_id == package_id
            && entry.entry.kind == EntryKind::Bot
            && entry.trust.is_offerable()
    })?;
    let artifact = entry
        .entry
        .versions
        .iter()
        .find(|v| v.version == version)?
        .bot()?;
    Some((&entry.entry, artifact))
}

/// Download the program `bot` binds, refusing more bytes than it declares and
/// any bytes that do not hash to its signed BLAKE3.
///
/// # Errors
/// Transport errors, an unsafe URL, or [`RegistryError::ProgramMismatch`].
pub fn fetch_bot_program(bot: &BotArtifact) -> Result<Vec<u8>, RegistryError> {
    let url = reqwest::Url::parse(&bot.program_url).map_err(|_| RegistryError::UnsafeEndpoint)?;
    let bytes = fetch_bounded(&url, bot.program_size_bytes, "bot program")?;
    if bytes.len() as u64 != bot.program_size_bytes {
        return Err(RegistryError::ProgramMismatch(format!(
            "{} bytes, the entry says {}",
            bytes.len(),
            bot.program_size_bytes
        )));
    }
    if blake3::hash(&bytes).to_hex().as_str() != bot.program_blake3_hex {
        return Err(RegistryError::ProgramMismatch(
            "BLAKE3 differs from program_blake3_hex".into(),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    fn bot(version: &str, program: &[u8], base: &str) -> registry_types::RegistryVersion {
        let hash = blake3::hash(program).to_hex().to_string();
        registry_types::RegistryVersion {
            version: version.into(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Bot(registry_types::BotArtifact {
                program_url: registry_types::bot_program_url(base, &hash),
                program_blake3_hex: hash,
                program_size_bytes: program.len() as u64,
                requires: registry_types::BotRequirements {
                    connectors: Vec::new(),
                    apps: vec!["so.tilo.chat".into()],
                    appearance: false,
                    triggers: vec!["record_changed".into()],
                },
            }),
        }
    }

    /// Serve `body` once on loopback; answers the origin.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        origin
    }

    #[test]
    fn a_fetched_program_must_match_its_signed_hash_and_size() {
        let program = b"{\"id\":\"echo\"}".to_vec();
        let origin = serve_once(program.clone());
        let artifact = bot("1.0.0", &program, &origin);
        assert_eq!(fetch_bot_program(artifact.bot().unwrap()).unwrap(), program);
        // Same length, other bytes: only the hash check can refuse it.
        let origin = serve_once(b"{\"id\":\"evil\"}".to_vec());
        let swapped = bot("1.0.0", &program, &origin);
        assert!(matches!(
            fetch_bot_program(swapped.bot().unwrap()),
            Err(RegistryError::ProgramMismatch(_))
        ));
    }
}
