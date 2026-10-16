use std::{
    ffi::CString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::ffi::OsStrExt,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, Zeroizing};

// ARM release UKIs include the kernel, initrd and board DTB. The Lenovo 83ED
// image already reaches 273,856,512 bytes before signing, so the verifier and
// broker output checks must accept more than 256 MiB.
const MAX_EFI_IMAGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_OTHER_SIGNED_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PRIVATE_KEY_BYTES: u64 = 1024 * 1024;
const MAX_CERTIFICATE_BYTES: u64 = 1024 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 1024 * 1024;
const EFI_GLOBAL_VARIABLE_GUID: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
const EFI_IMAGE_SECURITY_DATABASE_GUID: &str = "d719b2cb-3d3a-4596-a3bc-dad00e67656f";
const SECURE_BOOT_ATTRIBUTES: u32 = 0x27;

// Deliberately not `tilo_files::native::nofollow`: the signing tool depends on
// no workspace crate, so development-signing builds stay off workspace-hack.
fn open_readonly_without_symlinks(path: &Path) -> std::io::Result<fs::File> {
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;

    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains an embedded NUL byte",
        )
    })?;
    let flags = u64::try_from(libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .map_err(|_| std::io::Error::other("open flags are outside the Linux u64 ABI"))?;
    let how = OpenHow {
        flags,
        mode: 0,
        resolve: RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
    };
    // SAFETY: path and open_how remain live for this syscall. A successful
    // descriptor is uniquely transferred into File below.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            path.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        // systemd's RestrictSUIDSGID= answers openat2 with ENOSYS (seccomp
        // cannot inspect its mode). Walk the path instead, refusing a
        // symlink at every component exactly as RESOLVE_NO_SYMLINKS does.
        if error.raw_os_error() == Some(libc::ENOSYS) {
            return open_components_without_symlinks(path.as_bytes());
        }
        return Err(error);
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| std::io::Error::other("openat2 returned an invalid descriptor"))?;
    // SAFETY: openat2 returned this owned descriptor and no other File owns it.
    Ok(unsafe { fs::File::from_raw_fd(descriptor) })
}

/// `openat` one component at a time: every directory with `O_NOFOLLOW |
/// O_DIRECTORY` (a symlink fails with ENOTDIR), the file with `O_NOFOLLOW`
/// (a symlink fails with ELOOP).
fn open_components_without_symlinks(path: &[u8]) -> std::io::Result<fs::File> {
    fn openat(dir: libc::c_int, name: &[u8], flags: libc::c_int) -> std::io::Result<OwnedFd> {
        let name = CString::new(name).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains a NUL byte")
        })?;
        // SAFETY: name is a live C string; a successful descriptor is owned below.
        let descriptor = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned this descriptor and nothing else owns it.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }
    let mut components = path
        .split(|byte| *byte == b'/')
        .filter(|component| !component.is_empty() && *component != b".")
        .peekable();
    let mut current = if path.starts_with(b"/") {
        Some(openat(
            libc::AT_FDCWD,
            b"/",
            libc::O_PATH | libc::O_DIRECTORY,
        )?)
    } else {
        None
    };
    while let Some(component) = components.next() {
        let dir = current.as_ref().map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
        if components.peek().is_none() {
            let file = openat(
                dir,
                component,
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW,
            )?;
            return Ok(fs::File::from(file));
        }
        current = Some(openat(
            dir,
            component,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )?);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "path names no file",
    ))
}

fn read_bounded(path: &Path, limit: u64, kind: &str) -> Result<Vec<u8>, String> {
    let file = open_readonly_without_symlinks(path)
        .map_err(|error| format!("cannot open {kind} {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {kind} {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{kind} {} is not a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {kind} {}: {error}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "{kind} {} exceeds the {limit}-byte safety limit",
            path.display()
        ));
    }
    Ok(bytes)
}

fn read_text_bounded(path: &Path, limit: u64, kind: &str) -> Result<String, String> {
    String::from_utf8(read_bounded(path, limit, kind)?)
        .map_err(|_| format!("{kind} {} is not UTF-8", path.display()))
}

