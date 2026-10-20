use super::{Result, fs, invalid, payload, validate};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;

pub fn digest(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}
pub fn preflight(source: &str, receipt: &Value) -> Result<()> {
    let parent = fs::directory(source)?;
    fs::protected(&parent, fs::uid(), false)?;
    let data = fs::ciphertext(&parent, fs::uid())?;
    validate::receipt(receipt, data.len() as u64, &digest(&data))?;
    fs::publish_json(&parent, fs::RECEIPT, receipt, fs::uid(), fs::gid())
}
pub fn profile(source: &str) -> Result<()> {
    let parent = match fs::directory(source) {
        Ok(fd) => fd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if fs::exists(&parent, "prepared-setup.error")? {
        return Err(invalid());
    }
    let mut expected = None;
    if fs::exists(&parent, "profile.json")? {
        let fd = fs::open(&parent, "profile.json", libc::O_RDONLY, 0)?;
        let bytes = fs::read_fd(&fd, 32768, Some(fs::uid()), 0o022)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let obj = value.as_object().ok_or_else(invalid)?;
        let fields = [
            "schema",
            "purpose",
            "hostname",
            "username",
            "full_name",
            "device_label",
            "locale",
            "keyboard",
            "timezone",
            "filesystem",
            "prepared_setup",
        ];
        if obj.keys().any(|k| !fields.contains(&k.as_str()))
            || value.get("schema").and_then(Value::as_u64) != Some(1)
            || value
                .get("purpose")
                .is_some_and(|v| !matches!(v.as_str(), Some("computer" | "server")))
        {
            return Err(invalid());
        }
        if let Some(label) = value.get("device_label") {
            let label = label.as_str().ok_or_else(invalid)?;
            if (!label.is_empty() && label.trim().is_empty())
                || label.len() > 64
                || label
                    .chars()
                    .any(|c| c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c))
            {
                return Err(invalid());
            }
        }
        if let Some(receipt) = value.get("prepared_setup") {
            if !receipt.is_object() {
                return Err(invalid());
            }
            expected = Some(receipt.clone());
        }
    }
    if let Some(value) = expected {
        preflight(source, &value)?;
    } else if fs::exists(&parent, fs::RECEIPT)? {
        let value = fs::receipt(&parent, Some(fs::uid()), true)?.ok_or_else(invalid)?;
        preflight(source, &value)?;
    } else if fs::exists(&parent, fs::FILE)? {
        return Err(invalid());
    }
    Ok(())
}
pub fn carry(
    source: &str,
    target: &str,
    components: &[&str],
    uid: u32,
    gid: u32,
) -> Result<String> {
    let src = fs::directory(source)?;
    fs::protected(&src, fs::uid(), false)?;
    let data = fs::ciphertext(&src, fs::uid())?;
    let receipt = fs::receipt(&src, Some(fs::uid()), true)?.ok_or_else(invalid)?;
    let digest = digest(&data);
    validate::receipt(&receipt, data.len() as u64, &digest)?;
    let destination = fs::chain(target, components, uid, gid)?;
    fs::verify_chain(target, components, &destination, uid)?;
    fs::publish_cipher(&destination, &data, uid, gid)?;
    fs::publish_json(&destination, fs::RECEIPT, &receipt, uid, gid)?;
    if fs::exists(&src, "tpm-handoff.json")? {
        let handoff =
            fs::json_required(&src, "tpm-handoff.json", 64 * 1024, Some(fs::uid()), true)?;
        fs::publish_json(&destination, "tpm-handoff.json", &handoff, uid, gid)?;
    }
    fs::verify_chain(target, components, &destination, uid)?;
    Ok(digest)
}
struct Candidate {
    fd: File,
    before: Metadata,
    receipt: Value,
}
fn source(root: &str) -> Result<Option<Candidate>> {
    let parent = fs::directory(root)?;
    if !fs::exists(&parent, fs::DIRECTORY)? {
        return Ok(None);
    }
    let child = fs::child(&parent, fs::DIRECTORY)?;
    if !fs::exists(&child, fs::FILE)? {
        if fs::exists(&child, fs::RECEIPT)? {
            return Err(invalid());
        }
        return Ok(None);
    }
    let mut fd = fs::open(&child, fs::FILE, libc::O_RDONLY, 0)?;
    let before = fd.metadata()?;
    if !before.is_file() || !(48..=fs::MAX).contains(&before.len()) {
        return Err(invalid());
    }
    let receipt = fs::receipt(&child, None, false)?;
    let Some(receipt) = receipt else {
        if before.len() != fs::MAX {
            return Err(invalid());
        }
        let mut buffer = vec![0; 65536].into_boxed_slice();
        loop {
            let n = fd.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            if buffer
                .get(..n)
                .ok_or_else(invalid)?
                .iter()
                .any(|b| *b != b' ')
            {
                return Err(invalid());
            }
        }
        if fs::snapshot(&before) != fs::snapshot(&fd.metadata()?) {
            return Err(invalid());
        }
        return Ok(None);
    };
    let size = receipt
        .get("sealed_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    validate::receipt(
        &receipt,
        size,
        receipt
            .get("ciphertext_sha256")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?,
    )?;
    let mut prefix = [0; 8];
    fd.read_exact(&mut prefix)?;
    fd.seek(SeekFrom::Start(0))?;
    if before.len() < size || &prefix != b"TILOMIG1" {
        return Err(invalid());
    }
    Ok(Some(Candidate {
        fd,
        before,
        receipt,
    }))
}
pub fn extract(roots: &[String], output: &str) -> Result<Option<Value>> {
    let parent = fs::directory(output)?;
    fs::protected(&parent, fs::uid(), false)?;
    let mut candidates = std::collections::BTreeMap::new();
    for root in roots {
        let Some(candidate) = source(root)? else {
            continue;
        };
        let identity = fs::identity(&candidate.before);
        if let Some(old) = candidates.get(&identity) {
            let old: &Candidate = old;
            if old.receipt != candidate.receipt {
                return Err(invalid());
            }
        } else {
            candidates.insert(identity, candidate);
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    if candidates.len() != 1 {
        return Err(invalid());
    }
    let candidate = candidates.values().next().ok_or_else(invalid)?;
    let size = candidate
        .receipt
        .get("sealed_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    let temp = fs::Temporary::new(&parent, fs::uid(), fs::gid())?;
    let mut total = 0;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 65536].into_boxed_slice();
    loop {
        let n = (&candidate.fd).read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let sealed =
            usize::try_from(size.saturating_sub(total).min(n as u64)).map_err(|_| invalid())?;
        total = total.checked_add(n as u64).ok_or_else(invalid)?;
        if total > candidate.before.len()
            || total > fs::MAX
            || buffer
                .get(sealed..n)
                .ok_or_else(invalid)?
                .iter()
                .any(|b| *b != b' ')
        {
            return Err(invalid());
        }
        let bytes = buffer.get(..sealed).ok_or_else(invalid)?;
        digest.update(bytes);
        (&temp.fd).write_all(bytes)?;
    }
    if total != candidate.before.len()
        || fs::snapshot(&candidate.before) != fs::snapshot(&candidate.fd.metadata()?)
    {
        return Err(invalid());
    }
    validate::receipt(&candidate.receipt, size, &hex::encode(digest.finalize()))?;
    temp.publish(fs::FILE)?;
    fs::publish_json(
        &parent,
        fs::RECEIPT,
        &candidate.receipt,
        fs::uid(),
        fs::gid(),
    )?;
    Ok(Some(candidate.receipt.clone()))
}
pub fn authenticated(source: &str, value: &Value) -> Result<()> {
    validate::authenticated_metadata(value)?;
    let expected = value.get("completed_home").filter(|v| !v.is_null());
    let apps = value.get("completed_applications").filter(|v| !v.is_null());
    let apps_only = value
        .get("completed_applications_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let shared = validate::shared_apps(value)?;
    if apps_only {
        if expected.is_some() || apps.is_none() {
            return Err(invalid());
        }
    } else if expected.is_none() {
        if apps.is_some() || value.get("completed_setup").is_some_and(|v| !v.is_null()) {
            return Err(invalid());
        }
        return Ok(());
    }
    if let Some(e) = expected {
        validate::expectation(e, "home")?;
    }
    let setup = value.get("completed_setup").ok_or_else(invalid)?;
    let parent = fs::directory(source)?;
    fs::protected(&parent, fs::uid(), false)?;
    let data = fs::ciphertext(&parent, fs::uid())?;
    validate::receipt(setup, data.len() as u64, &digest(&data))?;
    if fs::receipt(&parent, Some(fs::uid()), true)?.as_ref() != Some(setup) {
        return Err(invalid());
    }
    if (value.get("retained_home").is_some_and(|v| !v.is_null()) && !shared)
        || expected.is_some_and(|e| e.get("setup_binding") != value.get("setup_binding"))
    {
        return Err(invalid());
    }
    let actual = payload::staged(source, "home")?;
    if actual.as_ref().and_then(|v| v.get("expectation")) != expected {
        return Err(invalid());
    }
    let actual_apps = payload::staged(source, "applications")?;
    if let Some(e) = apps {
        validate::expectation(e, "applications")?;
        if e.get("setup_binding") != value.get("setup_binding")
            || expected.is_some_and(|home| e.get("operation") == home.get("operation"))
            || actual_apps.as_ref().and_then(|v| v.get("expectation")) != Some(e)
        {
            return Err(invalid());
        }
    } else if actual_apps.is_some() {
        return Err(invalid());
    }
    Ok(())
}
pub fn retain_binding(source: &str, target: &str, value: &Value) -> Result<()> {
    if fs::uid() != 0 {
        return Err(invalid());
    }
    authenticated(source, value)?;
    let person = value.get("identity_public_key").ok_or_else(invalid)?;
    let binding = value.get("setup_binding").ok_or_else(invalid)?;
    let src = fs::directory(source)?;
    let target_fd = fs::directory(target)?;
    fs::protected(&src, 0, false)?;
    fs::protected(&target_fd, 0, false)?;
    let receipt = fs::receipt(&src, Some(0), true)?.ok_or_else(invalid)?;
    validate::receipt(
        &receipt,
        fs::ciphertext(&src, 0)?.len() as u64,
        &digest(&fs::ciphertext(&src, 0)?),
    )?;
    let destination = fs::chain(target, &["lib", "tilo", "prepared-setup"], 0, 0)?;
    if fs::receipt(&destination, Some(0), true)?.as_ref() != Some(&receipt) {
        return Err(invalid());
    }
    let data = fs::ciphertext(&destination, 0)?;
    validate::receipt(&receipt, data.len() as u64, &digest(&data))?;
    let origin = json!({"schema":1,"identity_public_key":person,"setup_binding":binding,"sealed_bytes":receipt.get("sealed_bytes"),"ciphertext_sha256":receipt.get("ciphertext_sha256")});
    validate::origin(&origin)?;
    fs::publish_json(&destination, "first-use-origin.json", &origin, 0, 0)
}
pub struct Account {
    pub home: String,
    pub uid: u32,
    pub gid: u32,
}
pub fn account(username: &str) -> Result<Account> {
    if fs::uid() != 0 {
        return Err(invalid());
    }
    let name = std::ffi::CString::new(username).map_err(|_| invalid())?;
    let mut buffer = vec![0u8; 1024];
    loop {
        let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &raw mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < 1024 * 1024 {
            buffer.resize(buffer.len().checked_mul(2).ok_or_else(invalid)?, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return Err(invalid());
        }
        let record = unsafe { record.assume_init() };
        let home = unsafe { std::ffi::CStr::from_ptr(record.pw_dir) }
            .to_str()
            .map_err(|_| invalid())?
            .to_owned();
        return Ok(Account {
            home,
            uid: record.pw_uid,
            gid: record.pw_gid,
        });
    }
}
pub fn deliver_binding(account: &Account, digest: &str) -> Result<()> {
    let source = fs::directory("/var/lib/tilo/prepared-setup")?;
    let home = fs::directory(&account.home)?;
    let system = fs::directory("/var/lib/tilo")?;
    fs::protected(&source, 0, false)?;
    fs::protected(&system, 0, false)?;
    let origin = fs::json_required(&source, "first-use-origin.json", 8192, Some(0), true)?;
    validate::origin(&origin)?;
    let receipt = fs::receipt(&source, Some(0), true)?.ok_or_else(invalid)?;
    if origin.get("ciphertext_sha256").and_then(Value::as_str) != Some(digest)
        || origin.get("sealed_bytes") != receipt.get("sealed_bytes")
        || origin.get("ciphertext_sha256") != receipt.get("ciphertext_sha256")
    {
        return Err(invalid());
    }
    let m = fs::protected(&home, account.uid, false)?;
    if m.gid() != account.gid {
        return Err(invalid());
    }
    let binding = fs::private_child(&system, "prepared-first-use", 0, 0, false)?;
    fs::owner(&binding, 0, 0, 0o755)?;
    let mut public = origin;
    let obj = public.as_object_mut().ok_or_else(invalid)?;
    obj.insert("uid".into(), json!(account.uid));
    obj.insert("gid".into(), json!(account.gid));
    obj.insert("home".into(), json!(account.home));
    obj.insert("home_device".into(), json!(m.dev()));
    obj.insert("home_inode".into(), json!(m.ino()));
    let name = format!("{}.json", account.uid);
    if fs::exists(&binding, &name)? {
        if fs::json_required(&binding, &name, 8192, Some(0), false)? != public {
            return Err(invalid());
        }
    } else {
        fs::publish_json(&binding, &name, &public, 0, 0)?;
        let fd = fs::open(&binding, &name, libc::O_RDONLY, 0)?;
        fs::owner(&fd, 0, 0, 0o644)?;
        fd.sync_all()?;
    }
    binding.sync_all()?;
    system.sync_all()?;
    let now = home.metadata()?;
    if fs::snapshot(&m) != fs::snapshot(&now)
        || (m.uid(), m.gid(), m.mode()) != (now.uid(), now.gid(), now.mode())
    {
        return Err(invalid());
    }
    Ok(())
}
