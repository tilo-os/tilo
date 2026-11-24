//! Starting a process with its link on Windows.
//!
//! The process inherits exactly the handles named for it: its end of the link, the null device
//! as its input and output, and the pipe its error output goes to, listed in its start-up
//! attributes (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), so no other handle this process holds
//! reaches it. It is created suspended; this process holds it and checks it is the one it
//! created before the link names it and it runs. Every handle the link carries afterwards is
//! duplicated into that process alone (`crate::packet`).
//!
//! A confined start ([`Confinement`]) also runs the process in an `AppContainer`, whose token
//! reaches only what was granted to the container's SID, and puts it in a job before it runs, so
//! the job's holder ends its whole tree.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, BorrowedHandle, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::ExitStatusExt as _;
use std::process::{Command, ExitStatus};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::{PSID, SECURITY_CAPABILITIES};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, GetProcessId,
    INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::FD_ENV;
use crate::packet::Descriptor;

/// A process [`super::spawn`] started.
#[derive(Debug)]
pub struct Child {
    process: Arc<OwnedHandle>,
    id: u32,
    /// Its error output, as it writes it.
    pub stderr: Option<std::fs::File>,
}

impl Child {
    /// Its process id.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// Wait for it to end, and how it ended.
    ///
    /// # Errors
    /// The system's error waiting for it.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        // SAFETY: the process handle this child holds.
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0;
        // SAFETY: the process handle and a valid output pointer.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &raw mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ExitStatus::from_raw(code))
    }

    /// End it now.
    ///
    /// # Errors
    /// The system's error ending it.
    pub fn kill(&mut self) -> io::Result<()> {
        // SAFETY: the process handle this child holds.
        if unsafe { TerminateProcess(self.process.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Whose handles are inheritable now: one start at a time marks its own, so no other start of
/// this process's takes them. (A start through `std::process::Command` passes every inheritable
/// handle; none of this process's spawns go through it while one of these runs.)
static STARTING: Mutex<()> = Mutex::new(());

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle the call just returned, owned from here.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn inheritable(handle: &OwnedHandle, inherit: bool) -> io::Result<()> {
    let flag = if inherit { HANDLE_FLAG_INHERIT } else { 0 };
    // SAFETY: a live handle.
    if unsafe { SetHandleInformation(handle.as_raw_handle(), HANDLE_FLAG_INHERIT, flag) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let wide: Vec<u16> = text.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a program, argument or variable holds a NUL",
        ));
    }
    Ok(wide)
}

/// `argument` as the C runtime splits a command line back into it.
fn quote(argument: &OsStr, line: &mut Vec<u16>) -> io::Result<()> {
    let argument = wide(argument)?;
    let [space, tab, quote, backslash] = [b' ', b'\t', b'"', b'\\'].map(u16::from);
    if !argument.is_empty() && !argument.iter().any(|c| [space, tab, quote].contains(c)) {
        line.extend(argument);
        return Ok(());
    }
    line.push(quote);
    let mut backslashes = 0_usize;
    for unit in argument {
        if unit == backslash {
            backslashes = backslashes.saturating_add(1);
        } else {
            if unit == quote {
                // Each backslash before a quote doubles, and the quote is escaped.
                line.extend(std::iter::repeat_n(
                    backslash,
                    backslashes.saturating_add(1),
                ));
            }
            backslashes = 0;
        }
        line.push(unit);
    }
    // Backslashes before the closing quote double.
    line.extend(std::iter::repeat_n(backslash, backslashes));
    line.push(quote);
    Ok(())
}

/// The command line `command` runs.
fn command_line(command: &Command) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    quote(command.get_program(), &mut line)?;
    for argument in command.get_args() {
        line.push(u16::from(b' '));
        quote(argument, &mut line)?;
    }
    line.push(0);
    Ok(line)
}