fn read_private_key(path: &Path) -> Result<Zeroizing<String>, String> {
    let file = open_readonly_without_symlinks(path)
        .map_err(|error| format!("cannot open private key {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect private key {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "private key {} is not a regular file",
            path.display()
        ));
    }
    // A privileged signer must not consume a key controlled by another local
    // account merely because root can bypass that account's mode-0600 read
    // restriction. Check ownership on the same open inode as the contents.
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!(
            "private key {} is not owned by the signing account",
            path.display()
        ));
    }
    if metadata.nlink() != 1 {
        return Err(format!(
            "private key {} has {} hard links; expected exactly one",
            path.display(),
            metadata.nlink()
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "private key {} has insecure permissions {mode:o}; expected 0600 or stricter",
            path.display()
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_PRIVATE_KEY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read private key {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_PRIVATE_KEY_BYTES {
        return Err(format!(
            "private key {} exceeds the {MAX_PRIVATE_KEY_BYTES}-byte safety limit",
            path.display()
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("private key {} is not UTF-8 PEM", path.display()))?;
    Ok(Zeroizing::new(text.to_owned()))
}

fn usage() -> String {
    "usage:\n  tilo-crypto hash-password < password\n  tilo-crypto certificate-subject CERT\n  tilo-crypto certificate-sha256 CERT\n  tilo-crypto certificate-der CERT OUTPUT\n  tilo-crypto certificate-esl CERT OUTPUT\n  tilo-crypto firmware-read-esl EFIVARS VARIABLE OUTPUT\n  tilo-crypto firmware-write-esl EFIVARS VARIABLE INPUT [SIGNING_KEY SIGNING_CERT]\n  tilo-crypto generate-ed25519-key PRIVATE_KEY PUBLIC_HEX\n  tilo-crypto generate-secure-boot-cert COMMON_NAME KEY CERT\n  tilo-crypto generate-tpm-authority-key PRIVATE_KEY PUBLIC_KEY\n  tilo-crypto generate-tls-fixture IP CA_CERT SERVER_KEY SERVER_CERT\n  tilo-crypto key-matches-certificate PRIVATE_KEY CERT\n  tilo-crypto p256-public-key-sha256 PUBLIC_KEY\n  tilo-crypto rsa-public-key PRIVATE_KEY PUBLIC_KEY\n  tilo-crypto rsa-public-key-info PUBLIC_KEY\n  tilo-crypto sign-rsa-sha256 KEY INPUT SIGNATURE\n  tilo-crypto verify-rsa-sha256 PUBLIC_KEY INPUT SIGNATURE\n  tilo-crypto verify-rsa-sha256-efi PUBLIC_KEY EFI_IMAGE SIGNATURE\n  tilo-crypto verify-p256-sha256 PUBLIC_KEY INPUT SIGNATURE\n  tilo-crypto sign-module KEY CERT INPUT OUTPUT\n  tilo-crypto verify-module CERT INPUT\n  tilo-crypto sign-efi KEY CERT INPUT OUTPUT\n  tilo-crypto verify-efi CERT INPUT\n  tilo-crypto verify-efi-signature-list CERT ESL\n  tilo-crypto verify-exclusive-efi-signature-list CERT ESL\n  tilo-crypto efi-signature-count INPUT\n  tilo-crypto durable-unlink PATH".into()
}

fn generate_ed25519_key(private_path: &Path, public_path: &Path) -> Result<(), String> {
    if private_path == public_path {
        return Err("private and public key paths must differ".into());
    }
    let mut seed = Zeroizing::new([0_u8; 32]);
    getrandom::fill(&mut *seed).map_err(|_| "operating-system random generator failed")?;
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    write_new(private_path, &*seed, 0o600)?;
    let public = format!("{}\n", hex::encode(signing.verifying_key().to_bytes()));
    if let Err(error) = write_new(public_path, public.as_bytes(), 0o644) {
        let _ = fs::remove_file(private_path);
        return Err(error);
    }
    Ok(())
}

fn durable_unlink_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let path = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or("durable unlink path has no parent directory")?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("cannot inspect unlink target {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "durable unlink target is not a regular non-symlink file: {}",
            path.display()
        ));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!(
            "durable unlink target is not owned by the caller: {}",
            path.display()
        ));
    }
    fs::remove_file(&path).map_err(|error| format!("cannot unlink {}: {error}", path.display()))?;
    let mut options = OpenOptions::new();
    options.read(true);
    options.custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
    options
        .open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "unlink completed but cannot persist directory {}: {error}",
                parent.display()
            )
        })
}

fn durable_rename_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let source = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let target = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let source_parent = source
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or("durable rename source has no parent directory")?;
    let target_parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or("durable rename target has no parent directory")?;
    if source_parent != target_parent {
        return Err("durable rename requires source and target in the same directory".into());
    }
    let metadata = fs::symlink_metadata(&source)
        .map_err(|error| format!("cannot inspect rename source {}: {error}", source.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "durable rename source is not a regular non-symlink file: {}",
            source.display()
        ));
    }
    fs::rename(&source, &target).map_err(|error| {
        format!(
            "cannot rename {} to {}: {error}",
            source.display(),
            target.display()
        )
    })?;
    let mut options = OpenOptions::new();
    options.read(true);
    options.custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
    options
        .open(source_parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "rename completed but cannot persist directory {}: {error}",
                source_parent.display()
            )
        })
}

