//! Absent-only firstboot defaults with durable, descriptor-anchored provenance.
//!
//! Existing files are never adopted as defaults, even after an interrupted run.
//! A receipt is an authority to replace files later, so verification deliberately
//! fails closed on changed metadata, links, custody paths and oversized input.
#[cfg(target_os = "linux")]
mod linux {
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::collections::HashSet;
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::fmt::Write as _;
    use std::fs::{self, File, Metadata};
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};

    const LIMIT: u64 = 4 << 20;
    const DIRECTORY: i32 = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    fn invalid<T>(message: &str) -> Result<T> {
        Err(message.into())
    }

    fn cstring(value: &OsStr) -> Result<CString> {
        Ok(CString::new(value.as_bytes())?)
    }

    fn syscall(result: i32) -> io::Result<i32> {
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result)
        }
    }

    fn open_at(directory: &File, name: &OsStr, flags: i32) -> Result<File> {
        let name = cstring(name)?;
        // SAFETY: name is NUL terminated; the returned descriptor is uniquely owned.
        let fd = syscall(unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        // SAFETY: openat returned a new descriptor, now owned by File.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_directory(path: &Path) -> Result<File> {
        let path = cstring(path.as_os_str())?;
        // SAFETY: path is NUL terminated; open returns a new descriptor.
        let fd = syscall(unsafe { libc::open(path.as_ptr(), DIRECTORY) })?;
        // SAFETY: the successful open descriptor has no other owner.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn metadata_at(directory: &File, name: &OsStr) -> Result<Option<Metadata>> {
        // The proc descriptor alias anchors lstat to this directory, without
        // following the final component. No user-supplied proc path is accepted.
        let path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
        match fs::symlink_metadata(path) {
            Ok(info) => Ok(Some(info)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn trusted(info: &Metadata) -> Result<()> {
        if info.uid() != 0 || info.mode() & 0o022 != 0 || info.file_type().is_symlink() {
            return invalid("firstboot seed authority is not root controlled");
        }
        Ok(())
    }

    fn stamp(info: &Metadata) -> Value {
        json!({"inode": info.ino(), "uid": info.uid(), "mode": info.mode() & 0o7777,
        "bytes": info.len(), "mtime_ns": i128::from(info.mtime()).saturating_mul(1_000_000_000).saturating_add(i128::from(info.mtime_nsec())),
        "ctime_ns": i128::from(info.ctime()).saturating_mul(1_000_000_000).saturating_add(i128::from(info.ctime_nsec()))})
    }

    fn chown(file: &File, uid: u32, gid: u32) -> Result<()> {
        // SAFETY: a live descriptor is passed; no borrowed memory is retained.
        syscall(unsafe { libc::fchown(file.as_raw_fd(), uid, gid) })?;
        Ok(())
    }

    fn chmod(file: &File, mode: u32) -> Result<()> {
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    fn mkdir(directory: &File, name: &OsStr, mode: u32) -> Result<()> {
        let name = cstring(name)?;
        // SAFETY: name is NUL terminated and directory is live.
        syscall(unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), mode) })?;
        Ok(())
    }

    fn filesystem(home: &File) -> Result<u64> {
        let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: fstatvfs initializes info on success.
        syscall(unsafe { libc::fstatvfs(home.as_raw_fd(), info.as_mut_ptr()) })?;
        // SAFETY: success above initialized every field.
        Ok(unsafe { info.assume_init() }.f_fsid)
    }

    fn custody(path: &Path) -> bool {
        path.starts_with(".config/tilo") || path.starts_with(".local/state/tilo")
    }

    fn bounded_read(file: &mut File) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut data)?;
        Ok(data)
    }

    fn digest(data: &[u8]) -> String {
        Sha256::digest(data)
            .iter()
            .fold(String::new(), |mut text, byte| {
                let _ = write!(text, "{byte:02x}");
                text
            })
    }

    /// Python's filesystem codec uses surrogateescape for non-UTF-8 names. Keep
    /// those bytes in receipts, rather than replacing, omitting or adopting them.
    fn json_path(path: &OsStr) -> Result<String> {
        let mut remaining = path.as_bytes();
        let mut output = String::from("\"");
        while !remaining.is_empty() {
            let (valid, bad) = match std::str::from_utf8(remaining) {
                Ok(_) => (remaining.len(), 0),
                Err(error) => (
                    error.valid_up_to(),
                    error
                        .error_len()
                        .unwrap_or_else(|| remaining.len().saturating_sub(error.valid_up_to())),
                ),
            };
            let text = serde_json::to_string(std::str::from_utf8(
                remaining.get(..valid).ok_or("invalid path encoding")?,
            )?)?;
            output.push_str(
                text.strip_prefix('"')
                    .and_then(|text| text.strip_suffix('"'))
                    .ok_or("invalid path encoding")?,
            );
            let rest = remaining.get(valid..).ok_or("invalid path encoding")?;
            for byte in rest.get(..bad).ok_or("invalid path encoding")? {
                write!(output, "\\udc{byte:02x}")?;
            }
            remaining = rest.get(bad..).ok_or("invalid path encoding")?;
        }
        output.push('"');
        Ok(output)
    }

    fn escaped_codepoint(data: &[u8]) -> Option<u16> {
        let escape = data.get(..6)?.strip_prefix(b"\\u")?;
        u16::from_str_radix(std::str::from_utf8(escape).ok()?, 16).ok()
    }

    /// serde rejects unpaired surrogates even though existing Python receipts can
    /// contain them. Substitute collision-free tokens before parsing, then recover
    /// raw filesystem bytes only in paths. Literal backslashes and genuine Unicode
    /// surrogate pairs are left intact. No permissive JSON parser is introduced.
    fn parse_receipt(data: &[u8]) -> Result<(Value, String)> {
        let marker = loop {
            let candidate = format!("tilo-surrogate-{}-", uuid::Uuid::new_v4().simple());
            if !data
                .windows(candidate.len())
                .any(|part| part == candidate.as_bytes())
            {
                break candidate;
            }
        };
        let mut sanitized = Vec::with_capacity(data.len());
        let mut remaining = data;
        while let Some((&first, rest)) = remaining.split_first() {
            if first == b'\\' {
                if let Some(code) = escaped_codepoint(remaining) {
                    let tail = remaining.get(6..).ok_or("malformed firstboot seed proof")?;
                    if (0xd800..=0xdbff).contains(&code)
                        && escaped_codepoint(tail)
                            .is_some_and(|low| (0xdc00..=0xdfff).contains(&low))
                    {
                        sanitized.extend_from_slice(
                            remaining
                                .get(..12)
                                .ok_or("malformed firstboot seed proof")?,
                        );
                        remaining = remaining
                            .get(12..)
                            .ok_or("malformed firstboot seed proof")?;
                        continue;
                    }
                    if (0xd800..=0xdfff).contains(&code) {
                        sanitized.extend_from_slice(format!("{marker}{code:04x}").as_bytes());
                        remaining = tail;
                        continue;
                    }
                }
                if let Some(pair) = remaining.get(..2) {
                    sanitized.extend_from_slice(pair);
                    remaining = remaining.get(2..).ok_or("malformed firstboot seed proof")?;
                    continue;
                }
            }
            sanitized.push(first);
            remaining = rest;
        }
        Ok((serde_json::from_slice(&sanitized)?, marker))
    }

    fn decode_receipt_path(path: &str, marker: &str) -> Result<OsString> {
        let mut output = Vec::new();
        let mut remaining = path;
        while let Some((prefix, rest)) = remaining.split_once(marker) {
            output.extend_from_slice(prefix.as_bytes());
            remaining = rest;
            let code = remaining
                .get(..4)
                .and_then(|value| u16::from_str_radix(value, 16).ok())
                .ok_or("unsafe firstboot seed proof path")?;
            if !(0xdc80..=0xdcff).contains(&code) {
                return invalid("unsafe firstboot seed proof path");
            }
            output.push(u8::try_from(code.saturating_sub(0xdc00))?);
            remaining = remaining
                .get(4..)
                .ok_or("unsafe firstboot seed proof path")?;
        }
        output.extend_from_slice(remaining.as_bytes());
        Ok(OsString::from_vec(output))
    }

    fn encode_receipt(home: &File, uid: u32, files: &[FileProof]) -> Result<Vec<u8>> {
        let mut encoded = format!(
            "{{\"schema\":1,\"home_inode\":{},\"filesystem\":{},\"uid\":{uid},\"files\":[",
            home.metadata()?.ino(),
            filesystem(home)?
        );
        for (index, entry) in files.iter().enumerate() {
            if index != 0 {
                encoded.push(',');
            }
            write!(
                encoded,
                "{{\"path\":{},\"stamp\":{},\"sha256\":\"{}\"}}",
                json_path(entry.path.as_os_str())?,
                entry.stamp,
                entry.sha256
            )?;
        }
        encoded.push_str("]}\n");
        Ok(encoded.into_bytes())
    }

    fn verify_receipt(proofs: &File, name: &OsStr, home: &File, uid: u32) -> Result<()> {
        let info = metadata_at(proofs, name)?.ok_or("missing firstboot seed proof")?;
        trusted(&info)?;
        if !info.is_file() || info.nlink() != 1 || info.len() > LIMIT {
            return invalid("unsafe firstboot seed proof");
        }
        let mut receipt = open_at(
            proofs,
            name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )?;
        let data = bounded_read(&mut receipt)?;
        let (body, surrogate_marker) = parse_receipt(&data)?;
        if stamp(&receipt.metadata()?) != stamp(&info) {
            return invalid("firstboot seed proof changed");
        }
        let files = body.get("files").and_then(Value::as_array);
        if !body.is_object()
            || body.get("schema") != Some(&json!(1))
            || body.get("uid") != Some(&json!(uid))
            || body.get("home_inode") != Some(&json!(home.metadata()?.ino()))
            || body.get("filesystem") != Some(&json!(filesystem(home)?))
            || files.is_none_or(|files| files.len() > 4096)
        {
            return invalid("firstboot seed proof does not match home");
        }
        let mut seen = HashSet::new();
        for entry in files.ok_or("firstboot seed proof does not match home")? {
            let Some(path) = entry.get("path").and_then(Value::as_str) else {
                return invalid("malformed firstboot seed proof");
            };
            let decoded = decode_receipt_path(path, &surrogate_marker)?;
            let relative = Path::new(&decoded);
            let parts: Vec<_> = relative.components().collect();
            // Path components normalize repeated separators and trailing slashes;
            // compare the spelling as well as rejecting absolute and parent paths.
            let normalized = parts
                .iter()
                .map(|part| part.as_os_str())
                .collect::<PathBuf>();
            if parts.is_empty()
                || parts
                    .iter()
                    .any(|part| !matches!(part, Component::Normal(_)))
                || normalized.as_os_str() != decoded
                || !seen.insert(decoded.clone())
                || custody(relative)
            {
                return invalid("unsafe firstboot seed proof path");
            }
            let (name, parents) = parts
                .split_last()
                .ok_or("unsafe firstboot seed proof path")?;
            let mut directory = home.try_clone()?;
            for part in parents {
                directory = open_at(&directory, part.as_os_str(), DIRECTORY)?;
            }
            let mut file = open_at(
                &directory,
                name.as_os_str(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )?;
            let before = file.metadata()?;
            if !before.is_file()
                || before.nlink() != 1
                || before.uid() != uid
                || before.len() > LIMIT
                || entry.get("stamp") != Some(&stamp(&before))
            {
                return invalid("attested firstboot default changed");
            }
            let data = bounded_read(&mut file)?;
            if stamp(&file.metadata()?) != stamp(&before)
                || entry.get("sha256") != Some(&json!(digest(&data)))
            {
                return invalid("attested firstboot default changed");
            }
        }
        Ok(())
    }

    /// Publish one complete inode, without replacement, then durably remove scratch.
    /// Never unlink the final name: after publication it may already be user data.
    fn publish(
        directory: &File,
        name: &OsStr,
        writer: impl FnOnce(&OsStr) -> Result<()>,
    ) -> Result<()> {
        let temporary =
            OsString::from(format!(".tilo-firstboot-{}", uuid::Uuid::new_v4().simple()));
        let result = (|| {
            writer(&temporary)?;
            let old = cstring(&temporary)?;
            let new = cstring(name)?;
            // SAFETY: both names are NUL terminated and relative to a live directory.
            syscall(unsafe {
                libc::renameat2(
                    directory.as_raw_fd(),
                    old.as_ptr(),
                    directory.as_raw_fd(),
                    new.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            })?;
            directory.sync_all()?;
            Ok(())
        })();
        let temporary = cstring(&temporary)?;
        // SAFETY: the scratch name belongs to this operation in a live directory.
        let cleanup = unsafe { libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
        if cleanup < 0 && io::Error::last_os_error().kind() != io::ErrorKind::NotFound {
            return Err(io::Error::last_os_error().into());
        }
        directory.sync_all()?;
        result
    }

    struct Owner {
        uid: u32,
        gid: u32,
    }

    fn publish_bytes(
        directory: &File,
        name: &OsStr,
        data: &[u8],
        mode: u32,
        owner: &Owner,
    ) -> Result<()> {
        publish(directory, name, |temporary| {
            let mut file = open_at(
                directory,
                temporary,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            )?;
            file.write_all(data)?;
            chown(&file, owner.uid, owner.gid)?;
            chmod(&file, mode)?;
            file.sync_all()?;
            Ok(())
        })
    }

    fn readlink(directory: &File, name: &OsStr) -> Result<OsString> {
        let name = cstring(name)?;
        let mut buffer = vec![0_u8; 4097];
        // SAFETY: buffer is writable for its length, and name is NUL terminated.
        let length = unsafe {
            libc::readlinkat(
                directory.as_raw_fd(),
                name.as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if length < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if length > 4096 {
            return invalid("firstboot skeleton link exceeds bounds");
        }
        buffer.truncate(usize::try_from(length)?);
        Ok(OsString::from_vec(buffer))
    }

    fn publish_link(source: &File, destination: &File, name: &OsStr, owner: &Owner) -> Result<()> {
        let literal = cstring(&readlink(source, name)?)?;
        publish(destination, name, |temporary| {
            let temporary = cstring(temporary)?;
            // SAFETY: strings are NUL terminated; the destination descriptor is live.
            syscall(unsafe {
                libc::symlinkat(
                    literal.as_ptr(),
                    destination.as_raw_fd(),
                    temporary.as_ptr(),
                )
            })?;
            // SAFETY: no-follow ownership changes only the newly created link.
            syscall(unsafe {
                libc::fchownat(
                    destination.as_raw_fd(),
                    temporary.as_ptr(),
                    owner.uid,
                    owner.gid,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            })?;
            Ok(())
        })
    }

    fn inventory(source: &File) -> Result<(Vec<OsString>, Vec<OsString>)> {
        let mut folders = Vec::new();
        let mut names = Vec::new();
        let mut folder_links = Vec::new();
        let alias = PathBuf::from(format!("/proc/self/fd/{}", source.as_raw_fd()));
        for entry in fs::read_dir(&alias)? {
            let entry = entry?;
            let info = fs::symlink_metadata(entry.path())?;
            if info.is_dir() {
                folders.push(entry.file_name());
            } else if info.file_type().is_symlink()
                && fs::metadata(entry.path()).is_ok_and(|info| info.is_dir())
            {
                folder_links.push(entry.file_name());
            } else {
                names.push(entry.file_name());
            }
        }
        folders.sort();
        names.sort();
        folder_links.sort();
        names.extend(folder_links);
        Ok((folders, names))
    }

    struct FileProof {
        path: PathBuf,
        stamp: Value,
        sha256: String,
    }

    fn seed_file(
        source: &File,
        destination: &File,
        name: &OsStr,
        relative: &Path,
        owner: &Owner,
        files: &mut Vec<FileProof>,
    ) -> Result<()> {
        let metadata = metadata_at(source, name)?.ok_or("firstboot skeleton changed")?;
        let existing = metadata_at(destination, name)?.is_some();
        if metadata.file_type().is_symlink() {
            if metadata.uid() != 0 {
                return invalid("firstboot skeleton link has foreign ownership");
            }
            if !existing {
                publish_link(source, destination, name, owner)?;
            }
            return Ok(());
        }
        trusted(&metadata)?;
        if !metadata.is_file() {
            return invalid("unsupported special file in firstboot skeleton");
        }
        if existing {
            return Ok(());
        }
        if metadata.len() > LIMIT || files.len() >= 4096 {
            return invalid("firstboot skeleton exceeds bounds");
        }
        let mut input = open_at(
            source,
            name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )?;
        let data = bounded_read(&mut input)?;
        if data.len() as u64 != metadata.len() || stamp(&input.metadata()?) != stamp(&metadata) {
            return invalid("firstboot skeleton changed");
        }
        publish_bytes(destination, name, &data, metadata.mode() & 0o777, owner)?;
        if !custody(relative) {
            let mut output = open_at(
                destination,
                name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )?;
            let witness = output.metadata()?;
            if !witness.is_file()
                || witness.nlink() != 1
                || witness.uid() != owner.uid
                || bounded_read(&mut output)? != data
                || stamp(&output.metadata()?) != stamp(&witness)
            {
                return invalid("attested firstboot default changed");
            }
            files.push(FileProof {
                path: relative.to_owned(),
                stamp: stamp(&witness),
                sha256: digest(&data),
            });
        }
        Ok(())
    }

    fn seed_directory(
        source: &File,
        destination: &File,
        relative: &Path,
        owner: &Owner,
        files: &mut Vec<FileProof>,
    ) -> Result<()> {
        trusted(&source.metadata()?)?;
        let (folders, names) = inventory(source)?;
        for name in names {
            seed_file(
                source,
                destination,
                &name,
                &relative.join(&name),
                owner,
                files,
            )?;
        }
        destination.sync_all()?;
        for name in folders {
            let origin = open_at(source, &name, DIRECTORY)?;
            trusted(&origin.metadata()?)?;
            let created = match metadata_at(destination, &name)? {
                Some(info) if !info.is_dir() || info.file_type().is_symlink() => continue,
                Some(_) => false,
                None => {
                    mkdir(destination, &name, origin.metadata()?.mode() & 0o7777)?;
                    true
                }
            };
            let child = open_at(destination, &name, DIRECTORY)?;
            if created {
                chmod(&child, origin.metadata()?.mode() & 0o7777)?;
                chown(&child, owner.uid, owner.gid)?;
            }
            seed_directory(&origin, &child, &relative.join(&name), owner, files)?;
        }
        Ok(())
    }

    fn verify_home_binding(parent: &File, name: &OsStr, home: &File) -> Result<()> {
        let owned = home.metadata()?;
        if metadata_at(parent, name)?
            .is_none_or(|current| current.ino() != owned.ino() || current.dev() != owned.dev())
        {
            return invalid("firstboot home is redirected");
        }
        Ok(())
    }

    fn seed(home_path: &Path, skeleton: &Path, owner: &Owner, proofs_path: &Path) -> Result<()> {
        if !rustix::process::geteuid().is_root() {
            return invalid("firstboot seed creation requires root");
        }
        let parent_path = proofs_path.parent().ok_or("invalid proof directory")?;
        let grandparent = open_directory(parent_path.parent().ok_or("invalid proof directory")?)?;
        trusted(&grandparent.metadata()?)?;
        let parent = open_at(
            &grandparent,
            parent_path.file_name().ok_or("invalid proof directory")?,
            DIRECTORY,
        )?;
        trusted(&parent.metadata()?)?;
        let proof_name = proofs_path.file_name().ok_or("invalid proof directory")?;
        if metadata_at(&parent, proof_name)?.is_none() {
            mkdir(&parent, proof_name, 0o755)?;
        }
        let proofs = open_at(&parent, proof_name, DIRECTORY)?;
        trusted(&proofs.metadata()?)?;
        // Resolve only the parent: Fedora /home itself is a trusted alias, but
        // neither the account directory nor any edge inside it may be redirected.
        let home_parent_path = home_path.parent().ok_or("invalid home")?;
        fs::create_dir_all(home_parent_path)?;
        let home_parent = open_directory(&fs::canonicalize(home_parent_path)?)?;
        let home_name = home_path.file_name().ok_or("invalid home")?;
        if metadata_at(&home_parent, home_name)?.is_none() {
            mkdir(&home_parent, home_name, 0o700)?;
        }
        let info = metadata_at(&home_parent, home_name)?.ok_or("firstboot home is redirected")?;
        if !info.is_dir() || info.file_type().is_symlink() {
            return invalid("firstboot home is redirected");
        }
        let home = open_at(&home_parent, home_name, DIRECTORY)?;
        let receipt_name = OsString::from(format!("{}.json", owner.uid));
        if metadata_at(&proofs, &receipt_name)?.is_some() {
            verify_receipt(&proofs, &receipt_name, &home, owner.uid)?;
            return verify_home_binding(&home_parent, home_name, &home);
        }
        let info = home.metadata()?;
        if (info.uid(), info.gid()) != (owner.uid, owner.gid) {
            chown(&home, owner.uid, owner.gid)?;
        }
        let mut files = Vec::new();
        match fs::metadata(skeleton) {
            Ok(info) => {
                trusted(&fs::symlink_metadata(skeleton)?)?;
                if info.is_dir() {
                    let source = open_directory(skeleton)?;
                    seed_directory(&source, &home, Path::new(""), owner, &mut files)?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let encoded = encode_receipt(&home, owner.uid, &files)?;
        if encoded.len() as u64 > LIMIT {
            return invalid("firstboot seed proof exceeds bounds");
        }
        verify_home_binding(&home_parent, home_name, &home)?;
        publish_bytes(
            &proofs,
            &receipt_name,
            &encoded,
            0o644,
            &Owner { uid: 0, gid: 0 },
        )?;
        home.sync_all()?;
        proofs.sync_all()?;
        // A same-name home substitution must not produce a successful transaction.
        verify_home_binding(&home_parent, home_name, &home)
    }

    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct AccountTarget {
        operation: String,
        target: String,
        user: String,
        uid: u32,
    }

    impl AccountTarget {
        fn binding(&self) -> Result<account_custody::portable::PortableBinding> {
            uuid::Uuid::parse_str(&self.operation)?;
            if self.uid != 1000
                || self.target.len() != 64
                || !self.target.bytes().all(|b| b.is_ascii_hexdigit())
                || self.user.is_empty()
                || self.user.len() > 32
                || !self
                    .user
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
            {
                return invalid("invalid installed account target");
            }
            Ok(account_custody::portable::PortableBinding::new(
                &self.operation,
                &self.target,
                &self.user,
                self.uid,
            )?)
        }
    }

    fn root_directory(path: &Path) -> Result<File> {
        if !path.is_absolute() {
            return invalid("expected absolute transport path");
        }
        let mut directory = open_directory(Path::new("/"))?;
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(name) => {
                    directory = open_at(&directory, name, DIRECTORY)?;
                    trusted(&directory.metadata()?)?;
                }
                _ => return invalid("unsafe transport path"),
            }
        }
        Ok(directory)
    }

    fn private_file(directory: &File, name: &str, limit: u64) -> Result<File> {
        let file = open_at(
            directory,
            OsStr::new(name),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )?;
        let info = file.metadata()?;
        if !info.is_file()
            || info.uid() != 0
            || info.mode() & 0o077 != 0
            || info.nlink() != 1
            || info.len() > limit
        {
            return invalid("unsafe account transport file");
        }
        Ok(file)
    }

    fn account_stage(target: &AccountTarget, destination: &Path) -> Result<()> {
        target.binding()?;
        if destination.file_name() != Some(OsStr::new("online-accounts")) {
            return invalid("invalid account staging directory");
        }
        let parent = root_directory(destination.parent().ok_or("missing staging parent")?)?;
        let name = OsStr::new("online-accounts");
        // Parse before publishing anything. The frame, including its key, is
        // always zeroized when buffered in the privileged transport process.
        let accounts =
            installer::account_handoff::InstallAccounts::read_from(&mut io::stdin().lock())?;
        let mut frame = zeroize::Zeroizing::new(Vec::new());
        accounts.write_to(&mut *frame)?;
        mkdir(&parent, name, 0o700)?;
        let directory = open_at(&parent, name, DIRECTORY)?;
        let result = (|| {
            let owner = Owner { uid: 0, gid: 0 };
            publish_bytes(
                &directory,
                OsStr::new("binding.json"),
                &serde_json::to_vec(target)?,
                0o600,
                &owner,
            )?;
            publish_bytes(&directory, OsStr::new("transport"), &frame, 0o600, &owner)?;
            directory.sync_all()?;
            parent.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            // This directory was created exclusively by this invocation.
            fs::remove_dir_all(destination)?;
            parent.sync_all()?;
        }
        result
    }

    fn account_identity(user: &str) -> Result<(PathBuf, Owner)> {
        let name = CString::new(user)?;
        // SAFETY: this single-threaded helper copies the NSS entry immediately.
        let account = unsafe { libc::getpwnam(name.as_ptr()) };
        if account.is_null() {
            return invalid("account consumer does not exist");
        }
        // SAFETY: getpwnam succeeded; its strings and scalar fields are initialized.
        let (home, owner) = unsafe {
            (
                PathBuf::from(OsStr::from_bytes(
                    CStr::from_ptr((*account).pw_dir).to_bytes(),
                )),
                Owner {
                    uid: (*account).pw_uid,
                    gid: (*account).pw_gid,
                },
            )
        };
        if owner.uid != 1000
            || (home != Path::new(&format!("/var/home/{user}"))
                && home != Path::new(&format!("/home/{user}")))
        {
            return invalid("installed account identity mismatch");
        }
        let canonical = fs::canonicalize(&home)?;
        if canonical != Path::new(&format!("/var/home/{user}"))
            || fs::symlink_metadata(&home)?.file_type().is_symlink()
            || fs::metadata(&canonical)?.uid() != owner.uid
        {
            return invalid("redirected installed account home");
        }
        Ok((canonical, owner))
    }

    fn account_consume(target: &AccountTarget) -> Result<()> {
        let (home, owner) = account_identity(&target.user)?;
        if rustix::process::getuid().as_raw() != owner.uid
            || rustix::process::geteuid().as_raw() != owner.uid
        {
            return invalid("account import requires the real destination user");
        }
        let accounts =
            installer::account_handoff::InstallAccounts::read_from(&mut io::stdin().lock())?;
        accounts.import_for_user(&home.join(".config/tilo"), &target.binding()?)
    }

    fn account_adopt(user: &str) -> Result<()> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        let directory = root_directory(Path::new("/etc/tilo/online-accounts"))?;
        if directory.metadata()?.mode() & 0o077 != 0 {
            return invalid("account transport directory is not private");
        }
        let target: AccountTarget =
            serde_json::from_reader(private_file(&directory, "binding.json", 4096)?)?;
        target.binding()?;
        if target.user != user {
            return invalid("account transport targets another user");
        }
        let (home, owner) = account_identity(user)?;
        if metadata_at(&directory, OsStr::new("adopted"))?.is_some() {
            let mut receipt = private_file(&directory, "adopted", 4096)?;
            if bounded_read(&mut receipt)? != serde_json::to_vec(&target)? {
                return invalid("account adoption receipt mismatch");
            }
        } else {
            let transport = private_file(&directory, "transport", (8 << 20) + 1024)?;
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args([
                    "--accounts-consume",
                    &target.operation,
                    &target.target,
                    &target.user,
                    "1000",
                ])
                .env_clear()
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .stdin(Stdio::from(transport))
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: the child performs only async-signal-safe credential syscalls
            // before exec. setuid changes real, effective and saved identity.
            unsafe {
                command.pre_exec(move || {
                    if libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setgid(owner.gid) != 0
                        || libc::setuid(owner.uid) != 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            if !command.status()?.success() {
                return invalid("installed account import failed; transport preserved");
            }
            publish_bytes(
                &directory,
                OsStr::new("adopted"),
                &serde_json::to_vec(&target)?,
                0o600,
                &Owner { uid: 0, gid: 0 },
            )?;
        }
        // Receipt publication precedes key removal, so a crash is retryable.
        let name = CString::new("transport")?;
        // SAFETY: directory and NUL-terminated relative name are valid.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0
            && io::Error::last_os_error().kind() != io::ErrorKind::NotFound
        {
            return Err(io::Error::last_os_error().into());
        }
        directory.sync_all()?;
        Ok(())
    }

    fn accounts_dispatch(arguments: &[OsString]) -> Result<()> {
        let args: Vec<_> = arguments
            .iter()
            .map(|v| v.to_str().ok_or("non-UTF8 account argument"))
            .collect::<std::result::Result<_, _>>()?;
        if let ["--accounts-fingerprint", size, major, wwn, serial] = args.as_slice() {
            println!(
                "{}",
                installer::account_handoff::target_fingerprint(size.parse()?, major, wwn, serial)
            );
            return Ok(());
        }
        if let ["--accounts-consume", operation, target, user, uid] = args.as_slice() {
            return account_consume(&AccountTarget {
                operation: (*operation).into(),
                target: (*target).into(),
                user: (*user).into(),
                uid: uid.parse()?,
            });
        }
        if !rustix::process::geteuid().is_root() || !rustix::process::getuid().is_root() {
            return invalid("account transport requires root");
        }
        match args.as_slice() {
            [
                "--accounts-stage",
                operation,
                target,
                user,
                uid,
                destination,
            ] => account_stage(
                &AccountTarget {
                    operation: (*operation).into(),
                    target: (*target).into(),
                    user: (*user).into(),
                    uid: uid.parse()?,
                },
                Path::new(destination),
            ),
            ["--accounts-adopt", user] => account_adopt(user),
            _ => invalid("invalid account transport arguments"),
        }
    }

    fn main_result() -> Result<()> {
        let arguments: Vec<_> = std::env::args_os().skip(1).collect();
        if arguments
            .first()
            .is_some_and(|v| v.as_bytes().starts_with(b"--accounts-"))
        {
            return accounts_dispatch(&arguments);
        }
        if arguments == [OsStr::new("--capabilities")] {
            println!("{{\"schema\":1,\"seed_protocol\":1}}");
            return Ok(());
        }
        if arguments.len() != 1 {
            return invalid("expected one account name");
        }
        let argument = arguments.first().ok_or("expected one account name")?;
        let name = cstring(argument)?;
        // SAFETY: name is NUL terminated. This single-threaded program copies the
        // passwd entry before calling anything else that could use the NSS buffer.
        let account = unsafe { libc::getpwnam(name.as_ptr()) };
        if account.is_null() {
            return invalid(&format!(
                "\"getpwnam(): name not found: '{}'\"",
                argument.to_string_lossy()
            ));
        }
        // SAFETY: successful getpwnam returns an initialized entry and pw_dir string.
        let (home, owner) = unsafe {
            (
                PathBuf::from(OsStr::from_bytes(
                    CStr::from_ptr((*account).pw_dir).to_bytes(),
                )),
                Owner {
                    uid: (*account).pw_uid,
                    gid: (*account).pw_gid,
                },
            )
        };
        seed(
            &home,
            Path::new("/etc/skel"),
            &owner,
            Path::new("/var/lib/tilo/firstboot-home-seeds"),
        )?;
        Ok(())
    }

    pub(super) fn run() -> std::process::ExitCode {
        if let Err(error) = main_result() {
            eprintln!("firstboot home defaults: {error}");
            std::process::ExitCode::FAILURE
        } else {
            std::process::ExitCode::SUCCESS
        }
    }
}

// Also called by the lightweight Cargo regression driver, which compiles this
// exact entry point without linking the graphical installer library.
pub(crate) fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    {
        linux::run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("firstboot home defaults require Linux");
        std::process::ExitCode::from(2)
    }
}
