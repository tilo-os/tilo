//! Unsafe native requests for the default-off diagnostic, not provisioning.
//!
//! Native-call safety: SDK buffers retain their documented size/alignment and
//! their owning allocations for each synchronous call; Files retain borrowed
//! handles. Counted UTF-16 and descriptor allocations outlive NtCreateFile.
//! Pending outputs are never consumed as initialized results: diagnostic-only
//! output allocations and uncertain returned handles are retained until exit.
//! Variable token arrays and SID/ACL storage are checked before interpretation.
use super::*;
use crate::native::atomic::directory_probe::observation;
use crate::native::atomic::directory_probe::{
    Candidate, Event, Outcome, ParentAccess, Request, Sink,
};
use std::{
    collections::BTreeMap,
    path::{Component, Prefix},
    time::Instant,
};
use windows_sys::{
    Wdk::{
        Storage::FileSystem::{FILE_DIRECTORY_FILE, NtFlushBuffersFileEx},
        System::SystemServices::RtlGetVersion,
    },
    Win32::{
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, LocalFree},
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo,
                SE_FILE_OBJECT,
            },
            DACL_SECURITY_INFORMATION, GetTokenInformation, LUID_AND_ATTRIBUTES,
            LookupPrivilegeNameW, OWNER_SECURITY_INFORMATION, SE_PRIVILEGE_ENABLED,
            TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenElevation, TokenPrivileges,
        },
        Storage::FileSystem::{
            FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_ATTRIBUTE_TAG_INFO, FILE_TRAVERSE,
            FILE_WRITE_ATTRIBUTES, FileAttributeTagInfo,
        },
        System::{
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            SystemInformation::{GetNativeSystemInfo, OSVERSIONINFOW, SYSTEM_INFO},
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
};

const MINIMAL: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
const WRITABLE: u32 = MINIMAL | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY | FILE_WRITE_ATTRIBUTES;
const CHILD: u32 = WRITABLE | FILE_LIST_DIRECTORY | DELETE;
const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const OPTIONS: u32 = FILE_DIRECTORY_FILE
    | FILE_WRITE_THROUGH
    | FILE_SYNCHRONOUS_IO_NONALERT
    | FILE_OPEN_REPARSE_POINT;
const PENDING: i32 = 0x103;