/// The environment `command` runs in, with its link named, as a block `CreateProcessW` reads.
fn environment(command: &Command, link: &OwnedHandle) -> io::Result<Vec<u16>> {
    let mut variables: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let link_value = OsString::from((link.as_raw_handle() as usize).to_string());
    let changes = command
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
        .chain([(OsString::from(FD_ENV), Some(link_value))]);
    for (name, value) in changes {
        // Windows compares variable names without case.
        variables.retain(|(held, _)| !held.eq_ignore_ascii_case(&name));
        if let Some(value) = value {
            variables.push((name, value));
        }
    }
    let mut block = Vec::new();
    for (name, value) in variables {
        block.extend(wide(&name)?);
        block.push(u16::from(b'='));
        block.extend(wide(&value)?);
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// The null device, inheritable once marked, for the input and output the process has none of.
fn null_device() -> io::Result<OwnedHandle> {
    let name: Vec<u16> = "NUL".encode_utf16().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated name and no security attributes.
    owned(unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    })
}

/// Where a started process is confined: the `AppContainer` its token is made for, and the job
/// it is put in before it runs.
pub struct Confinement<'a> {
    /// The container's SID, as the system wrote it: the only identity the process's token
    /// carries beyond its user's, and the one every grant to it names.
    pub container: &'a [u8],
    /// The job the process and every process it starts run in.
    pub job: BorrowedHandle<'a>,
}

/// Start-up attributes naming the only handles the process inherits, and the container it runs
/// in when it is confined.
struct Attributes {
    list: Vec<usize>,
    handles: Vec<HANDLE>,
    /// What the container attribute points at, which lives as long as the list.
    capabilities: Option<Box<SECURITY_CAPABILITIES>>,
}

impl Attributes {
    fn new(handles: Vec<HANDLE>, container: Option<&[u8]>) -> io::Result<Self> {
        let count = if container.is_some() { 2 } else { 1 };
        let mut size = 0;
        // SAFETY: asks only for the size of a list of `count` attributes.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &raw mut size) };
        let mut list = vec![0_usize; size.div_ceil(size_of::<usize>())];
        // SAFETY: a buffer of the size asked for, aligned for the list.
        if unsafe {
            InitializeProcThreadAttributeList(list.as_mut_ptr().cast(), count, 0, &raw mut size)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut attributes = Self {
            list,
            handles,
            capabilities: container.map(|sid| {
                Box::new(SECURITY_CAPABILITIES {
                    AppContainerSid: sid.as_ptr().cast_mut().cast::<std::ffi::c_void>() as PSID,
                    Capabilities: std::ptr::null_mut(),
                    CapabilityCount: 0,
                    Reserved: 0,
                })
            }),
        };
        // SAFETY: an initialized list, and the handles it names, which live as long as it does.
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.as_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                attributes.handles.as_ptr().cast(),
                size_of_val(attributes.handles.as_slice()),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if let Some(capabilities) = attributes.capabilities.as_mut() {
            // SAFETY: the list has room for a second attribute; the capabilities it points at
            // (and the SID they name, which the caller holds through the start) outlive it. No
            // capability is granted: the container reaches no network and no library folder.
            if unsafe {
                UpdateProcThreadAttribute(
                    attributes.list.as_ptr().cast_mut().cast(),
                    0,
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    std::ptr::from_mut::<SECURITY_CAPABILITIES>(capabilities).cast(),
                    size_of::<SECURITY_CAPABILITIES>(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            } == 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "this Windows cannot confine an app in an AppContainer: {}",
                        io::Error::last_os_error()
                    ),
                ));
            }
        }
        Ok(attributes)
    }

    fn as_ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.list.as_ptr().cast_mut().cast()
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: a list `new` initialized.
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}

