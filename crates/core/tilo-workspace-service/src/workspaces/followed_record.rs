//! One record of a workspace this device holds, followed as it changes: how a part of the daemon
//! reads a record without waiting.
//!
//! Records are sealed, so reading one ([`Workspaces::resource_document`]) waits for its keys. A
//! decision that cannot wait (whom the live relay admits, which device a file can go to) reads a
//! [`FollowedRecord`]: the record's bytes as last saved here, read again on each write to it, when
//! the follower missed writes, and when the workspace's governance or what is held here moves.

use std::sync::Arc;

use tilo_resource::{Replica, Resource};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;

use super::{Workspaces, person_workspace};

/// A record as this device last saved it: `None` while it holds none of it.
pub type Saved = Option<Arc<[u8]>>;

/// One record of a workspace, kept current here as the workspace takes in writes to it.
#[derive(Clone)]
pub struct FollowedRecord {
    workspace: Arc<str>,
    saved: watch::Receiver<Saved>,
}

impl FollowedRecord {
    /// The record's bytes, as this device last saved them.
    #[must_use]
    pub fn get(&self) -> Saved {
        self.saved.borrow().clone()
    }

    /// The record's state as a resource `R`: its genesis state while this device holds none of it.
    ///
    /// # Errors
    /// What this device holds is not a resource `R`.
    pub fn state<R: Resource>(&self) -> Result<R::State, String> {
        let replica = match self.get() {
            Some(bytes) => Replica::<R>::load(&bytes).map_err(|error| error.to_string())?,
            None => Replica::<R>::from_doc(person_workspace::genesis::<R>(&self.workspace)?),
        };
        replica.state().map_err(|error| error.to_string())
    }

    /// The record's bytes, moving each time they change.
    #[must_use]
    pub fn changes(&self) -> watch::Receiver<Saved> {
        self.saved.clone()
    }
}