fn firmware_variable_path(root: &Path, variable: &std::ffi::OsStr) -> Result<PathBuf, String> {
    let variable = variable
        .to_str()
        .ok_or("firmware variable name must be UTF-8")?;
    let guid = match variable {
        "db" => EFI_IMAGE_SECURITY_DATABASE_GUID,
        "KEK" | "PK" => EFI_GLOBAL_VARIABLE_GUID,
        _ => return Err(format!("unsupported Secure Boot variable: {variable}")),
    };
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("cannot inspect efivarfs {}: {error}", root.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "efivarfs is not a real directory: {}",
            root.display()
        ));
    }
    Ok(root.join(format!("{variable}-{guid}")))
}

fn firmware_read_esl_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let root = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let variable = args.next().ok_or_else(usage)?;
    let output = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let path = firmware_variable_path(&root, &variable)?;
    let encoded = read_bounded(&path, MAX_SIGNATURE_BYTES + 4, "firmware variable")?;
    let attributes = encoded
        .get(..4)
        .ok_or("firmware variable has no attributes")?
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| "firmware variable has invalid attributes")?;
    // Secure Boot authority variables must be persistent and protected by
    // authenticated, timestamped updates.  Accepting an ESL while ignoring
    // these efivarfs attributes could mistake an unauthenticated firmware
    // variable for the db/KEK/PK trust database we intend to verify.
    if attributes != SECURE_BOOT_ATTRIBUTES {
        return Err(format!(
            "firmware Secure Boot variable has unsafe attributes 0x{attributes:08x}; expected 0x{SECURE_BOOT_ATTRIBUTES:08x}"
        ));
    }
    let esl = encoded
        .get(4..)
        .filter(|bytes| !bytes.is_empty())
        .ok_or("firmware variable has no EFI signature-list payload")?;
    tilo_crypto::validate_efi_signature_list(esl)?;
    write_new(&output, esl, 0o644)
}

fn firmware_write_esl_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let root = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let variable = args.next().ok_or_else(usage)?;
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let signing_key = args.next().map(PathBuf::from);
    let signing_certificate = args.next().map(PathBuf::from);
    if args.next().is_some() || signing_key.is_some() != signing_certificate.is_some() {
        return Err(usage());
    }
    let path = firmware_variable_path(&root, &variable)?;
    let esl = read_bounded(&input, MAX_SIGNATURE_BYTES, "EFI signature list")?;
    if esl.is_empty() {
        return Err("refusing to write an empty EFI signature list".into());
    }
    tilo_crypto::validate_efi_signature_list(&esl)?;
    // SetupMode permits an empty PKCS#7 certificate in an authenticated update
    // envelope. PK is enrolled last by the caller, transitioning to UserMode;
    // firmware then requires genuinely signed updates for subsequent changes.
    // Use the actual UTC time. A deliberately future-dated update would raise
    // the variable's replay-protection floor and could prevent legitimate key
    // rotation until the wall clock catches up.
    let now = time::OffsetDateTime::now_utc();
    let year = u16::try_from(now.year()).map_err(|_| "current year is outside EFI_TIME")?;
    let mut timestamp = Vec::with_capacity(16);
    timestamp.extend_from_slice(&year.to_le_bytes());
    timestamp.extend_from_slice(&[
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        0,
    ]);
    timestamp.extend_from_slice(&0_u32.to_le_bytes());
    timestamp.extend_from_slice(&0_i16.to_le_bytes());
    timestamp.extend_from_slice(&[0, 0]);
    let signature = if let (Some(key), Some(certificate)) = (signing_key, signing_certificate) {
        let key = read_private_key(&key)?;
        let certificate = read_bounded(&certificate, MAX_CERTIFICATE_BYTES, "EFI certificate")?;
        let mut signed = Vec::new();
        for unit in variable.to_string_lossy().encode_utf16() {
            signed.extend_from_slice(&unit.to_le_bytes());
        }
        let guid = if variable == "db" {
            [
                0xcb, 0xb2, 0x19, 0xd7, 0x3a, 0x3d, 0x96, 0x45, 0xa3, 0xbc, 0xda, 0xd0, 0x0e, 0x67,
                0x65, 0x6f,
            ]
        } else {
            [
                0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03,
                0x2b, 0x8c,
            ]
        };
        signed.extend_from_slice(&guid);
        signed.extend_from_slice(&0x27_u32.to_le_bytes());
        signed.extend_from_slice(&timestamp);
        signed.extend_from_slice(&esl);
        tilo_crypto::sign_efi_variable_payload(&key, &certificate, &signed)?
    } else {
        Vec::new()
    };
    let capacity = 44_usize
        .checked_add(signature.len())
        .and_then(|size| size.checked_add(esl.len()))
        .ok_or("firmware-variable size overflow")?;
    let mut encoded = Vec::with_capacity(capacity);
    encoded.extend_from_slice(&0x27_u32.to_le_bytes());
    encoded.extend_from_slice(&timestamp);
    let certificate_length = 24_usize
        .checked_add(signature.len())
        .and_then(|length| u32::try_from(length).ok())
        .ok_or("EFI variable signature is too large")?;
    encoded.extend_from_slice(&certificate_length.to_le_bytes());
    encoded.extend_from_slice(&0x0200_u16.to_le_bytes());
    encoded.extend_from_slice(&0x0ef1_u16.to_le_bytes());
    encoded.extend_from_slice(&[
        0x9d, 0xd2, 0xaf, 0x4a, 0xdf, 0x68, 0xee, 0x49, 0x8a, 0xa9, 0x34, 0x7d, 0x37, 0x56, 0x65,
        0xa7,
    ]);
    encoded.extend_from_slice(&signature);
    encoded.extend_from_slice(&esl);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| format!("cannot open firmware variable {}: {error}", path.display()))?;
    let written = file
        .write(&encoded)
        .map_err(|error| format!("cannot write firmware variable {}: {error}", path.display()))?;
    if written != encoded.len() {
        return Err(format!(
            "short firmware-variable write: wrote {written} of {} bytes",
            encoded.len()
        ));
    }
    // efivarfs commits in write(2) and does not implement fsync(2).
    Ok(())
}

