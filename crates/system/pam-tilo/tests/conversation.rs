//! `pam_tilo` loaded by the real libpam from a service file in a temporary
//! directory, talking to a stand-in for `tilo-authd`: what it asks, what it
//! sends, what the person sees and what the stack makes of the answer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use serde_json::{Value, json};
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const PAM_SUCCESS: c_int = 0;
const PAM_PERM_DENIED: c_int = 6;
const PAM_AUTH_ERR: c_int = 7;
const PAM_MAXTRIES: c_int = 11;
const PAM_AUTHTOK_ERR: c_int = 20;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_ERROR_MSG: c_int = 3;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn =
    extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

type StartConfdir = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const PamConv,
    *const c_char,
    *mut *mut c_void,
) -> c_int;
type Phase = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type End = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;

struct Libpam {
    start_confdir: StartConfdir,
    authenticate: Phase,
    chauthtok: Phase,
    end: End,
}

fn libpam() -> Libpam {
    // SAFETY: loading the system's libpam; the handle is kept for the
    // process's life.
    let handle = unsafe { libc::dlopen(c"libpam.so.0".as_ptr(), libc::RTLD_NOW) };
    assert!(!handle.is_null(), "libpam.so.0 is not installed");
    let sym = |name: &CStr| {
        // SAFETY: a live handle and a NUL-terminated name.
        let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
        assert!(!ptr.is_null(), "libpam has no {name:?}");
        ptr
    };
    // SAFETY: each symbol has the signature Linux-PAM documents.
    unsafe {
        Libpam {
            start_confdir: std::mem::transmute::<*mut c_void, StartConfdir>(sym(
                c"pam_start_confdir",
            )),
            authenticate: std::mem::transmute::<*mut c_void, Phase>(sym(c"pam_authenticate")),
            chauthtok: std::mem::transmute::<*mut c_void, Phase>(sym(c"pam_chauthtok")),
            end: std::mem::transmute::<*mut c_void, End>(sym(c"pam_end")),
        }
    }
}

/// The person at the screen: answers each prompt in turn and notes what
/// they were shown.
#[derive(Default)]
struct Person {
    answers: VecDeque<String>,
    prompts: Vec<String>,
    errors: Vec<String>,
}

extern "C" fn converse(
    count: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    // SAFETY: appdata is the `Mutex<Person>` the test passed to pam_start.
    let person = unsafe { &*appdata.cast::<Mutex<Person>>() };
    let mut person = person.lock().unwrap();
    let n = usize::try_from(count).unwrap();
    // SAFETY: libpam frees the array and each answer with free().
    let out = unsafe { libc::calloc(n, std::mem::size_of::<PamResponse>()) }.cast::<PamResponse>();
    for i in 0..n {
        // SAFETY: libpam passes `count` messages.
        let message = unsafe { &**messages.add(i) };
        // SAFETY: each message text is NUL-terminated.
        let text = unsafe { CStr::from_ptr(message.msg) }
            .to_string_lossy()
            .into_owned();
        match message.msg_style {
            PAM_PROMPT_ECHO_OFF | 2 => {
                person.prompts.push(text);
                let answer = CString::new(person.answers.pop_front().unwrap_or_default()).unwrap();
                // SAFETY: `out` has room for `count` responses.
                unsafe {
                    (*out.add(i)).resp = libc::strdup(answer.as_ptr());
                }
            }
            PAM_ERROR_MSG => person.errors.push(text),
            _ => {}
        }
    }
    drop(person);
    // SAFETY: the out-pointer libpam passed.
    unsafe { *responses = out };
    PAM_SUCCESS
}

/// A stand-in for tilo-authd: answers each request with `answer` and keeps
/// what it was asked.
struct Daemon {
    socket: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Daemon {
    fn start(dir: &Path, answer: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        let socket = dir.join("socket");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let mut writer = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let reply = answer(&request);
                    seen.lock().unwrap().push(request);
                    writeln!(writer, "{reply}").unwrap();
                }
            }
        });
        Self { socket, requests }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

fn module() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let module = dir.join("libpam_tilo.so");
    assert!(module.exists(), "{} was not built", module.display());
    module
}

struct Outcome {
    status: c_int,
    person: Person,
}

