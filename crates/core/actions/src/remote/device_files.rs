//! The workspace's own folder, as the device serves it: a file put into it, and Files' verbs on
//! it by paths in it.
//!
//! This device holds the tree, so [`OPEN`] on [`FOLDER`] takes a file's `content` or many at once
//! as `files` and answers where each file is, and the verbs that change the folder by paths in it
//! are its actions beside the open. Each is decided by the workspace's grants for the application
//! that asks, exactly as Files' own actions are.
//!
//! An app calls by address and never names the device: the daemon that admitted its connection
//! is the device whose folder answers.

use serde_json::{Map, Value};

use super::{InvokeArgument, InvokeRequest, RemoteActionTarget};
use crate::InvocationId;
use crate::api::ActionAddress;

/// The application whose grants decide a call on the folder.
pub use tilo_app_id::apps::FILE_EXPLORER as APP;

/// The provider the device answers calls on its folder under.
pub const PROVIDER: &str = "tilo.device-files";

/// The resource the actions are on, and its kind: the workspace's own folder.
pub const FOLDER: &str = "files.folder";

/// Put the file, or the files, `arguments` name into the workspace's own folder.
pub const OPEN: &str = "open";

/// The argument of [`OPEN`] naming one file's content.
pub const CONTENT: &str = "content";

/// The argument of [`OPEN`] naming many files at once.
pub const FILES: &str = "files";

/// The argument of [`OPEN`] naming the file it is.
pub const NAME: &str = "name";

/// The argument of [`OPEN`] naming the folder of the workspace's own the files go under.
pub const DIR: &str = "dir";

/// The target of `action` on the device's folder.
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(FOLDER, action),
    }
}

/// The call of `action` on the folder with `arguments`, continuing the invocation
/// `on_behalf_of` where the caller makes it as one's work.
#[must_use]
pub fn request(
    action: &str,
    arguments: Map<String, Value>,
    on_behalf_of: Option<InvocationId>,
) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: target(action),
        argument: if arguments.is_empty() {
            InvokeArgument::None
        } else {
            InvokeArgument::Json(arguments)
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}