fn write_new(path: &Path, contents: &[u8], mode: u32) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn generate(common_name: &str, key_path: &Path, cert_path: &Path) -> Result<(), String> {
    if key_path.exists() || cert_path.exists() {
        return Err("refusing to replace an existing key or certificate".into());
    }
    let generated = tilo_crypto::generate_secure_boot_certificate(common_name)?;
    write_new(key_path, generated.private_key_pem.as_bytes(), 0o600)?;
    if let Err(error) = write_new(cert_path, generated.certificate_pem.as_bytes(), 0o644) {
        let _ = fs::remove_file(key_path);
        return Err(error);
    }
    Ok(())
}

fn generate_tpm_authority_key(private_path: &Path, public_path: &Path) -> Result<(), String> {
    if private_path == public_path {
        return Err("private and public key paths must differ".into());
    }
    if private_path.exists() || public_path.exists() {
        return Err("refusing to replace an existing TPM authority key".into());
    }
    let generated = tilo_crypto::generate_tpm_authority_key()?;
    write_new(private_path, generated.private_key_pem().as_bytes(), 0o600)?;
    if let Err(error) = write_new(public_path, generated.public_key_pem().as_bytes(), 0o644) {
        let _ = fs::remove_file(private_path);
        return Err(error);
    }
    Ok(())
}

fn sign_efi_command(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let key = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let certificate = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let output = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    if input == output {
        return Err("refusing to sign an EFI image in place".into());
    }
    let key = read_private_key(&key)?;
    let certificate = read_text_bounded(&certificate, MAX_CERTIFICATE_BYTES, "EFI certificate")?;
    let image = read_bounded(&input, MAX_EFI_IMAGE_BYTES, "EFI image")?;
    let signed = tilo_crypto::sign_efi_image(&key, &certificate, &image)?;
    write_new(&output, &signed, 0o644)
}

fn verify_efi_command(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let certificate = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let certificate = read_text_bounded(&certificate, MAX_CERTIFICATE_BYTES, "EFI certificate")?;
    let image = read_bounded(&input, MAX_EFI_IMAGE_BYTES, "EFI image")?;
    tilo_crypto::verify_efi_image(&certificate, &image)
}

fn verify_efi_signature_list_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let certificate = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let signature_list = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let certificate = read_bounded(&certificate, MAX_CERTIFICATE_BYTES, "certificate")?;
    let signature_list = read_bounded(&signature_list, MAX_SIGNATURE_BYTES, "EFI signature list")?;
    tilo_crypto::verify_efi_signature_list_contains_certificate(&certificate, &signature_list)
}

fn verify_exclusive_efi_signature_list_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let certificate = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let signature_list = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let certificate = read_bounded(&certificate, MAX_CERTIFICATE_BYTES, "certificate")?;
    let signature_list = read_bounded(&signature_list, MAX_SIGNATURE_BYTES, "EFI signature list")?;
    tilo_crypto::verify_efi_signature_list_exclusively_contains_certificate(
        &certificate,
        &signature_list,
    )
}

fn sign_module_command(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let key = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let certificate = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let output = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() || input == output {
        return Err(usage());
    }
    let key = read_private_key(&key)?;
    let certificate = read_bounded(&certificate, MAX_CERTIFICATE_BYTES, "certificate")?;
    let module = read_bounded(&input, MAX_OTHER_SIGNED_INPUT_BYTES, "kernel module")?;
    let signed = tilo_crypto::sign_kernel_module(&key, &certificate, &module)?;
    write_new(&output, &signed, 0o644)
}

fn verify_module_command(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let certificate = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let certificate = read_bounded(&certificate, MAX_CERTIFICATE_BYTES, "certificate")?;
    let module = read_bounded(&input, MAX_OTHER_SIGNED_INPUT_BYTES, "kernel module")?;
    tilo_crypto::verify_kernel_module(&certificate, &module)
}

