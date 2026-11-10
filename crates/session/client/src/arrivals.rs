//! What reaches this device's workspaces from their other devices, as a read that waits on it
//! hears of it: no timer, and nothing to do while nothing arrives.

/// Ticks whenever one of this device's workspaces took in a write another device made.
#[derive(Debug, Default)]
pub struct Arrivals {
    landed: tokio::sync::watch::Sender<u64>,
}

impl Arrivals {
    /// A write another device made landed in one of this device's workspaces.
    pub fn landed(&self) {
        self.landed
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Wait for the next write another device makes to land, from a value read before the wait:
    /// a receiver answers at once when one landed in between, and sleeps until one does.
    #[must_use]
    pub fn watching(&self) -> tokio::sync::watch::Receiver<u64> {
        self.landed.subscribe()
    }
}
