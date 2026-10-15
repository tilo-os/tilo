//! Optional capture-only GNU tar authority, held as sealed signed bytes.
use super::{CaptureAdmission, executable};
use std::{
    collections::{HashSet, VecDeque},
    fs::File,
    io,
    os::fd::{AsRawFd, RawFd},
    path::Path,
};
const TAR_FD: RawFd = 512;
const LIB_PATH: &str = "/run/tilo/migration/tools/lib";
pub(super) struct Archive {
    pub(super) tar: File,
    pub(super) libraries: Vec<(String, File)>,
}
impl Archive {
    pub(super) fn admit(admission: &CaptureAdmission) -> io::Result<Self> {
        let bundle = runtime_bundle::verify_bundle_with(
            &admission.runtime,
            &runtime_bundle::VerifyOptions {
                require_publisher: Some(admission.publisher.clone()),
                require_signature: true,
                require_arch: Some(std::env::consts::ARCH.into()),
            },
        )
        .map_err(io::Error::other)?;
        if bundle.root_hash != admission.runtime_root_hash {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let tar = executable::pin(&admission.runtime, &bundle, "bin/tar")?;
        let tar_path = format!("/proc/self/fd/{}", tar.as_raw_fd());
        let loader_path = format!("/proc/self/fd/{}", admission.bwrap.as_raw_fd());
        let interpreter = crate::loader_preflight::read_interpreter(Path::new(&tar_path))?
            .ok_or_else(|| io::Error::other("Signed archive helper has no interpreter"))?;
        if crate::loader_preflight::read_interpreter(Path::new(&loader_path))?.as_deref()
            != Some(interpreter.as_str())
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let initial = crate::loader_preflight::read_dynamic_section(Path::new(&tar_path))?
            .ok_or_else(|| io::Error::other("Signed archive helper is not a dynamic ELF"))?;
        let mut pending = VecDeque::from(initial.needed);
        let mut seen = HashSet::new();
        let mut libraries = Vec::new();
        let mut total = tar.metadata()?.len();
        while let Some(name) = pending.pop_front() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if seen.len() > 256
                || name.is_empty()
                || Path::new(&name).components().count() != 1
                || !matches!(
                    Path::new(&name).components().next(),
                    Some(std::path::Component::Normal(_))
                )
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let file =
                executable::pin_file(&admission.runtime, &bundle, &format!("lib/{name}"), false)?;
            total = total
                .checked_add(file.metadata()?.len())
                .ok_or(io::ErrorKind::InvalidData)?;
            if total > 128 * 1024 * 1024 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let path = format!("/proc/self/fd/{}", file.as_raw_fd());
            let dynamic = crate::loader_preflight::read_dynamic_section(Path::new(&path))?
                .ok_or_else(|| {
                    io::Error::other("Signed archive dependency is not a dynamic ELF")
                })?;
            pending.extend(dynamic.needed);
            libraries.push((name, file));
        }
        Ok(Self { tar, libraries })
    }
    pub(super) fn descriptor_pairs(&self) -> Vec<(RawFd, RawFd)> {
        let mut pairs = vec![(self.tar.as_raw_fd(), TAR_FD)];
        pairs.extend(
            self.libraries
                .iter()
                .zip(TAR_FD + 1..)
                .map(|((_, file), fd)| (file.as_raw_fd(), fd)),
        );
        pairs
    }
    pub(super) fn splice(
        &self,
        argv: &mut Vec<String>,
        ro_paths: &mut Vec<String>,
        original: bool,
    ) {
        let mut mounts = vec![
            "--dir".into(),
            LIB_PATH.into(),
            "--perms".into(),
            if original { "0555" } else { "0500" }.into(),
            "--ro-bind-data".into(),
            TAR_FD.to_string(),
            "/usr/bin/tar".into(),
        ];
        for ((name, _), fd) in self.libraries.iter().zip(TAR_FD + 1..) {
            mounts.extend([
                "--perms".into(),
                if original { "0444" } else { "0400" }.into(),
                "--ro-bind-data".into(),
                fd.to_string(),
                format!("{LIB_PATH}/{name}"),
            ]);
        }
        argv.splice(0..0, mounts);
        ro_paths.extend(["/usr/bin/tar".into(), LIB_PATH.into()]);
    }
}