fn certificate_der_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let output = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let bytes = read_bounded(&input, MAX_CERTIFICATE_BYTES, "certificate")?;
    write_new(&output, &tilo_crypto::certificate_der(&bytes)?, 0o644)
}

fn certificate_subject_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let path = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let bytes = read_bounded(&path, MAX_CERTIFICATE_BYTES, "certificate")?;
    println!("{}", tilo_crypto::inspect_certificate(&bytes)?.subject);
    Ok(())
}

fn certificate_sha256_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let path = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let bytes = read_bounded(&path, MAX_CERTIFICATE_BYTES, "certificate")?;
    println!("{}", tilo_crypto::certificate_sha256(&bytes)?);
    Ok(())
}

fn certificate_esl_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let output = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let bytes = read_bounded(&input, MAX_CERTIFICATE_BYTES, "certificate")?;
    write_new(
        &output,
        &tilo_crypto::certificate_efi_signature_list(&bytes)?,
        0o644,
    )
}

fn hash_password_command(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    if args.next().is_some() {
        return Err(usage());
    }
    let mut password = Vec::new();
    std::io::stdin()
        .take(1026)
        .read_to_end(&mut password)
        .map_err(|error| format!("cannot read password: {error}"))?;
    if password.last() == Some(&b'\n') {
        password.pop();
    }
    let result = tilo_crypto::hash_account_password(&mut password);
    password.zeroize();
    println!("{}", result?);
    Ok(())
}

fn generate_ed25519_key_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let private = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let public = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    generate_ed25519_key(&private, &public)
}

fn generate_secure_boot_cert_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let common_name = args.next().ok_or_else(usage)?;
    let key = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let cert = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    generate(
        common_name
            .to_str()
            .ok_or("common name must be valid UTF-8")?,
        &key,
        &cert,
    )
}

fn generate_tpm_authority_key_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let private = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let public = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    generate_tpm_authority_key(&private, &public)
}

#[cfg(feature = "tls-fixture")]
fn generate_tls_fixture_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let ip = args.next().ok_or_else(usage)?;
    let ca_certificate = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let server_key = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let server_certificate = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let ip = ip
        .to_str()
        .ok_or("server IP must be valid UTF-8")?
        .parse()
        .map_err(|_| "server IP is invalid")?;
    let fixture = tilo_crypto::generate_tls_fixture(ip)?;
    write_new(
        &ca_certificate,
        fixture.ca_certificate_pem.as_bytes(),
        0o644,
    )?;
    if let Err(error) = write_new(
        &server_key,
        fixture.server_private_key_pem.as_bytes(),
        0o600,
    ) {
        let _ = fs::remove_file(&ca_certificate);
        return Err(error);
    }
    if let Err(error) = write_new(
        &server_certificate,
        fixture.server_certificate_pem.as_bytes(),
        0o644,
    ) {
        let _ = fs::remove_file(&server_key);
        let _ = fs::remove_file(&ca_certificate);
        return Err(error);
    }
    Ok(())
}

fn p256_public_key_sha256_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let public = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let public = read_text_bounded(&public, MAX_CERTIFICATE_BYTES, "public key")?;
    println!("{}", tilo_crypto::p256_public_key_sha256(&public)?);
    Ok(())
}

fn sign_rsa_sha256_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let key = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let output = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let key = read_private_key(&key)?;
    let message = read_bounded(&input, MAX_OTHER_SIGNED_INPUT_BYTES, "signed input")?;
    let signature = tilo_crypto::sign_rsa_sha256(&key, &message)?;
    write_new(&output, &signature, 0o600)
}

fn rsa_public_key_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let private = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let output = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let private = read_private_key(&private)?;
    let public = tilo_crypto::rsa_public_key_pem(&private)?;
    write_new(&output, public.as_bytes(), 0o644)
}

fn rsa_public_key_info_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let public = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let public = read_text_bounded(&public, MAX_CERTIFICATE_BYTES, "public key")?;
    let (bits, identity) = tilo_crypto::rsa_public_key_info(&public)?;
    println!("{bits} {identity}");
    Ok(())
}

fn key_matches_certificate_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let private = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let certificate = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let private = read_private_key(&private)?;
    let certificate = read_bounded(&certificate, MAX_CERTIFICATE_BYTES, "certificate")?;
    tilo_crypto::rsa_private_key_matches_certificate(&private, &certificate)
}