type NativeFlush = unsafe extern "system" fn(
    HANDLE,
    u32,
    *const std::ffi::c_void,
    u32,
    *mut IO_STATUS_BLOCK,
) -> i32;
// Type-check the dynamic call against the pinned SDK binding without creating an
// eager import that could prevent reporting a missing runtime export.
const _: NativeFlush = NtFlushBuffersFileEx;
struct Log<'a> {
    sequence: u64,
    sink: &'a mut Sink<'a>,
    native_flush: Option<NativeFlush>,
}
impl Log<'_> {
    fn event(&mut self, operation: &str, fields: Vec<(&str, String)>) -> io::Result<()> {
        self.sequence += 1;
        (self.sink)(&Event {
            sequence: self.sequence,
            operation: operation.into(),
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.into(), v))
                .collect::<BTreeMap<_, _>>(),
        })
    }
    fn error(&mut self, operation: &str, error: &io::Error) -> io::Result<()> {
        self.event(
            operation,
            vec![
                ("error", error.to_string()),
                ("win32", format!("{:?}", error.raw_os_error())),
            ],
        )
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn nt_error(status: i32) -> io::Error {
    if status < 0 {
        io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
    } else {
        invalid("native result pending, unknown or incomplete")
    }
}
fn iosb() -> Box<IO_STATUS_BLOCK> {
    let mut value = Box::<IO_STATUS_BLOCK>::default();
    value.Anonymous.Status = PENDING;
    value.Information = usize::MAX;
    value
}

pub(crate) fn run(request: &Request, sink: &mut Sink<'_>) -> Outcome {
    let mut log = Log {
        sequence: 0,
        sink,
        native_flush: None,
    };
    let result = experiment(request, &mut log);
    let mut outcome = observation::outcome(result);
    if let Err(e) = log.event("terminal", vec![("fully_observed",outcome.fully_observed.to_string()),("candidate_calls_succeeded",outcome.candidate_calls_succeeded.to_string()),("result",outcome.terminal.clone()),("qualification","unqualified; fixture premise does not establish production anchor or crash durability".into())]) {
        outcome = Outcome::from_result(&Err(e), false);
    }
    outcome
}

fn environment(log: &mut Log<'_>) -> io::Result<()> {
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    let status = unsafe { RtlGetVersion(&raw mut version) };
    log.event(
        "os_version_completion",
        vec![("ntstatus", format!("{status:#010x}"))],
    )?;
    if status != 0 {
        return Err(nt_error(status));
    }
    log.event(
        "os_version",
        vec![
            ("major", version.dwMajorVersion.to_string()),
            ("minor", version.dwMinorVersion.to_string()),
            ("build", version.dwBuildNumber.to_string()),
        ],
    )?;
    let mut system = SYSTEM_INFO::default();
    unsafe {
        GetNativeSystemInfo(&raw mut system);
    }
    log.event(
        "architecture",
        vec![
            (
                "native_architecture",
                unsafe { system.Anonymous.Anonymous.wProcessorArchitecture }.to_string(),
            ),
            ("process_architecture", std::env::consts::ARCH.into()),
        ],
    )?;
    log.event(
        "query_requested",
        vec![("operation", "OpenProcessToken TOKEN_QUERY".into())],
    )?;
    let mut raw = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    let token = Token(raw);
    log.event(
        "query_requested",
        vec![("operation", "GetTokenInformation TokenElevation".into())],
    )?;
    let mut elevation = TOKEN_ELEVATION::default();
    let mut length = 0;
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != size_of::<TOKEN_ELEVATION>() {
        return Err(invalid("incomplete elevation evidence"));
    }
    log.event(
        "token_elevation",
        vec![("elevated", elevation.TokenIsElevated.to_string())],
    )?;
    log.event(
        "query_requested",
        vec![(
            "operation",
            "GetTokenInformation TokenPrivileges sizing".into(),
        )],
    )?;
    let result = unsafe {
        GetTokenInformation(
            token.0,
            TokenPrivileges,
            ptr::null_mut(),
            0,
            &raw mut length,
        )
    };
    if result != 0
        || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        || !(4..=65536).contains(&length)
    {
        return Err(invalid("cannot size token privilege evidence"));
    }
    log.event(
        "query_requested",
        vec![("operation", "GetTokenInformation TokenPrivileges".into())],
    )?;
    let capacity = length;
    let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenPrivileges,
            buffer.as_mut_ptr().cast(),
            capacity,
            &raw mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if length > capacity || length < 4 {
        return Err(invalid("invalid privilege evidence size"));
    }
    let count = unsafe { (*buffer.as_ptr().cast::<TOKEN_PRIVILEGES>()).PrivilegeCount } as usize;
    let offset = offset_of!(TOKEN_PRIVILEGES, Privileges);
    if count > (length as usize - offset) / size_of::<LUID_AND_ATTRIBUTES>() {
        return Err(invalid("privilege array outside returned evidence"));
    }
    let privileges = unsafe {
        std::slice::from_raw_parts(
            buffer
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<LUID_AND_ATTRIBUTES>(),
            count,
        )
    };
    let mut forbidden = false;
    for entry in privileges {
        log.event(
            "query_requested",
            vec![
                ("operation", "LookupPrivilegeNameW".into()),
                ("luid_low", entry.Luid.LowPart.to_string()),
                ("luid_high", entry.Luid.HighPart.to_string()),
            ],
        )?;
        let mut name = [0u16; 256];
        let mut capacity = name.len() as u32;
        if unsafe {
            LookupPrivilegeNameW(
                ptr::null(),
                &entry.Luid,
                name.as_mut_ptr(),
                &raw mut capacity,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let name = String::from_utf16(
            name.get(..capacity as usize)
                .ok_or_else(|| invalid("privilege name size"))?,
        )
        .map_err(|_| invalid("privilege name encoding"))?;
        let enabled = entry.Attributes & SE_PRIVILEGE_ENABLED != 0;
        forbidden |= enabled
            && matches!(
                name.as_str(),
                "SeBackupPrivilege"
                    | "SeRestorePrivilege"
                    | "SeManageVolumePrivilege"
                    | "SeTakeOwnershipPrivilege"
                    | "SeDebugPrivilege"
                    | "SeTcbPrivilege"
            );
        log.event(
            "token_privilege",
            vec![
                ("name", name),
                ("attributes", format!("{:#x}", entry.Attributes)),
                ("enabled", enabled.to_string()),
            ],
        )?;
    }
    if forbidden || elevation.TokenIsElevated != 0 {
        return Err(refused(
            "ordinary non-elevated token without enabled privileged storage authority required",
        ));
    }
    Ok(())
}

fn descriptor(file: &File) -> io::Result<String> {
    let mut raw = ptr::null_mut();
    let flags = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            flags,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut raw,
        )
    };
    struct Allocation(*mut std::ffi::c_void);
    impl Drop for Allocation {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    let _owned = Allocation(raw);
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    if raw.is_null() {
        return Err(invalid("missing security descriptor"));
    }
    let mut text = ptr::null_mut();
    let mut count = 0;
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            raw,
            1,
            flags,
            &raw mut text,
            &raw mut count,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _text = Allocation(text.cast());
    if text.is_null() || count == 0 || count > 65536 {
        return Err(invalid("invalid security descriptor text"));
    }
    let units = unsafe { std::slice::from_raw_parts(text, count as usize) };
    String::from_utf16(units.strip_suffix(&[0]).unwrap_or(units))
        .map_err(|_| invalid("security descriptor text encoding"))
}
fn snapshot(
    file: &File,
    name: &str,
    log: &mut Log<'_>,
) -> io::Result<(Identity, FILE_ATTRIBUTE_TAG_INFO)> {
    log.event(
        "query_requested",
        vec![
            (
                "operation",
                "FileIdInfo, FileAttributeTagInfo, owner/DACL descriptor".into(),
            ),
            ("relative_name", name.into()),
        ],
    )?;
    let identity = Identity::of(file)?;
    let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            (&raw mut tag).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    log.event(
        "held_object",
        vec![
            ("relative_name", name.into()),
            (
                "volume_serial",
                identity.public_volume.inner.serial.to_string(),
            ),
            (
                "file_id",
                identity.id.iter().map(|b| format!("{b:02x}")).collect(),
            ),
            ("attributes", format!("{:#x}", tag.FileAttributes)),
            ("reparse_tag", format!("{:#x}", tag.ReparseTag)),
            ("owner_dacl_sddl", descriptor(file)?),
        ],
    )?;
    Ok((identity, tag))
}

fn inspect(
    file: &File,
    name: &str,
    principal: Option<&acl::Principal>,
    log: &mut Log<'_>,
) -> io::Result<Identity> {
    let (identity, tag) = snapshot(file, name, log)?;
    if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(refused("directory reparse or non-directory refused"));
    }
    if let Some(principal) = principal {
        acl::validate_private_security(file, principal)?;
        log.event("privacy_validated", vec![("relative_name", name.into())])?;
    }
    Ok(identity)
}

fn relative(
    parent: &File,
    name: &str,
    access: u32,
    create: bool,
    descriptor: Option<acl::Descriptor>,
    log: &mut Log<'_>,
) -> io::Result<File> {
    observation::required(relative_observed(
        parent, name, access, create, descriptor, log,
    ))
}

fn relative_observed(
    parent: &File,
    name: &str,
    access: u32,
    create: bool,
    descriptor: Option<acl::Descriptor>,
    log: &mut Log<'_>,
) -> io::Result<observation::Directory<File>> {
    let mut units = component(OsStr::new(name))?;
    let mut unicode = Box::new(UNICODE_STRING {
        Length: (units.len() * 2) as u16,
        MaximumLength: (units.len() * 2) as u16,
        Buffer: units.as_mut_ptr(),
    });
    // Duplicate only existing authority; never reopen a path for stronger rights.
    let parent_custody = parent.try_clone()?;
    let attrs = Box::new(OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent_custody.as_raw_handle(),
        ObjectName: &raw mut *unicode,
        Attributes: 0,
        SecurityDescriptor: descriptor
            .as_ref()
            .map_or(ptr::null_mut(), |d| d.as_ptr().cast()),
        SecurityQualityOfService: ptr::null_mut(),
    });
    let mut raw: Box<HANDLE> = Box::new(ptr::null_mut());
    let mut completion = iosb();
    let now = Instant::now();
    let status = unsafe {
        NtCreateFile(
            &raw mut *raw,
            access,
            &raw const *attrs,
            &raw mut *completion,
            ptr::null(),
            FILE_ATTRIBUTE_DIRECTORY,
            SHARE,
            if create { FILE_CREATE } else { FILE_OPEN },
            OPTIONS,
            ptr::null(),
            0,
        )
    };
    let done = if status == PENDING {
        PENDING
    } else {
        unsafe { completion.Anonymous.Status }
    };
    // Synchronous handles must complete synchronously. On malformed pending output,
    // retain native output storage/handle until process exit rather than free live I/O.
    let info = if status == PENDING {
        usize::MAX
    } else {
        completion.Information
    };
    // Only STATUS_SUCCESS transfers a proven owned handle. On any other
    // result do not interpret or close a possibly unspecified output handle.
    // The bounded diagnostic process retains uncertain resources until exit.
    let file = if status == 0 && !raw.is_null() && *raw as isize != -1 {
        Some(unsafe { File::from_raw_handle(*raw) })
    } else {
        None
    };
    if status == PENDING {
        // All addresses submitted to the syscall remain valid even if an
        // unexpected asynchronous request still owns them. Do not inspect its
        // output slot, wait on an unproven handle, or guess cancellation.
        let _ = Box::leak(completion);
        let _ = Box::leak(raw);
        let _ = Box::leak(unicode);
        let _ = Box::leak(attrs);
        std::mem::forget(units);
        std::mem::forget(descriptor);
        std::mem::forget(parent_custody);
    }
    log.event(
        if create { "create" } else { "open" },
        vec![
            ("name", name.into()),
            ("access", format!("{access:#x}")),
            ("share", format!("{SHARE:#x}")),
            ("options", format!("{OPTIONS:#x}")),
            (
                "disposition",
                if create { FILE_CREATE } else { FILE_OPEN }.to_string(),
            ),
            ("ntstatus", format!("{status:#010x}")),
            ("io_status", format!("{done:#010x}")),
            ("io_information", info.to_string()),
            ("duration_ns", now.elapsed().as_nanos().to_string()),
        ],
    )?;
    if status == PENDING {
        return Err(invalid("pending directory request; namespace unknown"));
    }
    if status < 0 {
        return Ok(observation::Directory::Refused(nt_error(status)));
    }
    if status != 0 {
        return Err(nt_error(status));
    }
    if done != 0 || info != if create { 2 } else { 1 } {
        return Err(invalid("incomplete directory creation/open completion"));
    }
    file.map(observation::Directory::Opened)
        .ok_or_else(|| invalid("successful directory request returned no handle"))
}

