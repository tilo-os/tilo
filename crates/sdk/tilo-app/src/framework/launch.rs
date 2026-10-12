//! What a launch asks an app to open, and the commands its binary runs
//! instead of opening a window.
//!
//! A launch names things beside the app: a file a person opened with it, the
//! paths or URLs of its command line. The app answers in two steps, each
//! through a concept it already has. Which record the launch opens is a
//! [`RecordId`](ui_core::record::RecordId)
//! ([`ResourceApp::launch_record`](super::ResourceApp::launch_record)): the
//! file's own record ([`RecordId::identify_file`](ui_core::record::RecordId::identify_file)),
//! a record derived from it, a minted one. What the launch then does in that
//! record is the resource's actions
//! ([`ResourceApp::launch`](super::ResourceApp::launch)), submitted through
//! the same door as every control, so they are checked, attributed to the
//! participant, undoable and in history.
//!
//! An app's own command-line options
//! ([`ResourceApp::OPTIONS`](super::ResourceApp::OPTIONS),
//! [`ResourceApp::FLAGS`](super::ResourceApp::FLAGS)) say how this window
//! runs rather than what it opens: the layout an on-screen keyboard starts
//! in, a debug overlay. The launch hands them over beside the arguments, and
//! views and surfaces read them where they set themselves up
//! ([`RecordEnv::launch_option`](super::RecordEnv::launch_option)).

use ui_core::application::{LaunchOption, OpenFile, PlaybackHandoff};

/// What a launch handed the app: the arguments of its command line or of an
/// "open with" (paths, URLs), the app's own options it named, the file a
/// host lent it, the content store where the app keeps what a launch hands
/// it, and where playback of the media it names stood on the device or app
/// that handed it over.
#[derive(Clone, Copy)]
pub struct LaunchInput<'a> {
    arguments: &'a [String],
    options: &'a [LaunchOption],
    file: Option<&'a OpenFile>,
    content: Option<&'a tilo_content::ContentStore>,
    handoff: Option<&'a PlaybackHandoff>,
    record: Option<&'a dyn std::any::Any>,
}

impl<'a> LaunchInput<'a> {
    /// A launch naming `arguments` and handing over `file`: what a host
    /// asks [`ResourceApp::launch`](super::ResourceApp::launch) about, and
    /// what a test of an app's launch builds.
    pub fn new(arguments: &'a [String], file: Option<&'a OpenFile>) -> Self {
        Self {
            arguments,
            options: &[],
            file,
            content: None,
            handoff: None,
            record: None,
        }
    }

    /// The same launch on a record that is already there, as it is now.
    #[must_use]
    pub fn with_record(mut self, state: &'a dyn std::any::Any) -> Self {
        self.record = Some(state);
        self
    }

    /// The state of the record the launch opened, as it is when the launch
    /// is taken: `S` is the app resource's state. `None` before the record
    /// is open (a [`genesis`](super::ResourceApp::genesis)) or for another
    /// type.
    pub fn record<S: 'static>(&self) -> Option<&'a S> {
        self.record?.downcast_ref::<S>()
    }

    /// The same launch on a host that lends the app `content`.
    #[must_use]
    pub fn with_content(mut self, content: Option<&'a tilo_content::ContentStore>) -> Self {
        self.content = content;
        self
    }

    /// The same launch, continuing `handoff`'s playback.
    #[must_use]
    pub fn with_handoff(mut self, handoff: Option<&'a PlaybackHandoff>) -> Self {
        self.handoff = handoff;
        self
    }

    /// This launch, also naming the app's own `options`.
    #[must_use]
    pub fn with_options(mut self, options: &'a [LaunchOption]) -> Self {
        self.options = options;
        self
    }