/// Run one PAM phase of a service whose stack is `lines`, with `@MODULE@`
/// and `@SOCKET@` filled in, for `user`.
fn run(
    dir: &Path,
    socket: &Path,
    lines: &str,
    user: &str,
    answers: &[&str],
    phase: fn(&Libpam) -> Phase,
) -> Outcome {
    let confdir = dir.join("pam.d");
    std::fs::create_dir_all(&confdir).unwrap();
    let stack = lines
        .replace("@MODULE@", &module().display().to_string())
        .replace("@SOCKET@", &socket.display().to_string());
    std::fs::write(confdir.join("test"), stack).unwrap();

    let pam = libpam();
    let person = Mutex::new(Person {
        answers: answers.iter().map(|a| (*a).to_owned()).collect(),
        ..Person::default()
    });
    let conv = PamConv {
        conv: converse,
        appdata_ptr: std::ptr::from_ref(&person).cast_mut().cast(),
    };
    let service = c"test";
    let user = CString::new(user).unwrap();
    let confdir_c = CString::new(confdir.display().to_string()).unwrap();
    let mut handle: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid strings and conversation, and an out-pointer.
    let rc = unsafe {
        (pam.start_confdir)(
            service.as_ptr(),
            user.as_ptr(),
            &raw const conv,
            confdir_c.as_ptr(),
            &raw mut handle,
        )
    };
    assert_eq!(rc, PAM_SUCCESS, "pam_start_confdir");
    // SAFETY: a handle from pam_start.
    let status = unsafe { (phase(&pam))(handle, 0) };
    // SAFETY: ends the handle started above.
    unsafe { (pam.end)(handle, status) };
    Outcome {
        status,
        person: person.into_inner().unwrap(),
    }
}

fn authenticate(pam: &Libpam) -> Phase {
    pam.authenticate
}

fn chauthtok(pam: &Libpam) -> Phase {
    pam.chauthtok
}

/// The shipped shape: the daemon's answer is final, and only when it cannot
/// answer does the rest of the stack (here, a module that accepts anything)
/// get a say.
const SIGN_IN: &str = "\
auth [success=done authinfo_unavail=ignore ignore=ignore default=die] @MODULE@ socket=@SOCKET@
auth required pam_permit.so
";

fn verify_answer(answer: Value) -> impl Fn(&Value) -> Value + Send + Sync + 'static {
    move |request| {
        assert_eq!(request["op"], "verify");
        answer.clone()
    }
}

#[test]
fn the_right_secret_signs_in_after_one_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(dir.path(), verify_answer(json!({"ok": true})));
    let out = run(
        dir.path(),
        &daemon.socket,
        SIGN_IN,
        "ada",
        &["123456"],
        authenticate,
    );
    assert_eq!(out.status, PAM_SUCCESS);
    assert_eq!(out.person.prompts.len(), 1);
    assert_eq!(
        daemon.requests(),
        [json!({"op": "verify", "user": "ada", "secret": "123456"})]
    );
}

#[test]
fn a_wrong_secret_is_final_and_says_how_many_tries_are_left() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        dir.path(),
        verify_answer(json!({"ok": false, "error": "wrong", "tries_left": 4, "retry_in_s": 7200})),
    );
    let out = run(
        dir.path(),
        &daemon.socket,
        SIGN_IN,
        "ada",
        &["000000"],
        authenticate,
    );
    assert_eq!(
        out.status, PAM_AUTH_ERR,
        "the module after it must not get a say"
    );
    assert_eq!(
        out.person.errors,
        ["That isn't it. 4 tries left before a wait."]
    );
}

#[test]
fn a_locked_out_tpm_says_when_to_try_again() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        dir.path(),
        verify_answer(
            json!({"ok": false, "error": "locked-out", "tries_left": 0, "retry_in_s": 7200}),
        ),
    );
    let out = run(
        dir.path(),
        &daemon.socket,
        SIGN_IN,
        "ada",
        &["123456"],
        authenticate,
    );
    assert_eq!(out.status, PAM_MAXTRIES);
    assert_eq!(
        out.person.errors,
        ["Too many tries. The next try is possible in 120 min, or use the recovery key."]
    );
}

#[test]
fn asking_about_someone_else_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        dir.path(),
        verify_answer(json!({"ok": false, "error": "not-permitted"})),
    );
    let out = run(
        dir.path(),
        &daemon.socket,
        SIGN_IN,
        "bob",
        &["123456"],
        authenticate,
    );
    assert_eq!(out.status, PAM_PERM_DENIED);
}

#[test]
fn without_the_daemon_the_rest_of_the_stack_decides_and_nothing_is_asked_twice() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        dir.path(),
        &dir.path().join("no-such-socket"),
        SIGN_IN,
        "ada",
        &[],
        authenticate,
    );
    assert_eq!(out.status, PAM_SUCCESS);
    assert!(
        out.person.prompts.is_empty(),
        "the module asked before knowing it could check"
    );
}