fn volume(file: &File, log: &mut Log<'_>) -> io::Result<()> {
    let mut device = Box::<FILE_FS_DEVICE_INFORMATION>::default();
    let mut completion = iosb();
    let status = unsafe {
        NtQueryVolumeInformationFile(
            file.as_raw_handle(),
            &raw mut *completion,
            (&raw mut *device).cast(),
            size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            FileFsDeviceInformation,
        )
    };
    if status == PENDING {
        let _ = Box::leak(completion);
        let _ = Box::leak(device);
        log.event(
            "volume_device",
            vec![
                ("ntstatus", format!("{status:#010x}")),
                ("completion", "pending; outputs unavailable".into()),
            ],
        )?;
        return Err(invalid("pending local device query"));
    }
    let done = unsafe { completion.Anonymous.Status };
    let bytes = completion.Information;
    let kind = device.DeviceType;
    let characteristics = device.Characteristics;
    log.event(
        "volume_device_completion",
        vec![
            ("ntstatus", format!("{status:#010x}")),
            ("io_status", format!("{done:#010x}")),
            ("io_information", bytes.to_string()),
        ],
    )?;
    if status != 0 || done != 0 || bytes < size_of::<FILE_FS_DEVICE_INFORMATION>() {
        return Err(invalid("missing complete local-device evidence"));
    }
    log.event(
        "volume_device",
        vec![
            ("device_type", kind.to_string()),
            ("characteristics", format!("{characteristics:#x}")),
        ],
    )?;
    log.event(
        "query_requested",
        vec![("operation", "GetVolumeInformationByHandleW".into())],
    )?;
    let mut name = [0u16; 261];
    let mut flags = 0;
    if unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut flags,
            name.as_mut_ptr(),
            name.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    log.event(
        "filesystem",
        vec![
            (
                "name",
                String::from_utf16_lossy(
                    &name[..name.iter().position(|n| *n == 0).unwrap_or(name.len())],
                ),
            ),
            ("flags", format!("{flags:#x}")),
        ],
    )?;
    if kind != FILE_DEVICE_DISK
        || characteristics & (FILE_REMOTE_DEVICE | FILE_REMOTE_DEVICE_VSMB) != 0
        || name.get(..5) != Some(&[78, 84, 70, 83, 0])
        || flags & FILE_PERSISTENT_ACLS == 0
    {
        return Err(refused("local NTFS with persistent ACLs required"));
    }
    Ok(())
}