fn verify_rsa_sha256_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
    efi_image: bool,
) -> Result<(), String> {
    let key = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let signature = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let key = read_text_bounded(&key, MAX_CERTIFICATE_BYTES, "public key")?;
    // The detached target-UKI signature covers the full EFI image. Keep the
    // ordinary RSA operation bounded separately for small policy messages.
    let limit = if efi_image {
        MAX_EFI_IMAGE_BYTES
    } else {
        MAX_OTHER_SIGNED_INPUT_BYTES
    };
    let message = read_bounded(&input, limit, "signed input")?;
    let signature = read_bounded(&signature, MAX_SIGNATURE_BYTES, "signature")?;
    tilo_crypto::verify_rsa_sha256(&key, &message, &signature)
}

fn verify_p256_sha256_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let key = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    let signature = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let key = read_text_bounded(&key, MAX_CERTIFICATE_BYTES, "public key")?;
    let message = read_bounded(&input, MAX_OTHER_SIGNED_INPUT_BYTES, "signed input")?;
    let signature = read_bounded(&signature, MAX_SIGNATURE_BYTES, "signature")?;
    tilo_crypto::verify_p256_sha256(&key, &message, &signature)
}

fn efi_signature_count_command(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let input = args.next().map(PathBuf::from).ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let image = read_bounded(&input, MAX_EFI_IMAGE_BYTES, "EFI image")?;
    println!("{}", tilo_crypto::efi_signature_count(&image)?);
    Ok(())
}

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let command = args.next().ok_or_else(usage)?;
    match command.to_str() {
        Some("hash-password") => hash_password_command(args),
        Some("certificate-subject") => certificate_subject_command(args),
        Some("certificate-sha256") => certificate_sha256_command(args),
        Some("certificate-der") => certificate_der_command(args),
        Some("certificate-esl") => certificate_esl_command(args),
        Some("firmware-read-esl") => firmware_read_esl_command(args),
        Some("firmware-write-esl") => firmware_write_esl_command(args),
        Some("generate-ed25519-key") => generate_ed25519_key_command(args),
        Some("generate-secure-boot-cert") => generate_secure_boot_cert_command(args),
        Some("generate-tpm-authority-key") => generate_tpm_authority_key_command(args),
        #[cfg(feature = "tls-fixture")]
        Some("generate-tls-fixture") => generate_tls_fixture_command(args),
        Some("p256-public-key-sha256") => p256_public_key_sha256_command(args),
        Some("sign-rsa-sha256") => sign_rsa_sha256_command(args),
        Some("rsa-public-key") => rsa_public_key_command(args),
        Some("rsa-public-key-info") => rsa_public_key_info_command(args),
        Some("key-matches-certificate") => key_matches_certificate_command(args),
        Some("verify-rsa-sha256") => verify_rsa_sha256_command(args, false),
        Some("verify-rsa-sha256-efi") => verify_rsa_sha256_command(args, true),
        Some("verify-p256-sha256") => verify_p256_sha256_command(args),
        Some("sign-module") => sign_module_command(args),
        Some("verify-module") => verify_module_command(args),
        Some("sign-efi") => sign_efi_command(args),
        Some("verify-efi") => verify_efi_command(args),
        Some("verify-efi-signature-list") => verify_efi_signature_list_command(args),
        Some("verify-exclusive-efi-signature-list") => {
            verify_exclusive_efi_signature_list_command(args)
        }
        Some("durable-rename") => durable_rename_command(args),
        Some("durable-unlink") => durable_unlink_command(args),
        Some("efi-signature-count") => efi_signature_count_command(args),
        _ => Err(usage()),
    }
}

