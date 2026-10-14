use super::{Result, fs, invalid, media, validate};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::process::{Command, Stdio};

pub const KINDS: [&str; 3] = ["home", "applications", "browser"];
const ROOT: &str = "/run/tilo-installer";
const NATIVE: &str = "/usr/libexec/tilo-prepared-setup";
const SLOT: u64 = 130 * 1024 * 1024;
pub fn names(kind: &str) -> Result<(&str, &str, &str)> {
    match kind {
        "home" => Ok(("home-selection.json", "portable-home.json", "prepared-home")),
        "applications" => Ok((
            "applications-selection.json",
            "portable-applications.json",
            "prepared-applications",
        )),
        "browser" => Ok((
            "browser-selection.json",
            "portable-browser.json",
            "prepared-browser",
        )),
        _ => Err(invalid()),
    }
}
pub fn selected() -> Result<String> {
    let data = media::command(NATIVE, &["--selected-install-generation"], 257, 30)?;
    let text = std::str::from_utf8(&data).map_err(|_| invalid())?;
    let path = text.strip_suffix('\n').ok_or_else(invalid)?;
    if path.contains('\n')
        || (path != ROOT
            && completed(
                path.strip_prefix(&format!("{ROOT}/.completed-setup-"))
                    .ok_or_else(invalid)?,
            )? != path)
    {
        return Err(invalid());
    }
    Ok(path.to_owned())
}
fn completed(attempt: &str) -> Result<String> {
    let id = uuid::Uuid::parse_str(attempt).map_err(|_| invalid())?;
    if id.to_string() != attempt {
        return Err(invalid());
    }
    Ok(format!("{ROOT}/.completed-setup-{attempt}"))
}
pub fn validate_completed(source: &str, binding: &Value) -> Result<()> {
    let obj = binding.as_object().ok_or_else(invalid)?;
    if obj.len() != 2
        || binding.get("kind").and_then(Value::as_str) != Some("completed_generation")
        || completed(
            binding
                .get("attempt")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?,
        )? != source
        || selected()? != source
    {
        return Err(invalid());
    }
    Ok(())
}
pub fn staged(source: &str, kind: &str) -> Result<Option<Value>> {
    let (_, locator, _) = names(kind)?;
    let fd = match fs::directory(source) {
        Ok(fd) => fd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !fs::exists(&fd, locator)? {
        return Ok(None);
    }
    fs::protected(&fd, fs::uid(), false)?;
    let value = fs::json_required(&fd, locator, 32768, Some(fs::uid()), true)?;
    let keys = [
        "schema",
        "binding",
        "expectation",
        "source_identity",
        "prepared_setup",
    ];
    let object = value.as_object().ok_or_else(invalid)?;
    if object.len() != keys.len()
        || !keys.iter().all(|k| object.contains_key(*k))
        || value.get("schema").and_then(Value::as_u64) != Some(1)
        || value
            .get("source_identity")
            .and_then(Value::as_array)
            .is_none_or(|a| a.len() != 5 || a.iter().any(|n| n.as_u64().is_none()))
    {
        return Err(invalid());
    }
    validate::expectation(value.get("expectation").ok_or_else(invalid)?, kind)?;
    if fs::receipt(&fd, Some(fs::uid()), true)?.as_ref() != value.get("prepared_setup") {
        return Err(invalid());
    }
    let binding = value.get("binding").ok_or_else(invalid)?;
    if binding.get("kind").and_then(Value::as_str) == Some("completed_generation") {
        validate_completed(source, binding)?;
    }
    Ok(Some(value))
}
pub fn package(parent: &File, expected: &Value, own: Option<u32>) -> Result<(File, Metadata)> {
    let filename = expected
        .get("filename")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let (mut fd, fixed) = match fs::open(parent, filename, libc::O_RDONLY, 0) {
        Ok(fd) => (fd, false),
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && own.is_none()
                && expected.get("purpose").and_then(Value::as_str) == Some("browser_profiles") =>
        {
            (
                fs::open(
                    &fs::child(parent, fs::DIRECTORY)?,
                    "browser.sealed",
                    libc::O_RDONLY,
                    0,
                )?,
                true,
            )
        }
        Err(e) => return Err(e),
    };
    let m = fd.metadata()?;
    let size = expected
        .get("sealed_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    let mut prefix = [0; 8];
    fd.read_exact(&mut prefix)?;
    fd.seek(SeekFrom::Start(0))?;
    if !m.is_file()
        || m.len() != if fixed { SLOT } else { size }
        || own.is_some_and(|o| m.uid() != o || m.mode() & 0o077 != 0)
        || &prefix != b"TILOMIG1"
    {
        return Err(invalid());
    }
    Ok((fd, m))
}
fn digest_expected(expected: &Value) -> Result<Vec<u8>> {
    expected
        .get("ciphertext_sha256")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?
        .iter()
        .map(|n| {
            n.as_u64()
                .and_then(|v| u8::try_from(v).ok())
                .ok_or_else(invalid)
        })
        .collect()
}
pub fn stream(
    fd: &File,
    before: &Metadata,
    expected: &Value,
    mut out: Option<&File>,
) -> Result<()> {
    (&*fd).seek(SeekFrom::Start(0))?;
    let sealed = expected
        .get("sealed_bytes")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    let fixed = expected.get("purpose").and_then(Value::as_str) == Some("browser_profiles")
        && before.len() == SLOT;
    let mut remaining = sealed;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(65536)).map_err(|_| invalid())?;
        let n = (&*fd).read(buffer.get_mut(..limit).ok_or_else(invalid)?)?;
        if n == 0 {
            return Err(invalid());
        }
        let bytes = buffer.get(..n).ok_or_else(invalid)?;
        digest.update(bytes);
        if let Some(dest) = out.as_mut() {
            dest.write_all(bytes)?;
        }
        remaining -= n as u64;
    }
    if fixed {
        let mut tail = before.len().checked_sub(sealed).ok_or_else(invalid)?;
        while tail > 0 {
            let limit = usize::try_from(tail.min(65536)).map_err(|_| invalid())?;
            let n = (&*fd).read(buffer.get_mut(..limit).ok_or_else(invalid)?)?;
            if n == 0
                || buffer
                    .get(..n)
                    .ok_or_else(invalid)?
                    .iter()
                    .any(|b| *b != b' ')
            {
                return Err(invalid());
            }
            tail -= n as u64;
        }
    }
    if (&*fd).read(buffer.get_mut(..1).ok_or_else(invalid)?)? != 0
        || fs::snapshot(before) != fs::snapshot(&fd.metadata()?)
        || digest.finalize().as_slice() != digest_expected(expected)?
    {
        return Err(invalid());
    }
    Ok(())
}
fn stream_completed(
    source: &str,
    binding: &Value,
    expected: &Value,
    kind: &str,
    mut out: Option<&File>,
) -> Result<()> {
    names(kind)?;
    validate_completed(source, binding)?;
    let mut child = Command::new(NATIVE)
        .args(["--stream-completed-payload", kind])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut pipe = child.stdout.take().ok_or_else(invalid)?;
    let result = (|| {
        let size = expected
            .get("sealed_bytes")
            .and_then(Value::as_u64)
            .ok_or_else(invalid)?;
        let mut total = 0;
        let mut digest = Sha256::new();
        let mut prefix: Vec<u8> = Vec::new();
        let mut buffer = [0; 65536];
        loop {
            let limit = usize::try_from((size - total + 1).min(65536)).map_err(|_| invalid())?;
            let n = pipe.read(buffer.get_mut(..limit).ok_or_else(invalid)?)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > size {
                return Err(invalid());
            }
            let bytes = buffer.get(..n).ok_or_else(invalid)?;
            prefix.extend(bytes.iter().take(8 - prefix.len()));
            digest.update(bytes);
            if let Some(dest) = out.as_mut() {
                dest.write_all(bytes)?;
            }
        }
        if total != size
            || prefix != b"TILOMIG1"
            || digest.finalize().as_slice() != digest_expected(expected)?
        {
            return Err(invalid());
        }
        Ok(())
    })();
    // Closing the reader signals BrokenPipe, but never kills the issuer before
    // its authenticated mount leases retire. EOF alone is not custody success.
    drop(pipe);
    let status = child.wait()?;
    result?;
    if !status.success() {
        return Err(invalid());
    }
    validate_completed(source, binding)
}
pub fn input(source: &str, value: &Value, kind: &str, out: Option<&File>) -> Result<()> {
    let binding = value.get("binding").ok_or_else(invalid)?;
    let expected = value.get("expectation").ok_or_else(invalid)?;
    if binding.get("kind").and_then(Value::as_str) == Some("completed_generation") {
        return stream_completed(source, binding, expected, kind, out);
    }
    if source.starts_with(&format!("{ROOT}/.completed-setup-")) {
        return Err(invalid());
    }
    media::with_root(binding, |root| {
        let (fd, m) = package(root, expected, None)?;
        let snapshot = serde_json::to_value(fs::snapshot(&m)).map_err(|_| invalid())?;
        if value.get("source_identity") != Some(&snapshot) {
            return Err(invalid());
        }
        stream(&fd, &m, expected, out)
    })
}
pub fn required(expected: &Value, kind: &str) -> Result<u128> {
    let n = |k| expected.get(k).and_then(Value::as_u64).ok_or_else(invalid);
    let meta = expected
        .get("limits")
        .and_then(|v| v.get("metadata_bytes"))
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    Ok(u128::from(n("sealed_bytes")?)
        + u128::from(n("plaintext_bytes")?) * if kind == "applications" { 2 } else { 1 }
        + u128::from(meta)
        + u128::from(n("entries")?) * 8192
        + 16 * 1024 * 1024)
}
pub fn retain(source: &str, target: &str, kind: &str) -> Result<()> {
    let Some(value) = staged(source, kind)? else {
        return Ok(());
    };
    let (_, _, custody) = names(kind)?;
    let expected = value.get("expectation").ok_or_else(invalid)?;
    let components = ["lib", "tilo", custody];
    let destination = fs::chain(target, &components, fs::uid(), fs::gid())?;
    let filename = expected
        .get("filename")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if fs::exists(&destination, filename)? {
        let (fd, m) = package(&destination, expected, Some(fs::uid()))?;
        stream(&fd, &m, expected, None)?;
        input(source, &value, kind, None)?;
    } else {
        if u128::from(fs::capacity(&destination)?.0) < required(expected, kind)? {
            return Err(invalid());
        }
        let temp = fs::Temporary::new(&destination, fs::uid(), fs::gid())?;
        input(source, &value, kind, Some(&temp.fd))?;
        temp.fd.sync_all()?;
        stream(&temp.fd, &temp.fd.metadata()?, expected, None)?;
        temp.publish(filename)?;
    }
    fs::verify_chain(target, &components, &destination, fs::uid())?;
    fs::publish_json(
        &destination,
        "expectation.json",
        expected,
        fs::uid(),
        fs::gid(),
    )
}
pub fn deliver_to(source: &str, home: &str, uid: u32, gid: u32, kind: &str) -> Result<Value> {
    let (_, _, custody) = names(kind)?;
    let source_fd = fs::directory(source)?;
    fs::protected(&source_fd, fs::uid(), true)?;
    let components = [".local", "state", "tilo", custody];
    let destination = fs::chain(home, &components, uid, gid)?;
    let expected = fs::json_required(&source_fd, "expectation.json", 16384, Some(fs::uid()), true)?;
    validate::expectation(&expected, kind)?;
    if source_fd.metadata()?.dev() != destination.metadata()?.dev() {
        return Err(invalid());
    }
    let filename = expected
        .get("filename")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if fs::exists(&destination, filename)? {
        let (fd, m) = package(&destination, &expected, None)?;
        if m.mode() & 0o077 != 0 || (m.uid() != uid && m.uid() != fs::uid()) {
            return Err(invalid());
        }
        stream(&fd, &m, &expected, None)?;
        if m.uid() != uid {
            let original = fs::open(&source_fd, filename, libc::O_RDONLY, 0)?;
            if fs::identity(&original.metadata()?) != fs::identity(&m) {
                return Err(invalid());
            }
            fs::owner(&fd, uid, gid, 0o600)?;
            fd.sync_all()?;
        }
    } else {
        let (fd, m) = package(&source_fd, &expected, None)?;
        if m.mode() & 0o077 != 0 || (m.uid() != uid && m.uid() != fs::uid()) {
            return Err(invalid());
        }
        stream(&fd, &m, &expected, None)?;
        fs::link(&source_fd, filename, &destination, filename)?;
        fs::owner(&fd, uid, gid, 0o600)?;
        fd.sync_all()?;
    }
    fs::verify_chain(home, &components, &destination, uid)?;
    fs::publish_json(&destination, "expectation.json", &expected, uid, gid)?;
    let (fd, m) = package(&destination, &expected, Some(uid))?;
    stream(&fd, &m, &expected, None)?;
    destination.sync_all()?;
    fs::verify_chain(home, &components, &destination, uid)?;
    if fs::identity(&fs::directory(source)?.metadata()?) != fs::identity(&source_fd.metadata()?) {
        return Err(invalid());
    }
    let received = fs::open(&destination, filename, libc::O_RDONLY, 0)?;
    if fs::identity(&received.metadata()?) != fs::identity(&m) {
        return Err(invalid());
    }
    if fs::exists(&source_fd, filename)? {
        let original = fs::open(&source_fd, filename, libc::O_RDONLY, 0)?;
        if fs::identity(&original.metadata()?) != fs::identity(&received.metadata()?) {
            return Err(invalid());
        }
        fs::unlink(&source_fd, filename)?;
        source_fd.sync_all()?;
    }
    Ok(expected)
}
fn unused_browser(root: &File) -> Result<()> {
    let child = fs::child(root, fs::DIRECTORY)?;
    if fs::json(
        &child,
        "browser-receipt.json",
        4096,
        None,
        false,
        true,
        true,
    )?
    .is_some()
    {
        return Err(invalid());
    }
    let fd = fs::open(&child, "browser.sealed", libc::O_RDONLY, 0)?;
    let m = fd.metadata()?;
    if !m.is_file() || m.len() != SLOT {
        return Err(invalid());
    }
    let mut buf = [0; 65536];
    loop {
        let n = (&fd).read(&mut buf)?;
        if n == 0 {
            break;
        }
        if buf.get(..n).ok_or_else(invalid)?.iter().any(|b| *b != b' ') {
            return Err(invalid());
        }
    }
    if fs::snapshot(&m) != fs::snapshot(&fd.metadata()?) {
        return Err(invalid());
    }
    Ok(())
}
pub fn stage(
    roots: &[String],
    output: &str,
    prepared: &Value,
    devices: &[String],
    kind: &str,
) -> Result<()> {
    let (selection, locator, _) = names(kind)?;
    let mut identities = std::collections::BTreeSet::new();
    let mut unique = Vec::new();
    for root in roots {
        let fd = fs::directory(root)?;
        if identities.insert(fs::identity(&fd.metadata()?)) {
            unique.push(root);
        }
    }
    let mut selections = Vec::new();
    for root in &unique {
        let fd = fs::directory(root)?;
        if !fs::exists(&fd, fs::DIRECTORY)? {
            continue;
        }
        let child = fs::child(&fd, fs::DIRECTORY)?;
        if !fs::exists(&child, selection)? {
            if kind == "browser"
                && (fs::exists(&child, "browser-receipt.json")?
                    || fs::exists(&child, "browser.sealed")?)
            {
                return Err(invalid());
            }
            continue;
        }
        let value = fs::json(
            &child,
            selection,
            if kind == "browser" { 4096 } else { 18432 },
            None,
            false,
            kind == "browser",
            false,
        )?;
        let Some(value) = value else {
            unused_browser(&fd)?;
            continue;
        };
        let obj = value.as_object().ok_or_else(invalid)?;
        if obj.len() != 3
            || value.get("schema").and_then(Value::as_u64) != Some(1)
            || value.get("prepared_setup") != Some(prepared)
            || !obj.contains_key(kind)
        {
            return Err(invalid());
        }
        let expected = value.get(kind).ok_or_else(invalid)?;
        validate::expectation(expected, kind)?;
        selections.push((media::binding(root, devices)?, expected.clone()));
    }
    if selections.is_empty() {
        return Ok(());
    }
    if selections.len() != 1 {
        return Err(invalid());
    }
    let (selected, expected) = selections.first().ok_or_else(invalid)?;
    let mut found = Vec::new();
    for root in &unique {
        let fd = fs::directory(root)?;
        let receipt_name = format!(
            "{}.receipt.json",
            expected
                .get("operation")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?
        );
        let receipt = if fs::exists(&fd, &receipt_name)? {
            Some(fs::json_required(&fd, &receipt_name, 16384, None, false)?)
        } else if kind == "browser" && fs::exists(&fd, fs::DIRECTORY)? {
            let child = fs::child(&fd, fs::DIRECTORY)?;
            if !fs::exists(&child, "browser-receipt.json")? {
                continue;
            }
            let v = fs::json(
                &child,
                "browser-receipt.json",
                4096,
                None,
                false,
                true,
                true,
            )?;
            if v.is_some()
                && fs::exists(
                    &fd,
                    expected
                        .get("filename")
                        .and_then(Value::as_str)
                        .ok_or_else(invalid)?,
                )?
            {
                return Err(invalid());
            }
            v
        } else {
            None
        };
        let Some(receipt) = receipt else {
            continue;
        };
        if receipt != *expected {
            return Err(invalid());
        }
        let binding = media::binding(root, devices)?;
        if !devices.is_empty() && binding.get("disk_identity") != selected.get("disk_identity") {
            return Err(invalid());
        }
        let (_, m) = package(&fd, expected, None)?;
        found.push(json!({"schema":1,"binding":binding,"expectation":expected,"source_identity":fs::snapshot(&m),"prepared_setup":prepared}));
    }
    if found.len() != 1 {
        return Err(invalid());
    }
    fs::publish_json(
        &fs::directory(output)?,
        locator,
        found.first().ok_or_else(invalid)?,
        fs::uid(),
        fs::gid(),
    )
}
