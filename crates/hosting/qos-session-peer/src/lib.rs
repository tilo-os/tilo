//! The device host: what a user's own machine or home server runs to host its
//! workspaces, built on the shared host core.
pub mod recovery;

#[cfg(test)]
tilo_automerge_isolate::test_worker!();