fn main() -> std::process::ExitCode {
    // Secure Boot private keys must never enter a persisted core image.
    // SAFETY: PR_SET_DUMPABLE consumes scalar arguments only.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        eprintln!("tilo-crypto: could not disable core dumps");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(error) = run() {
        eprintln!("tilo-crypto: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn component_walk_opens_files_and_refuses_every_symlink() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let real = temp.path().join("real");
        fs::create_dir(&real).expect("directory");
        fs::write(real.join("key.pub"), b"public").expect("file");
        symlink(&real, temp.path().join("linked-dir")).expect("directory link");
        symlink(real.join("key.pub"), temp.path().join("linked.pub")).expect("file link");
        let open = |path: &Path| open_components_without_symlinks(path.as_os_str().as_bytes());

        let mut content = String::new();
        open(&real.join("key.pub"))
            .expect("plain path opens")
            .read_to_string(&mut content)
            .expect("readable");
        assert_eq!(content, "public");
        assert!(open(&temp.path().join("linked-dir/key.pub")).is_err());
        assert!(open(&temp.path().join("linked.pub")).is_err());
        assert!(open(&real.join("missing")).is_err());
    }

    #[test]
    fn detached_efi_signature_check_accepts_large_uki_without_widening_generic_rsa() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let public = temp.path().join("authority.pub");
        let image = temp.path().join("target.efi");
        let signature = temp.path().join("target.sig");
        let authority = tilo_crypto::generate_tpm_authority_key().expect("RSA authority");
        fs::write(&public, authority.public_key_pem()).expect("public authority");
        fs::File::create(&image)
            .expect("sparse target UKI")
            .set_len(MAX_OTHER_SIGNED_INPUT_BYTES + 1)
            .expect("large target UKI");
        fs::write(&signature, b"invalid signature").expect("signature fixture");
        let arguments = || {
            [
                OsString::from(&public),
                OsString::from(&image),
                OsString::from(&signature),
            ]
            .into_iter()
        };
        let generic = verify_rsa_sha256_command(arguments(), false).unwrap_err();
        assert!(generic.contains("268435456-byte safety limit"), "{generic}");
        let efi = verify_rsa_sha256_command(arguments(), true).unwrap_err();
        assert!(!efi.contains("safety limit"), "{efi}");
    }

    #[test]
    fn ed25519_generation_is_exclusive_and_emits_matching_public_identity() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let private = temp.path().join("owner.key");
        let public = temp.path().join("owner.pub");
        generate_ed25519_key(&private, &public).expect("generate owner identity");
        let seed: [u8; 32] = fs::read(&private)
            .expect("read private seed")
            .try_into()
            .expect("32-byte private seed");
        let expected = ed25519_dalek::SigningKey::from_bytes(&seed).verifying_key();
        assert_eq!(
            fs::read_to_string(&public).expect("read public identity"),
            format!("{}\n", hex::encode(expected.to_bytes()))
        );
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(generate_ed25519_key(&private, &public).is_err());
    }

    #[test]
    fn tpm_authority_generation_is_exclusive_and_private() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let private = temp.path().join("authority.key");
        let public = temp.path().join("authority.pub");
        generate_tpm_authority_key(&private, &public).expect("generate TPM authority");
        let public_pem = fs::read_to_string(&public).expect("read public key");
        assert_eq!(
            tilo_crypto::rsa_public_key_info(&public_pem)
                .expect("inspect public key")
                .0,
            2048
        );
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(generate_tpm_authority_key(&private, &public).is_err());
    }

    #[test]
    fn durable_rename_is_same_directory_and_rejects_symlink_sources() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let other = temp.path().join("other");
        fs::create_dir(&other).expect("second directory");
        let source = temp.path().join("signed.new");
        let target = temp.path().join("signed.efi");
        fs::write(&source, b"verified signed image").expect("signed fixture");

        durable_rename_command([OsString::from(&source), OsString::from(&target)].into_iter())
            .expect("durable same-directory replacement");
        assert!(!source.exists());
        assert_eq!(
            fs::read(&target).expect("installed image"),
            b"verified signed image"
        );

        let cross_source = temp.path().join("cross.new");
        fs::write(&cross_source, b"cross-directory").expect("cross fixture");
        assert!(
            durable_rename_command(
                [
                    OsString::from(&cross_source),
                    OsString::from(other.join("target")),
                ]
                .into_iter(),
            )
            .is_err()
        );
        assert!(cross_source.exists());

        let link = temp.path().join("link.new");
        symlink(&cross_source, &link).expect("source symlink");
        assert!(
            durable_rename_command([OsString::from(&link), OsString::from(&target)].into_iter(),)
                .is_err()
        );
    }

    #[test]
    fn durable_unlink_rejects_symlinks_and_removes_only_the_named_inode() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let key = temp.path().join("PK.key");
        let sibling = temp.path().join("db.key");
        let link = temp.path().join("key-link");
        fs::write(&key, b"retired enrollment authority").expect("key fixture");
        fs::write(&sibling, b"ongoing signing authority").expect("sibling fixture");
        symlink(&key, &link).expect("key symlink");
        assert!(
            durable_unlink_command([OsString::from(&link)].into_iter()).is_err(),
            "retirement must never follow a link"
        );
        assert!(key.exists());
        durable_unlink_command([OsString::from(&key)].into_iter())
            .expect("durably retire exact enrollment key");
        assert!(!key.exists());
        assert_eq!(
            fs::read(sibling).expect("ongoing key remains"),
            b"ongoing signing authority"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one lifecycle test keeps all efivarfs mutation assertions together"
    )]
    fn firmware_esl_round_trip_uses_attributes_and_refuses_symlinks() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = temp.path().join("db.esl");
        let output = temp.path().join("readback.esl");
        let mut esl = vec![0_u8; 16];
        esl.extend_from_slice(&45_u32.to_le_bytes());
        esl.extend_from_slice(&0_u32.to_le_bytes());
        esl.extend_from_slice(&17_u32.to_le_bytes());
        esl.extend_from_slice(&[0_u8; 16]);
        esl.push(1);
        fs::write(&input, &esl).expect("write ESL fixture");
        let year_before = time::OffsetDateTime::now_utc().year();
        firmware_write_esl_command(
            [
                OsString::from(temp.path()),
                OsString::from("db"),
                OsString::from(&input),
            ]
            .into_iter(),
        )
        .expect("write fake efivarfs variable");
        let year_after = time::OffsetDateTime::now_utc().year();

        let variable = temp
            .path()
            .join(format!("db-{EFI_IMAGE_SECURITY_DATABASE_GUID}"));
        let encoded = fs::read(&variable).expect("read encoded variable");
        assert_eq!(&encoded[..4], &0x27_u32.to_le_bytes());
        let encoded_year = i32::from(u16::from_le_bytes([encoded[4], encoded[5]]));
        assert!((year_before..=year_after).contains(&encoded_year));
        assert_eq!(&encoded[20..24], &24_u32.to_le_bytes());
        assert_eq!(&encoded[24..26], &0x0200_u16.to_le_bytes());
        assert_eq!(&encoded[26..28], &0x0ef1_u16.to_le_bytes());
        assert_eq!(&encoded[44..], esl);

        // efivarfs accepts the authenticated update envelope, while firmware
        // exposes only the resulting ESL on subsequent reads.
        let mut stored = 0x27_u32.to_le_bytes().to_vec();
        stored.extend_from_slice(&esl);
        fs::write(&variable, stored).expect("simulate firmware read-back");

        let mut unauthenticated = 0x07_u32.to_le_bytes().to_vec();
        unauthenticated.extend_from_slice(&esl);
        fs::write(&variable, unauthenticated).expect("simulate unsafe firmware attributes");
        assert!(
            firmware_read_esl_command(
                [
                    OsString::from(temp.path()),
                    OsString::from("db"),
                    OsString::from(&output),
                ]
                .into_iter(),
            )
            .is_err(),
            "an unauthenticated Secure Boot variable must not establish firmware trust"
        );
        assert!(!output.exists());

        let mut malformed = SECURE_BOOT_ATTRIBUTES.to_le_bytes().to_vec();
        malformed.extend_from_slice(b"not-an-esl");
        fs::write(&variable, malformed).expect("simulate malformed authenticated read-back");
        assert!(
            firmware_read_esl_command(
                [
                    OsString::from(temp.path()),
                    OsString::from("db"),
                    OsString::from(&output),
                ]
                .into_iter(),
            )
            .is_err(),
            "authenticated attributes must not make malformed ESL data trusted"
        );
        assert!(!output.exists());

        let mut stored = 0x27_u32.to_le_bytes().to_vec();
        stored.extend_from_slice(&esl);
        fs::write(&variable, stored).expect("restore authenticated firmware read-back");
        firmware_read_esl_command(
            [
                OsString::from(temp.path()),
                OsString::from("db"),
                OsString::from(&output),
            ]
            .into_iter(),
        )
        .expect("read fake efivarfs variable");
        assert_eq!(fs::read(output).expect("read output"), esl);

        fs::write(&input, b"not-an-esl").expect("write malformed ESL fixture");
        assert!(
            firmware_write_esl_command(
                [
                    OsString::from(temp.path()),
                    OsString::from("db"),
                    OsString::from(&input),
                ]
                .into_iter(),
            )
            .is_err(),
            "firmware writer must reject malformed ESL bytes before opening efivarfs"
        );

        fs::remove_file(&variable).expect("remove variable");
        symlink(&input, &variable).expect("create variable symlink");
        assert!(
            firmware_write_esl_command(
                [
                    OsString::from(temp.path()),
                    OsString::from("db"),
                    OsString::from(&input),
                ]
                .into_iter(),
            )
            .is_err()
        );
    }

    #[test]
    fn private_key_reader_rejects_symlinks_and_permissive_files() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let key = temp.path().join("db.key");
        let link = temp.path().join("db-link.key");
        write_new(&key, b"private key fixture", 0o600).expect("private key fixture");
        assert_eq!(
            &*read_private_key(&key).expect("read private key"),
            "private key fixture"
        );

        symlink(&key, &link).expect("private key symlink");
        assert!(read_private_key(&link).is_err());
        assert!(read_bounded(&link, 1024, "input fixture").is_err());

        let hard_link = temp.path().join("db-hard-link.key");
        fs::hard_link(&key, &hard_link).expect("private key hard link");
        let error = read_private_key(&key).expect_err("multiply-linked key must fail");
        assert!(error.contains("2 hard links"));
        fs::remove_file(hard_link).expect("remove private key hard link");

        let linked_parent = temp.path().join("linked-parent");
        symlink(temp.path(), &linked_parent).expect("parent-directory symlink");
        assert!(read_private_key(&linked_parent.join("db.key")).is_err());
        assert!(read_bounded(&linked_parent.join("db.key"), 1024, "input fixture").is_err());

        fs::set_permissions(&key, fs::Permissions::from_mode(0o640))
            .expect("loosen private key mode");
        let error = read_private_key(&key).expect_err("group-readable key must fail");
        assert!(error.contains("insecure permissions 640"));
    }
}