fn flush(
    file: &File,
    name: &str,
    access: u32,
    checkpoint: &str,
    log: &mut Log<'_>,
) -> io::Result<bool> {
    let mut success = true;
    for repeat in 1..=2 {
        let mut completion = iosb();
        let now = Instant::now();
        let call = log
            .native_flush
            .ok_or_else(|| invalid("runtime native flush export not admitted"))?;
        let status = unsafe {
            call(
                file.as_raw_handle(),
                0,
                ptr::null(),
                0,
                &raw mut *completion,
            )
        };
        let done = if status == PENDING {
            PENDING
        } else {
            unsafe { completion.Anonymous.Status }
        };
        let information = if status == PENDING {
            usize::MAX
        } else {
            completion.Information
        };
        if status == PENDING {
            let _ = Box::leak(completion);
        }
        log.event(
            checkpoint,
            vec![
                ("operation", "NtFlushBuffersFileEx".into()),
                ("name", name.into()),
                ("access", format!("{access:#x}")),
                ("flags", "0".into()),
                ("repeat", repeat.to_string()),
                ("ntstatus", format!("{status:#010x}")),
                ("io_status", format!("{done:#010x}")),
                ("io_information", information.to_string()),
                ("duration_ns", now.elapsed().as_nanos().to_string()),
            ],
        )?;
        success &= observation::flush(status, done, information)?;
    }
    let now = Instant::now();
    let result = unsafe { FlushFileBuffers(file.as_raw_handle()) };
    let error = if result == 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    log.event(
        "win32_flush_comparison",
        vec![
            ("name", name.into()),
            ("access", format!("{access:#x}")),
            ("success", (result != 0).to_string()),
            (
                "win32",
                format!("{:?}", error.as_ref().and_then(io::Error::raw_os_error)),
            ),
            ("duration_ns", now.elapsed().as_nanos().to_string()),
        ],
    )?;
    Ok(success)
}