/// Start `command` with the other end of a new link, named by [`FD_ENV`], confined as
/// `confinement` says when it says.
pub(super) fn spawn(
    command: &Command,
    confinement: Option<&Confinement<'_>>,
) -> io::Result<(Descriptor, Child)> {
    let (ours, theirs) = tilo_socket::link::pair()?;
    let theirs = theirs.into_handle();
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: valid output pointers, no attributes: neither end is inheritable yet.
    if unsafe { CreatePipe(&raw mut read, &raw mut write, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let (stderr, stderr_write) = (owned(read)?, owned(write)?);
    let null = null_device()?;
    let mut line = command_line(command)?;
    let environment = environment(command, &theirs)?;
    let directory = command
        .get_current_dir()
        .map(|directory| wide(directory.as_os_str()).map(|wide| [wide, vec![0]].concat()))
        .transpose()?;
    let inherited = [&theirs, &stderr_write, &null];
    let attributes = Attributes::new(
        inherited
            .iter()
            .map(|handle| handle.as_raw_handle())
            .collect(),
        confinement.map(|confinement| confinement.container),
    )?;
    // SAFETY: STARTUPINFOEXW and PROCESS_INFORMATION are plain data, valid zeroed.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap_or(u32::MAX);
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = null.as_raw_handle();
    startup.StartupInfo.hStdOutput = null.as_raw_handle();
    startup.StartupInfo.hStdError = stderr_write.as_raw_handle();
    startup.lpAttributeList = attributes.as_ptr();
    // SAFETY: as above.
    let mut started: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    {
        let _starting = STARTING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for handle in inherited {
            inheritable(handle, true)?;
        }
        // SAFETY: a NUL-terminated, writable command line; an environment block and directory
        // that outlive the call; start-up information whose attribute list names live handles.
        let created = unsafe {
            CreateProcessW(
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_SUSPENDED
                    | EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_UNICODE_ENVIRONMENT
                    | CREATE_NO_WINDOW,
                environment.as_ptr().cast(),
                directory
                    .as_ref()
                    .map_or(std::ptr::null(), |directory| directory.as_ptr()),
                &raw const startup.StartupInfo,
                &raw mut started,
            )
        };
        let error = io::Error::last_os_error();
        // The process holds its own copies now: this process closes its own before any other
        // start could take them.
        drop(attributes);
        drop((theirs, stderr_write, null));
        if created == 0 {
            return Err(error);
        }
    }
    let process = Arc::new(owned(started.hProcess)?);
    let thread = owned(started.hThread)?;
    // The process is the one created, before anything names it: its id answers from its handle.
    // SAFETY: the process handle CreateProcessW returned.
    if unsafe { GetProcessId(process.as_raw_handle()) } != started.dwProcessId {
        // SAFETY: as above; it never ran.
        unsafe { TerminateProcess(process.as_raw_handle(), 1) };
        return Err(io::Error::other("a started process is not the one created"));
    }
    if let Some(confinement) = confinement {
        // SAFETY: the job and the suspended process's handles, both live.
        if unsafe {
            AssignProcessToJobObject(confinement.job.as_raw_handle(), process.as_raw_handle())
        } == 0
        {
            let error = io::Error::last_os_error();
            // SAFETY: as above; it never ran.
            unsafe { TerminateProcess(process.as_raw_handle(), 1) };
            return Err(error);
        }
    }
    let link = Descriptor::to_started(ours, Arc::clone(&process));
    let child = Child {
        process,
        id: started.dwProcessId,
        stderr: Some(std::fs::File::from(stderr)),
    };
    // SAFETY: the suspended process's first thread.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        let error = io::Error::last_os_error();
        // SAFETY: the process handle; it never ran.
        unsafe { TerminateProcess(child.process.as_raw_handle(), 1) };
        return Err(error);
    }
    Ok((link, child))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixtures")]
mod tests {
    use super::*;

    fn split(line: &[u16]) -> String {
        String::from_utf16(line.strip_suffix(&[0]).unwrap()).unwrap()
    }

    #[test]
    fn a_command_line_reads_back_as_its_arguments() {
        let mut command = Command::new(r"C:\Program Files\tilo\tilo-launcher.exe");
        command.args(["plain", "", "two words", r#"say "hi""#, r"ends\", r#"a\"b"#]);
        assert_eq!(
            split(&command_line(&command).unwrap()),
            r#""C:\Program Files\tilo\tilo-launcher.exe" plain "" "two words" "say \"hi\"" ends\ "a\\\"b""#
        );
        let mut command = Command::new("x");
        command.arg(r"trailing\ space\");
        assert_eq!(
            split(&command_line(&command).unwrap()),
            r#"x "trailing\ space\\""#
        );
        let mut command = Command::new("x");
        command.arg("nul\0");
        assert!(command_line(&command).is_err());
    }

    /// Set on the copy of this test binary the test below starts.
    const STARTED: &str = "TILO_TEST_STARTED_PROCESS";

    /// Started as a process with its link: the test binary runs this test again, which plays
    /// the started process when it is one.
    #[test]
    fn a_started_process_takes_its_link_and_the_handles_on_it() {
        use crate::process_link::{FromProcess, Packet as _, ToProcess, take};
        use std::io::{Read as _, Write as _};
        if std::env::var_os(STARTED).is_some() {
            let link = take().expect("the link this process was handed");
            assert!(take().is_none(), "a link is taken once");
            let Some(ToProcess::Patch { instance: 7, patch }) = ToProcess::receive(&link).unwrap()
            else {
                panic!("the patch the starter sent");
            };
            (&patch.into_stream().unwrap())
                .write_all(b"through the handle")
                .unwrap();
            let (_ours, theirs) = crate::packet::pair().unwrap();
            assert!(
                ToProcess::Patch {
                    instance: 1,
                    patch: theirs
                }
                .send(&link)
                .is_err(),
                "a started process hands its starter no handles"
            );
            FromProcess::Ended(7).send(&link).unwrap();
            eprintln!("said on its error output");
            return;
        }
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process_link::windows::tests::a_started_process_takes_its_link_and_the_handles_on_it",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(STARTED, "1");
        let (link, mut child) = super::spawn(&command, None).unwrap();
        let (ours, theirs) = crate::packet::pair().unwrap();
        ToProcess::Patch {
            instance: 7,
            patch: theirs,
        }
        .send(&link)
        .unwrap();
        let mut said = String::new();
        let mut stderr = child.stderr.take().unwrap();
        assert_eq!(
            FromProcess::receive(&link).unwrap(),
            Some(FromProcess::Ended(7))
        );
        let mut through = [0_u8; 18];
        (&ours.into_stream().unwrap())
            .read_exact(&mut through)
            .unwrap();
        assert_eq!(
            &through, b"through the handle",
            "the handle reached the started process"
        );
        assert!(child.wait().unwrap().success());
        stderr.read_to_string(&mut said).unwrap();
        assert!(said.contains("said on its error output"), "{said}");
        assert_eq!(
            FromProcess::receive(&link).unwrap(),
            None,
            "its end closed as it ended"
        );
        assert!(super::super::hung_up(&link));
    }

    #[test]
    fn the_environment_names_the_link_once() {
        let (_ours, theirs) = tilo_socket::link::pair().unwrap();
        let theirs = theirs.into_handle();
        let mut command = Command::new("x");
        command
            .env("tilo_svc_process_fd", "stale")
            .env("TILO_KEPT", "yes");
        let block = environment(&command, &theirs).unwrap();
        let text = String::from_utf16(&block).unwrap();
        let variables: Vec<&str> = text.split('\0').filter(|line| !line.is_empty()).collect();
        let named: Vec<&&str> = variables
            .iter()
            .filter(|line| line.to_ascii_uppercase().starts_with(FD_ENV))
            .collect();
        assert_eq!(
            named,
            [&format!("{FD_ENV}={}", theirs.as_raw_handle() as usize).as_str()]
        );
        assert!(variables.contains(&"TILO_KEPT=yes"));
    }
}
