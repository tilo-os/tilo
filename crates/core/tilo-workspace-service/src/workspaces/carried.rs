/// How a member's write reached the host, which decides whether the
/// service still has to keep what it carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Carried {
    /// Over the workspace's link, or replayed from the device store: kept
    /// there, and carried to the linked devices already.
    Kept,
    /// Some way the store keeps nothing of, a device's entry: the service
    /// keeps it and carries it on the link.
    Unkept,
}
