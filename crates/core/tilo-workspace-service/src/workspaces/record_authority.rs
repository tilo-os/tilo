//! Which certificate a host serves its workspaces with: the one the records
//! under its directory name as their authority, when that is the host's own
//! device.

use std::path::Path;

use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::WorkspaceServices;

/// `host`, serving with the certificate the workspace records under `dir`
/// name, when they are the same device: the record's authority is the one the
/// workspace was made with, and every comparison a host makes for its
/// workspace is exact. A record of another device — or one too old to read —
/// leaves the host's own, and opening refuses it as before.
pub(super) fn host_serving(mut host: WorkspaceServices, dir: &Path) -> WorkspaceServices {
    let names = std::fs::read_dir(dir).ok().into_iter().flatten().flatten();
    for entry in names {
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "workspace")
        {
            continue;
        }
        let Ok(authority) = document_authority(&path) else {
            continue;
        };
        if authority.device_public_key == host.cert.device_public_key
            && authority.user_public_key == host.cert.user_public_key
            && authority.capabilities == host.cert.capabilities
        {
            host.cert = authority;
        }
        break;
    }
    host
}

/// The certificate a workspace record names as its authority, read from the
/// record's own bytes: the record starts with its version, the workspace's
/// UUID text, the workspace's id and its name, and the authority is the fifth
/// field.
fn document_authority(path: &Path) -> Result<DeviceCert, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let mut rest: &[u8] = &bytes;
    let (_, tail) = postcard::take_from_bytes::<u32>(rest).map_err(|error| error.to_string())?;
    rest = tail;
    let (_, tail) = postcard::take_from_bytes::<String>(rest).map_err(|error| error.to_string())?;
    rest = tail;
    let (_, tail) =
        postcard::take_from_bytes::<WorkspaceId>(rest).map_err(|error| error.to_string())?;
    rest = tail;
    let (_, tail) = postcard::take_from_bytes::<String>(rest).map_err(|error| error.to_string())?;
    rest = tail;
    let (cert, _) =
        postcard::take_from_bytes::<DeviceCert>(rest).map_err(|error| error.to_string())?;
    Ok(cert)
}
