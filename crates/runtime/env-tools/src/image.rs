//! Sealing a built tree into a `tool` image.
//!
//! The manifest is a pure function of the cache key and the request, so one
//! signer sealing one key's tree twice produces the same package id.

use std::io::Write as _;
use std::path::Path;

use tilo_app_package::manifest::Arch;
use tilo_app_package::{BlobSource, ImageSpec, Kind, Packed, SigningIdentity};

use crate::key::CacheKey;

/// The version every tool image carries. A tool's identity is its package
/// id; what it holds is named by the packages in its manifest.
pub const TOOL_VERSION: &str = "1.0.0";

/// The architecture this process's binaries — and so the runtime it builds
/// on — run on.
///
/// # Errors
///
/// On an architecture images aren't built for.
pub fn host_arch() -> anyhow::Result<Arch> {
    match std::env::consts::ARCH {
        "x86_64" => Ok(Arch::X86_64),
        "aarch64" => Ok(Arch::AArch64),
        other => anyhow::bail!("tool images are built for x86_64 or aarch64, not {other}"),
    }
}

/// `so.tilo.tool.<first requested package>`, lower-case, with anything but
/// letters, digits and dashes turned into dashes.
pub fn tool_id(requested: &[String]) -> String {
    let first = requested.first().map_or("tool", String::as_str);
    let slug: String = first
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    format!(
        "so.tilo.tool.{}",
        if slug.is_empty() { "tool" } else { slug }
    )
}

/// The image description of `key`'s tool.
pub fn tool_spec(key: &CacheKey, requested: &[String], arch: Arch) -> ImageSpec {
    ImageSpec {
        kind: Kind::Tool,
        id: tool_id(requested),
        version: TOOL_VERSION.to_owned(),
        name: Some(requested.join(", ")),
        arch,
        runtime: Some(key.runtime),
        packages: key.nevras.clone(),
    }
}

/// Seal `tree` into a signed tool image for `key`.
///
/// # Errors
///
/// What [`tilo_app_package::seal_image`] returns.
pub fn seal_tool(
    tree: &Path,
    key: &CacheKey,
    requested: &[String],
    arch: Arch,
    identity: &SigningIdentity,
) -> anyhow::Result<Packed> {
    Ok(tilo_app_package::seal_image(
        tree,
        &tool_spec(key, requested, arch),
        identity,
    )?)
}

/// Write `packed` as a full `.tapp` at `path` (via a temporary file beside
/// it, renamed into place).
///
/// # Errors
///
/// Creating, writing or renaming the file.
pub fn write_tapp(path: &Path, packed: &Packed) -> anyhow::Result<()> {
    let temp = path.with_extension(format!("tapp.{}.tmp", std::process::id()));
    let result = (|| -> anyhow::Result<()> {
        let file = std::fs::File::create(&temp)?;
        let payload: &dyn BlobSource = packed;
        let mut out = tilo_app_package::tapp::write(
            std::io::BufWriter::new(file),
            &packed.meta,
            Some(payload),
        )?;
        out.flush()?;
        drop(out);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_app_package::PackageId;

    #[test]
    fn ids_are_reverse_dns_slugs_of_the_request() {
        assert_eq!(tool_id(&["pandoc".into()]), "so.tilo.tool.pandoc");
        assert_eq!(tool_id(&["gcc".into(), "make".into()]), "so.tilo.tool.gcc");
        assert_eq!(
            tool_id(&["/usr/bin/Python3".into()]),
            "so.tilo.tool.usr-bin-python3"
        );
        assert_eq!(tool_id(&[]), "so.tilo.tool.tool");
    }

    #[test]
    fn one_signer_sealing_one_key_gets_one_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        std::fs::write(dir.path().join("usr/bin/hello"), "hi").unwrap();
        let key = CacheKey::new(PackageId::from_bytes([1; 32]), ["hello-1-1.x86_64"]);
        let identity = SigningIdentity::from_seed(&[7; 32]);
        let requested = vec!["hello".to_owned()];
        let a = seal_tool(dir.path(), &key, &requested, Arch::X86_64, &identity).unwrap();
        let b = seal_tool(dir.path(), &key, &requested, Arch::X86_64, &identity).unwrap();
        assert_eq!(a.id(), b.id());
        let image = a.meta.manifest.image.as_ref().unwrap();
        assert_eq!(image.runtime, Some(key.runtime));
        assert_eq!(image.packages, key.nevras);
        assert_eq!(a.meta.manifest.package.kind, Kind::Tool);

        let other = CacheKey::new(PackageId::from_bytes([2; 32]), ["hello-1-1.x86_64"]);
        let c = seal_tool(dir.path(), &other, &requested, Arch::X86_64, &identity).unwrap();
        assert_ne!(a.id(), c.id(), "a tool names its runtime");
    }
}
