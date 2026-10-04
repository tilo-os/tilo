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
//! ([`ViewCx::launch_option`](super::ViewCx::launch_option),
//! [`SurfaceCx::launch_option`](super::SurfaceCx::launch_option)).

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
        }
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

/// Split a command line into its arguments and the app's own `options`
/// (each taking the next word, or written `--name=value`) and `flags`, as
/// the launcher reads it: a word naming neither is an argument.
pub(crate) fn split_command_line(
    words: &[String],
    options: &[&str],
    flags: &[&str],
) -> (Vec<String>, Vec<LaunchOption>) {
    let mut arguments = Vec::new();
    let mut named = Vec::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if flags.contains(&word.as_str()) {
            named.push(LaunchOption {
                name: word.clone(),
                value: None,
            });
        } else if let Some((name, value)) = word.split_once('=')
            && options.contains(&name)
        {
            named.push(LaunchOption {
                name: name.to_owned(),
                value: Some(value.to_owned()),
            });
        } else if options.contains(&word.as_str()) {
            named.push(LaunchOption {
                name: word.clone(),
                value: Some(words.next().cloned().unwrap_or_default()),
            });
        } else {
            arguments.push(word.clone());
        }
    }
    (arguments, named)
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
