/// How a member's write reached the governor, which decides whether the
/// service still has to keep what it carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Carried {
    /// Over the member's server session: the server kept it before the
    /// governor was told, so it is replayed to whoever catches up already.
    Kept,
    /// Some way the server keeps nothing of, the workspace's direct link or
    /// a device's entry: the service keeps it for the members on server
    /// sessions and whoever catches up later.
    Unkept,
}