impl Workspaces {
    /// Follow the record `record` of `app` in `workspace`, on the current Tokio runtime, for as
    /// long as anyone holds what this answers and these workspaces run.
    ///
    /// # Panics
    /// When called outside a Tokio runtime.
    #[must_use]
    pub fn follow_record(&self, workspace: &str, (app, record): (&str, &str)) -> FollowedRecord {
        let (tell, saved) = watch::channel(None);
        // Subscribed before the first read, so a write between the two is read again.
        let (mut writes, mut governance) = (self.document_writes(), self.governance_changes());
        let mut holds = self.holds_changed();
        let mut stops = self.stops_changed();
        let workspaces = self.downgrade();
        let (id, app, record) = (workspace.to_owned(), app.to_owned(), record.to_owned());
        tokio::spawn(async move {
            loop {
                let Some(held) = workspaces.upgrade() else {
                    return;
                };
                // The part this copy was made by has ended: what it read is not kept current
                // by it any more, and the part that starts next reads its own.
                if held.stopped() || *stops.borrow() {
                    return;
                }
                // A record not held yet, or whose keys are on their way, stays as last read.
                if let Ok(read) = held.resource_document(&id, &app, &record).await {
                    let read: Saved = read.map(Arc::from);
                    tell.send_if_modified(|saved| {
                        let moved = *saved != read;
                        *saved = read;
                        moved
                    });
                }
                drop(held);
                let again = loop {
                    tokio::select! {
                        write = writes.recv() => match write {
                            Ok(write) if write.workspace == id && write.app == app && write.record == record => break true,
                            Ok(_) => {}
                            Err(RecvError::Lagged(_)) => break true,
                            Err(RecvError::Closed) => break false,
                        },
                        changed = governance.recv() => match changed {
                            Ok(changed) if changed == id => break true,
                            Ok(_) => {}
                            Err(RecvError::Lagged(_)) => break true,
                            Err(RecvError::Closed) => break false,
                        },
                        held = holds.changed() => break held.is_ok(),
                        changed = stops.changed() => break changed.is_err() || *stops.borrow(),
                        () = tell.closed() => break false,
                    }
                };
                if !again {
                    return;
                }
            }
        });
        FollowedRecord {
            workspace: Arc::from(workspace),
            saved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{hosting_device, test_host};
    use super::super::person_workspace::devices::{DEVICES, Devices};
    use super::person_workspace;
    use tilo_resource::Invoke;

    /// The person's device writes into their own workspace before this device holds it: the
    /// write waits, lands once the workspace is held, and a follower of the record sees it. The
    /// person's own action asked of the record then is answered at once, as taken up and not
    /// finished, since the device may never be taken in, and following it hears it made once it
    /// is; one the record refuses once it is held is heard refused, with why.
    #[allow(
        clippy::too_many_lines,
        reason = "one flow, in order: what is taken up, the workspace held, and how following each ending reads"
    )]
    #[tokio::test]
    async fn a_write_before_the_workspace_is_held_lands_and_its_follower_sees_it() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let host = test_host(dir.path(), &key, &cert).await;
        let workspaces = host.workspaces().clone();
        let own = person_workspace::workspace_id(&workspaces);
        let followed = workspaces.follow_record(&own, DEVICES);
        let waiting = tokio::spawn({
            let (workspaces, keep) = (workspaces.clone(), Devices::keeping(&cert).unwrap());
            async move { person_workspace::apply_when_held(&workspaces, DEVICES, keep).await }
        });
        let person = tilo_actions::remote::Caller::authenticated(
            cert.user_public_key,
            cert.device_public_key,
        )
        .in_workspace(own.clone());
        let invocation = tilo_actions::InvocationId::new();
        let records = workspaces
            .providers()
            .get(tilo_actions::remote::RESOURCE_PROVIDER)
            .expect("the device applies its person's record actions")
            .answers;
        let ask = |invocation, keep: Invoke<Devices>| {
            let request = tilo_actions::remote::InvokeRequest {
                invocation,
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: format!(
                        "{}:{}:{}",
                        tilo_actions::remote::RESOURCE_PROVIDER,
                        DEVICES.0,
                        DEVICES.1
                    ),
                    address: keep.address(),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(keep.arguments),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            };
            records.invoke(&person, request)
        };
        let asked = ask(invocation, Devices::keeping(&cert).unwrap());
        let receipt = tilo_test_env::within("the person's action is answered at once", asked)
            .await
            .expect("the person's action is answered");
        assert_eq!(
            receipt.expect("the action is taken up").status,
            tilo_actions::remote::ReceiptStatus::Accepted,
            "taken up, not finished, while the workspace is not held"
        );
        // A write the record refuses once it is held is taken up as any other, so nothing is
        // known of the refusal when it is answered: following it hears why it could not be made.
        let refused = tilo_actions::InvocationId::new();
        let asked = ask(
            refused,
            Devices::KEEP_FACT.with((
                hex::encode(cert.device_public_key),
                "not a certificate's hex".to_owned(),
                "about".to_owned(),
                String::new(),
            )),
        );
        let receipt = tilo_test_env::within("the refused action is answered as taken up", asked)
            .await
            .expect("the refused action is answered");
        assert_eq!(
            receipt.expect("the refused action is taken up").status,
            tilo_actions::remote::ReceiptStatus::Accepted,
            "taken up, and refused once the workspace is held"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(!waiting.is_finished(), "the write waits for the workspace");

        person_workspace::found(&workspaces).await.unwrap();
        let mut changes = followed.changes();
        tilo_test_env::within("the write lands and the follower sees it", async {
            waiting
                .await
                .unwrap()
                .expect("written once the workspace is held");
            let key = tilo_resource::Id::from_raw(hex::encode(cert.device_public_key));
            while !followed
                .state::<Devices>()
                .is_ok_and(|devices| devices.devices.get(&key).is_some())
            {
                changes.changed().await.expect("the record is followed");
            }
        })
        .await;
        tilo_test_env::within("following the action taken up hears it was made", async {
            loop {
                match records
                    .observe(&person, &invocation)
                    .map(|state| state.update)
                {
                    Some(tilo_actions::InvocationUpdate::Succeeded { .. }) => break,
                    Some(tilo_actions::InvocationUpdate::Accepted { .. }) => {}
                    other => panic!("the action taken up was not made: {other:?}"),
                }
                match records.next_change(&person, &invocation) {
                    Some(moved) => {
                        moved.await;
                    }
                    None => panic!("the action taken up says when it moves"),
                }
            }
        })
        .await;
        tilo_test_env::within("following the refused action taken up hears why", async {
            loop {
                match records.observe(&person, &refused).map(|state| state.update) {
                    Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                        assert!(
                            error.message.contains("hex"),
                            "the refusal says what the record refused: {error:?}"
                        );
                        break;
                    }
                    Some(tilo_actions::InvocationUpdate::Accepted { .. }) => {}
                    other => panic!("the action taken up was refused: {other:?}"),
                }
                match records.next_change(&person, &refused) {
                    Some(moved) => {
                        moved.await;
                    }
                    None => panic!("the refused action says when it moves"),
                }
            }
        })
        .await;
    }
}