#[test]
fn someone_the_daemon_does_not_know_is_left_to_the_rest_of_the_stack() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(
        dir.path(),
        verify_answer(json!({"ok": false, "error": "not-enrolled"})),
    );
    let out = run(
        dir.path(),
        &daemon.socket,
        SIGN_IN,
        "ldap-user",
        &["x"],
        authenticate,
    );
    assert_eq!(out.status, PAM_SUCCESS);
}

const GREETER: &str = "\
auth sufficient @MODULE@ socket=@SOCKET@ boot_sign_in
auth [success=done authinfo_unavail=ignore ignore=ignore default=die] @MODULE@ socket=@SOCKET@
";

#[test]
fn the_boot_sign_in_starts_the_session_without_asking_again() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(dir.path(), |request| {
        assert_eq!(request["op"], "claim-boot-sign-in");
        json!({"ok": true})
    });
    let out = run(
        dir.path(),
        &daemon.socket,
        GREETER,
        "ada",
        &[],
        authenticate,
    );
    assert_eq!(out.status, PAM_SUCCESS);
    assert!(out.person.prompts.is_empty());
    assert_eq!(
        daemon.requests(),
        [json!({"op": "claim-boot-sign-in", "user": "ada"})]
    );
}

#[test]
fn without_a_boot_sign_in_the_greeter_asks_for_the_secret() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(dir.path(), |request| match request["op"].as_str() {
        Some("claim-boot-sign-in") => json!({"ok": false, "error": "no-boot-sign-in"}),
        Some("verify") => json!({"ok": true}),
        other => panic!("unexpected {other:?}"),
    });
    let out = run(
        dir.path(),
        &daemon.socket,
        GREETER,
        "ada",
        &["123456"],
        authenticate,
    );
    assert_eq!(out.status, PAM_SUCCESS);
    assert_eq!(out.person.prompts.len(), 1);
    assert_eq!(daemon.requests().len(), 2);
}

const PASSWORD: &str = "\
password [success=done authinfo_unavail=ignore ignore=ignore default=die] @MODULE@ socket=@SOCKET@
password required pam_permit.so
";

#[test]
fn changing_the_secret_proves_the_current_one_and_asks_for_the_new_one_twice() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(dir.path(), |request| match request["op"].as_str() {
        Some("status") => json!({"ok": true, "enrolled": true, "factors": ["password"]}),
        Some("set-secret") => json!({"ok": true}),
        other => panic!("unexpected {other:?}"),
    });
    let out = run(
        dir.path(),
        &daemon.socket,
        PASSWORD,
        "ada",
        &["123456", "2468", "2468"],
        chauthtok,
    );
    assert_eq!(out.status, PAM_SUCCESS, "{:?}", out.person.errors);
    assert_eq!(out.person.prompts.len(), 3);
    let set: Vec<Value> = daemon
        .requests()
        .into_iter()
        .filter(|r| r["op"] == "set-secret")
        .collect();
    assert_eq!(
        set,
        [json!({"op": "set-secret", "user": "ada", "current": "123456", "new": "2468"})]
    );
}

#[test]
fn a_new_secret_too_weak_for_a_machine_without_a_tpm_is_refused_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(dir.path(), |request| match request["op"].as_str() {
        Some("status") => json!({"ok": true, "enrolled": true, "factors": ["password"]}),
        Some("set-secret") => json!({"ok": false, "error": "weak-secret"}),
        other => panic!("unexpected {other:?}"),
    });
    let out = run(
        dir.path(),
        &daemon.socket,
        PASSWORD,
        "ada",
        &["correct horse battery staple", "1234", "1234"],
        chauthtok,
    );
    assert_eq!(out.status, PAM_AUTHTOK_ERR);
    assert_eq!(out.person.errors.len(), 1);
    assert!(out.person.errors[0].contains("has to be longer"));
}

#[test]
fn someone_still_on_the_old_password_changes_it_in_the_rest_of_the_stack() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(dir.path(), |request| {
        assert_eq!(request["op"], "status", "nothing but a status question");
        json!({"ok": true, "enrolled": false})
    });
    let out = run(dir.path(), &daemon.socket, PASSWORD, "ada", &[], chauthtok);
    assert_eq!(out.status, PAM_SUCCESS);
    assert!(out.person.prompts.is_empty());
}