    /// The launch's arguments, in order; at most
    /// [`ResourceApp::ARGUMENTS`](super::ResourceApp::ARGUMENTS) of them.
    pub fn arguments(&self) -> &'a [String] {
        self.arguments
    }

    /// The first argument, for an app that takes one (a path, a URL).
    pub fn argument(&self) -> Option<&'a str> {
        self.arguments.first().map(String::as_str)
    }

    /// The file a host handed over to open: its name, media type and bytes.
    /// Only a file of a type the app
    /// [opens](super::ResourceApp::OPENS) reaches it.
    pub fn file(&self) -> Option<&'a OpenFile> {
        self.file
    }

    /// The content store of the host the app is launched in: a handed
    /// file's bytes are kept there, and the launch's actions name them by
    /// hash, so the record never carries a file and every device that holds
    /// the bytes reads them. `None` where the host lends none.
    pub fn content(&self) -> Option<&'a tilo_content::ContentStore> {
        self.content
    }

    /// Where playback of the media the launch names stood on the device or
    /// app that handed it over: the position to continue from, whether it
    /// keeps playing and what it was called there. `None` for an ordinary
    /// launch, which starts at the beginning.
    pub fn handoff(&self) -> Option<&'a PlaybackHandoff> {
        self.handoff
    }

    /// The value of the app's option `name` (`--purpose`), when the launch
    /// named it; see [`ResourceApp::OPTIONS`](super::ResourceApp::OPTIONS).
    pub fn option(&self, name: &str) -> Option<&'a str> {
        option(self.options, name)
    }

    /// Whether the launch named the app's switch `name`; see
    /// [`ResourceApp::FLAGS`](super::ResourceApp::FLAGS).
    pub fn flag(&self, name: &str) -> bool {
        flag(self.options, name)
    }

    /// Whether the launch named nothing: a plain start of the app.
    pub fn is_empty(&self) -> bool {
        self.arguments.is_empty() && self.options.is_empty() && self.file.is_none()
    }
}

pub(crate) fn option<'a>(options: &'a [LaunchOption], name: &str) -> Option<&'a str> {
    options
        .iter()
        .find(|option| option.name == name)
        .and_then(|option| option.value.as_deref())
}

pub(crate) fn flag(options: &[LaunchOption], name: &str) -> bool {
    options
        .iter()
        .any(|option| option.name == name && option.value.is_none())
}

/// A command the app's binary runs instead of opening a window: a developer
/// utility (writing fixtures, staging a repository) that shares the app's
/// code but is not part of using it.
#[derive(Clone, Copy, Debug)]
pub struct Command {
    /// The option that runs the command, taking one value: `--emit-photos`.
    pub option: &'static str,
    /// Runs the command with the option's value. The process exits after it,
    /// reporting the error, if any.
    pub run: fn(&str) -> Result<(), String>,
}

/// A helper the app's binary is besides the app: a privileged writer, a
/// staging step, a packaging probe. Asked first, with the process's whole
/// command line, before anything of the app starts (no window, no session,
/// no portal), it answers `None` for a command line that is not its own,
/// else runs and answers the exit code the process ends with. A refused
/// command line is an error, which ends the process with code 1.
#[derive(Clone, Copy)]
pub struct Helper {
    /// Runs the helper if `args` (the whole command line, the binary's path
    /// first) is its own.
    pub run: fn(&[String]) -> Result<Option<i32>, String>,
}

/// Opens what a launch naming some arguments opens in the window an app runs
/// in, in place of the record it shows: what
/// [`RecordEnv::open_here`](super::RecordEnv::open_here) does, kept by a custom
/// surface for the input that later asks for it (a file chosen in a picker).
#[derive(Clone)]
pub struct OpenHere(std::rc::Rc<dyn Fn(Vec<String>) -> ui_core::application::ReplacementOutcome>);

impl OpenHere {
    pub(crate) fn new(
        open: impl Fn(Vec<String>) -> ui_core::application::ReplacementOutcome + 'static,
    ) -> Self {
        Self(std::rc::Rc::new(open))
    }

    /// Ask for what a launch naming `arguments` opens, in the window's
    /// place. The host opens it after the event that asked; the outcome
    /// answers whether the window shows it, or why not — the record is
    /// unavailable, the file unreadable — so the asker can answer the
    /// operation that asked and tell the person.
    pub fn open(&self, arguments: &[String]) -> ui_core::application::ReplacementOutcome {
        (self.0)(arguments.to_vec())
    }
}

impl std::fmt::Debug for OpenHere {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenHere")
    }
}
