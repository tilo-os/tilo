app tilo.workspace "Workspace"

/// A claim a device holds on something every device may take on, such as an ask an agent
/// answers: the earliest claim wins, and one that writes nothing for a minute may be taken
/// over, so the device that holds it can change at any moment.
type Lease {
    /// The device whose claim stands now, by its public key in lowercase hex: what
    /// `invoker.device()` names the device that signed an invocation by.
    device: Text
}

/// The workspace's own state, which code reads as `workspace.<field>` without a `use`. Nothing
/// writes it: the host answers each read from the workspace document as it stands then. It is
/// the root the host publishes rather than a `root` here, since nothing opens it in a window.
type Workspace {
    /// The leases whose claim stands now, keyed by the record they are about (`ask/<id>`).
    leases: Map<Lease>
}