fn roots(
    request: &Request,
    principal: &acl::Principal,
    log: &mut Log<'_>,
) -> io::Result<(File, File)> {
    let mut components = request.root.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d,
            _ => {
                return Err(refused(
                    "probe requires an absolute local drive directory; UNC/device paths refused",
                ));
            }
        },
        _ => return Err(refused("absolute drive root required")),
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(refused("drive-relative root refused"));
    }
    let drive_path = format!("{}:\\", char::from(drive));
    let open = |access| {
        File::options()
            .access_mode(access)
            .share_mode(SHARE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&drive_path)
    };
    log.event(
        "drive_directory_open_requested",
        vec![
            ("path", drive_path.clone()),
            ("access", format!("{MINIMAL:#x}")),
            ("share", format!("{SHARE:#x}")),
            (
                "flags",
                format!(
                    "{:#x}",
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT
                ),
            ),
            ("note", "directory handle only; no volume handle".into()),
        ],
    )?;
    let mut held = open(MINIMAL)?;
    inspect(&held, &drive_path, None, log)?;
    log.event(
        "ancestor_writable_access_requested",
        vec![
            ("path", drive_path.clone()),
            ("access", format!("{WRITABLE:#x}")),
        ],
    )?;
    match open(WRITABLE) {
        Ok(file) => {
            let same = Identity::of(&file)? == Identity::of(&held)?;
            log.event(
                "ancestor_writable_access",
                vec![
                    ("path", drive_path.clone()),
                    ("access", format!("{WRITABLE:#x}")),
                    ("same_identity", same.to_string()),
                ],
            )?;
            if !same {
                return Err(refused("drive directory identity changed"));
            }
        }
        Err(e) => log.error("ancestor_writable_access_refused", &e)?,
    }
    let names: Vec<_> = components
        .map(|c| match c {
            Component::Normal(n) => n
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("root must have representable diagnostic components")),
            _ => Err(refused("root traversal components refused")),
        })
        .collect::<io::Result<_>>()?;
    if names.is_empty() {
        return Err(refused(
            "volume-root fixture prohibited; choose disposable child namespace",
        ));
    }
    let mut writable_root = None;
    for (index, name) in names.iter().enumerate() {
        let child = relative(&held, name, MINIMAL, false, None, log)?;
        let identity = inspect(&child, name, None, log)?;
        match observation::ancestor(
            relative_observed(&held, name, WRITABLE, false, None, log),
            index + 1 == names.len(),
            |error| log.error("ancestor_writable_access_refused", error),
        )? {
            Some(writable) => {
                if Identity::of(&writable)? != identity {
                    return Err(refused(
                        "ancestor access handles identify different objects",
                    ));
                }
                log.event(
                    "ancestor_writable_access",
                    vec![
                        ("component", name.clone()),
                        ("access", format!("{WRITABLE:#x}")),
                        ("same_identity", "true".into()),
                        (
                            "boundary",
                            if index + 1 == names.len() {
                                "test-anchor"
                            } else {
                                "above-test-anchor"
                            }
                            .into(),
                        ),
                    ],
                )?;
                if index + 1 == names.len() {
                    writable_root = Some(writable);
                }
            }
            None => {}
        }
        held = child;
    }
    let identity = inspect(&held, ".", Some(principal), log)?;
    volume(&held, log)?;
    let equal = identity.public_volume.inner.serial == request.evidence.volume_serial
        && identity.id == request.evidence.file_id;
    log.event("anchor_comparison",vec![("matches",equal.to_string()),("external_establishment_record",request.evidence.establishment_record.clone()),("storage_profile_claim",request.evidence.storage_profile.clone()),("premise","controller-supplied; this process cannot prove power-cycle establishment or cache configuration".into())])?;
    if !equal {
        return Err(refused(
            "held root identity mismatches external controller evidence; no fixture effects",
        ));
    }
    Ok((
        held,
        writable_root.ok_or_else(|| refused("writable test-root handle unavailable"))?,
    ))
}

