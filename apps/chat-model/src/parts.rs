//! Where in the chain of the chat's message parts a person read to: the mark
//! the chat resource keeps per person and channel, and what a part carries of the
//! parts before it.

use tilo_resource::Model;

/// How far a person read a channel: every top-level message of the channel
/// in the parts before `part`, and the first `seen` of them in `part`.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReadMark {
    /// The part the person read into.
    pub part: i64,
    /// Top-level messages of the channel they saw in that part.
    pub seen: i64,
}
