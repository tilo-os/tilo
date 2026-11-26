//! The shell lane an app's own connection carries: a shell the device's shell runtime runs, with
//! its terminal kept by the device and its bytes carried to the app.
//!
//! An app's process is sandboxed: it holds no socket of the shell runtime's and starts no program
//! of the device's. An app whose signed manifest declares it may open shells (`shells`) asks the
//! device for one on its connection ([`ShellOpen`]). The device opens it on its one connection to
//! the runtime, in the workspace the app was started in and never another, keeps the terminal
//! the runtime hands it, and from then on carries the session on this connection: what the app
//! types ([`ShellBytes`] as `ShellInput`), the window's size, the shell's output back
//! (`ShellOutput`) and, last, how the session ended (`ShellEnded`). Closing the shell
//! (`ShellClose`) or the connection lets the session go.

use serde::{Deserialize, Serialize};

/// The most bytes one frame of a shell's input or output carries.
pub const MAX_SHELL_BYTES: usize = 16 * 1024;

/// A shell to open: where it runs and how it starts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellOpen {
    /// Where it runs, as the shell runtime names a session's target (`{"kind": "environment",
    /// "record": "work"}`, `{"kind": "host"}`).
    pub target: serde_json::Value,
    /// The window's size, in cells.
    pub cols: u16,
    pub rows: u16,
    /// An agent asks for it, rather than the person.
    #[serde(default)]
    pub agent: bool,
    /// The program to run instead of the shell, when one is named.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// The hosted session to come back to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_session: Option<String>,
    /// The Terminal session record the shell belongs to, in a workspace a server hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// The key the shell marks each command line it runs with, so the app sees where each
    /// starts and ends in its output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<String>,
}

/// An opened shell: what the app names it by on the connection, and what the runtime said of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellOpened {
    /// The shell's id on this connection.
    pub shell: u64,
    /// The program at its prompt (`ush`, `host`).
    pub name: String,
    /// The hosted session it is, to come back to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_session: Option<String>,
    /// Whether the shell marks its command lines with the key the open named.
    #[serde(default)]
    pub commands: bool,
}

/// Bytes of one shell's input or output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellBytes {
    pub shell: u64,
    /// The bytes, as hex digits.
    #[serde(with = "hex_bytes")]
    pub bytes: Vec<u8>,
}

/// How a shell's session ended: the runtime's own report (`{"ended": "lost", "reason": ...}`),
/// empty when the shell exited and nothing more was said.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellEnded {
    pub shell: u64,
    #[serde(default)]
    pub report: String,
}

/// The report of a session whose connection to the device was lost: the device's word for
/// `ShellEnded` when it can no longer say anything itself.
#[must_use]
pub fn lost_report(reason: &str) -> String {
    serde_json::json!({ "ended": "lost", "reason": reason }).to_string()
}

mod hex_bytes {
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let digits = String::deserialize(deserializer)?;
        if digits.len() > super::MAX_SHELL_BYTES * 2 {
            return Err(serde::de::Error::custom("a shell frame over its bound"));
        }
        hex::decode(digits).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shells_bytes_travel_as_hex_and_an_oversized_frame_is_refused() {
        let frame = ShellBytes {
            shell: 3,
            bytes: b"echo hi\r".to_vec(),
        };
        let text = serde_json::to_string(&frame).unwrap();
        assert_eq!(text, r#"{"shell":3,"bytes":"6563686f2068690d"}"#);
        assert_eq!(serde_json::from_str::<ShellBytes>(&text).unwrap(), frame);
        let over = format!(
            r#"{{"shell":3,"bytes":"{}"}}"#,
            "00".repeat(MAX_SHELL_BYTES + 1)
        );
        assert!(serde_json::from_str::<ShellBytes>(&over).is_err());
    }
}