fn experiment(request: &Request, log: &mut Log<'_>) -> io::Result<bool> {
    log.event(
        "experiment",
        vec![
            ("candidate", format!("{:?}", request.candidate)),
            ("parent_access", format!("{:?}", request.parent_access)),
            ("qualification", "never certified by this process".into()),
            (
                "retention",
                "all fixture objects retained; external controller owns cleanup".into(),
            ),
        ],
    )?;
    environment(log)?;
    let principal = acl::Principal::current()?;
    let (root, root_write) = roots(request, &principal, log)?;
    // Verify runtime presence before the first call. The pinned SDK binding is used
    // only after export observation; an absent export is a missing prerequisite.
    let module_name: Vec<u16> = "ntdll.dll".encode_utf16().chain(Some(0)).collect();
    let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    if module.is_null() {
        return Err(io::Error::last_os_error());
    }
    let export = unsafe { GetProcAddress(module, c"NtFlushBuffersFileEx".as_ptr().cast()) };
    let available = export.is_some();
    log.event(
        "native_flush_export",
        vec![
            ("available", available.to_string()),
            (
                "binding",
                "windows-sys 0.61.2 ntdll NtFlushBuffersFileEx".into(),
            ),
        ],
    )?;
    if !available {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "NtFlushBuffersFileEx export unavailable",
        ));
    }
    // SAFETY: named ntdll export; signature checked against windows-sys above.
    log.native_flush = export.map(|function| unsafe {
        std::mem::transmute::<unsafe extern "system" fn() -> isize, NativeFlush>(function)
    });
    if matches!(request.candidate, Candidate::Denied | Candidate::Reparse) {
        let name = &request.components[0];
        let observed = relative(&root, name, MINIMAL, false, None, log)?;
        let before = Identity::of(&observed)?;
        let accepted = if request.candidate == Candidate::Reparse {
            let evidence = (|| {
                let (identity, tag) = snapshot(&observed, name, log)?;
                log.event(
                    "reparse_fixture",
                    vec![
                        ("relative_name", name.clone()),
                        ("attributes", format!("{:#x}", tag.FileAttributes)),
                        ("tag", format!("{:#x}", tag.ReparseTag)),
                        ("identity", format!("{identity:?}")),
                    ],
                )?;
                Ok(tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            })();
            let rejected = observation::reparse(evidence)?;
            log.error(
                "expected_nofollow_refusal",
                &refused("observed reparse object rejected by nofollow admission"),
            )?;
            rejected
        } else {
            inspect(&observed, name, None, log)?;
            match relative_observed(&root, name, WRITABLE, false, None, log)? {
                observation::Directory::Refused(e)
                    if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) =>
                {
                    log.error("expected_access_refusal", &e)?;
                    true
                }
                observation::Directory::Refused(e) => return Err(e),
                observation::Directory::Opened(_) => false,
            }
        };
        let after = relative(&root, name, MINIMAL, false, None, log)?;
        if Identity::of(&after)? != before {
            return Err(refused("negative fixture identity changed"));
        }
        log.event(
            "candidate_complete",
            vec![
                ("expected_refusal_observed", accepted.to_string()),
                (
                    "existing_target_unchanged",
                    "identity unchanged; no mutating request submitted".into(),
                ),
            ],
        )?;
        return Ok(accepted);
    }
    let parent_access = match request.parent_access {
        ParentAccess::Minimal => MINIMAL,
        ParentAccess::Writable => WRITABLE,
    };
    if request.candidate == Candidate::Replay {
        let mut parent = root;
        let mut parent_commit = if parent_access == MINIMAL {
            parent.try_clone()?
        } else {
            root_write
        };
        let mut path = String::new();
        let mut success = true;
        for name in &request.components {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(name);
            let child = relative(&parent, name, CHILD, false, None, log)?;
            inspect(&child, &path, Some(&principal), log)?;
            success &= flush(&child, &path, CHILD, "child_flush", log)?;
            success &= flush(
                &parent_commit,
                &format!("parent-of:{path}"),
                parent_access,
                "parent_flush",
                log,
            )?;
            // Reopen from the held parent with the requested commit mask, then
            // compare before advancing. All existing edges are replayed.
            let commit = relative(&parent, name, parent_access, false, None, log)?;
            if Identity::of(&commit)? != Identity::of(&child)? {
                return Err(refused("replay handles differ"));
            }
            parent = child;
            parent_commit = commit;
        }
        log.event(
            "candidate_complete",
            vec![
                ("all_existing_edges_replayed", "true".into()),
                ("calls_succeeded", success.to_string()),
            ],
        )?;
        return Ok(success);
    }
    let stamp = tilo_clock::try_unix_now_ns()
        .ok_or_else(|| io::Error::other("system clock is before the Unix epoch"))?;
    let namespace = format!("directory-probe-{}-{stamp}", std::process::id());
    log.event(
        "fixture_planned",
        vec![
            ("relative_name", namespace.clone()),
            ("exclusive", "true".into()),
        ],
    )?;
    let namespace_file = relative(
        &root_write,
        &namespace,
        CHILD,
        true,
        Some(principal.private_descriptor()?),
        log,
    )?;
    inspect(&namespace_file, &namespace, Some(&principal), log)?;
    // Record both the namespace's own uncertain edge and the selected child edge.
    // No successful call here upgrades the namespace into a durable authority.
    let mut success = flush(&namespace_file, &namespace, CHILD, "child_flush", log)?;
    success &= flush(&root_write, ".", WRITABLE, "parent_flush", log)?;
    let parent_min = relative(&root, &namespace, MINIMAL, false, None, log)?;
    let parent_write = relative(&root, &namespace, WRITABLE, false, None, log)?;
    if Identity::of(&parent_min)? != Identity::of(&namespace_file)?
        || Identity::of(&parent_write)? != Identity::of(&namespace_file)?
    {
        return Err(refused("fixture parent masks refer to different objects"));
    }
    let parent_commit = if parent_access == MINIMAL {
        parent_min
    } else {
        parent_write
    };
    let name = if request.candidate == Candidate::Staged {
        "stage"
    } else {
        "final"
    };
    let child = relative(
        &namespace_file,
        name,
        CHILD,
        true,
        Some(principal.private_descriptor()?),
        log,
    )?;
    let identity = inspect(
        &child,
        &format!("{namespace}/{name}"),
        Some(&principal),
        log,
    )?;
    if request.candidate == Candidate::Collision {
        let create_collision = relative_observed(
            &namespace_file,
            "final",
            CHILD,
            true,
            Some(principal.private_descriptor()?),
            log,
        );
        let expected = observation::collision(
            create_collision,
            |error| matches!(error.raw_os_error(), Some(code) if code == ERROR_ALREADY_EXISTS as i32 || code == ERROR_FILE_EXISTS as i32),
            |error| log.error("exclusive_create_collision", error),
        )?;
        let stage = relative(
            &namespace_file,
            "collision-stage",
            CHILD,
            true,
            Some(principal.private_descriptor()?),
            log,
        )?;
        let stage_identity = inspect(
            &stage,
            &format!("{namespace}/collision-stage"),
            Some(&principal),
            log,
        )?;
        let parent = Directory {
            file: namespace_file.try_clone()?,
        };
        let rename = RenameBuffer::new(
            &parent,
            &component(OsStr::new("final"))?,
            PublishMode::IfAbsent,
        )?;
        let result = rename.submit(&stage);
        let rename_expected = matches!(&result,Err(e) if e.raw_os_error()==Some(ERROR_ALREADY_EXISTS as i32)||e.raw_os_error()==Some(ERROR_FILE_EXISTS as i32)||e.raw_os_error()==Some(ERROR_ACCESS_DENIED as i32));
        log.event(
            "rename",
            vec![
                ("flags", "0".into()),
                ("destination", format!("{namespace}/final")),
                ("source", format!("{namespace}/collision-stage")),
                (
                    "win32",
                    format!(
                        "{:?}",
                        result.as_ref().err().and_then(io::Error::raw_os_error)
                    ),
                ),
                ("success", result.is_ok().to_string()),
            ],
        )?;
        let target = relative(&namespace_file, "final", MINIMAL, false, None, log)?;
        let source = relative(
            &namespace_file,
            "collision-stage",
            MINIMAL,
            false,
            None,
            log,
        )?;
        let untouched =
            Identity::of(&target)? == identity && Identity::of(&source)? == stage_identity;
        log.event(
            "collision_reconciliation",
            vec![(
                "target_and_stage_identities_unchanged",
                untouched.to_string(),
            )],
        )?;
        if !untouched {
            return Err(refused("collision target/source changed"));
        }
        success &= expected && rename_expected;
    } else if request.candidate == Candidate::Staged {
        let parent = Directory {
            file: namespace_file.try_clone()?,
        };
        let rename = RenameBuffer::new(
            &parent,
            &component(OsStr::new("final"))?,
            PublishMode::IfAbsent,
        )?;
        let now = Instant::now();
        let result = rename.submit(&child);
        log.event(
            "rename",
            vec![
                ("flags", "0".into()),
                ("source", format!("{namespace}/stage")),
                ("destination", format!("{namespace}/final")),
                ("empty_stage", "true".into()),
                ("duration_ns", now.elapsed().as_nanos().to_string()),
                (
                    "win32",
                    format!(
                        "{:?}",
                        result.as_ref().err().and_then(io::Error::raw_os_error)
                    ),
                ),
                ("success", result.is_ok().to_string()),
            ],
        )?;
        result?;
    }
    let named = relative(&namespace_file, "final", MINIMAL, false, None, log)?;
    if inspect(&named, &format!("{namespace}/final"), Some(&principal), log)? != identity
        || Identity::of(&child)? != identity
    {
        return Err(refused("final name differs from retained child"));
    }
    success &= flush(
        &child,
        &format!("{namespace}/final"),
        CHILD,
        "child_flush",
        log,
    )?;
    success &= flush(
        &parent_commit,
        &namespace,
        parent_access,
        "parent_flush",
        log,
    )?;
    log.event(
        "candidate_complete",
        vec![
            ("relative_name", format!("{namespace}/final")),
            ("calls_succeeded", success.to_string()),
            ("durability_certified", "false".into()),
        ],
    )?;
    Ok(success)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mismatched_controller_identity_refuses_before_fixture_mutation() {
        // This is a required native fixture, never silently a skipped test. The
        // controller supplies a protected owner-private, ordinary-user NTFS root.
        let root = std::env::var_os("TILO_DIRECTORY_PROBE_TEST_ROOT")
            .expect("required external disposable native probe root");
        let file = File::options()
            .access_mode(MINIMAL)
            .share_mode(SHARE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&root)
            .unwrap();
        let identity = Identity::of(&file).unwrap();
        let mut wrong = identity.id;
        wrong[0] ^= 1;
        let request = Request {
            root: root.into(),
            profile: "local-ntfs".into(),
            evidence: crate::native::atomic::directory_probe::AnchorEvidence {
                volume_serial: identity.public_volume.inner.serial,
                file_id: wrong,
                establishment_record: "deliberately mismatched test evidence".into(),
                storage_profile: "externally provisioned ordinary-user disposable NTFS fixture"
                    .into(),
            },
            candidate: Candidate::Direct,
            parent_access: ParentAccess::Writable,
            components: vec![],
        };
        let mut events = Vec::new();
        let outcome = run(&request, &mut |event| {
            events.push(event.clone());
            Ok(())
        });
        assert!(!outcome.candidate_calls_succeeded);
        assert!(
            events
                .iter()
                .any(|event| event.operation == "anchor_comparison"
                    && event.fields.get("matches").is_some_and(|v| v == "false")),
            "native prerequisite missing or did not reach actual mismatch: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| event.operation == "create" || event.operation == "rename")
        );
    }
}
