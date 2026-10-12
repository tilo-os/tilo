use super::*;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
struct Host {
    account: ServiceAccount,
    fault: u8,
    calls: AtomicUsize,
}
fn group() -> GroupSummary {
    GroupSummary {
        id: "123-456@g.us".into(),
        title: "Private fixture group".into(),
    }
}
impl GroupHost for Host {
    fn change(&self, request: GroupChangeRequest) -> GroupActionFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(GroupReceipt {
            account: self.account.clone(),
            group: request.group,
            operation: request.operation,
        })))
    }
    fn send(&self, request: GroupSendRequest) -> GroupActionFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(GroupReceipt {
            account: self.account.clone(),
            group: request.group,
            operation: request.operation,
        })))
    }

    fn list(&self) -> GroupsFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut groups = vec![group()];
        if self.fault == 1 {
            groups.push(group());
        }
        Box::pin(std::future::ready(Ok(GroupList {
            account: self.account.clone(),
            groups,
        })))
    }
    fn read(&self, _: String) -> MembershipFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut group = group();
        if self.fault == 2 {
            group.id = "789@g.us".into();
        }
        let mut members = vec![GroupMember {
            address: "123@lid".into(),
            role: GroupRole::Admin,
        }];
        if self.fault == 3 {
            members.push(members[0].clone());
        }
        if self.fault == 4 {
            members[0].address = "123@g.us".into();
        }
        Box::pin(std::future::ready(Ok(GroupMembership {
            account: self.account.clone(),
            group,
            members,
            announcement: false,
            self_member: if self.fault == 5 {
                "999@lid"
            } else {
                "123@lid"
            }
            .into(),
            review: "fixture-review".into(),
        })))
    }
}
fn complete<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("controlled group host must answer immediately"),
    }
}
fn client(fault: u8, host_account: &str) -> (GroupClient, Arc<Host>) {
    let host = Arc::new(Host {
        account: ServiceAccount::new("whatsapp", host_account),
        fault,
        calls: AtomicUsize::new(0),
    });
    (
        GroupClient::new(
            "principal".into(),
            ServiceAccount::new("whatsapp", "account-a"),
            host.clone(),
        ),
        host,
    )
}
#[test]
fn private_group_client_binds_account_group_and_exact_member_identities() {
    let (client, _) = client(0, "account-a");
    let Ok(list) = complete(client.list()) else {
        panic!("valid list refused");
    };
    assert_eq!(list.groups[0].id, "123-456@g.us");
    let Ok(membership) = complete(client.read("123-456@g.us".into())) else {
        panic!("valid membership refused");
    };
    assert_eq!(membership.members[0].address, "123@lid");
    for fault in 1..=5 {
        let (client, _) = self::client(fault, "account-a");
        if fault == 1 {
            assert!(matches!(
                complete(client.list()),
                Err(SendError::InvalidRequest)
            ));
        } else {
            assert!(complete(client.read("123-456@g.us".into())).is_err());
        }
    }
    let (client, _) = self::client(0, "account-b");
    assert!(matches!(complete(client.list()), Err(SendError::Revoked)));
    assert!(matches!(
        complete(client.read("123-456@g.us".into())),
        Err(SendError::Revoked)
    ));
}
#[test]
fn direct_person_addresses_cannot_trigger_group_service_io() {
    let (client, host) = client(0, "account-a");
    for id in [
        "123@lid",
        "123@s.whatsapp.net",
        "-456@g.us",
        "123--456@g.us",
        "123:4@g.us",
    ] {
        assert!(matches!(
            complete(client.read(id.into())),
            Err(SendError::InvalidRequest)
        ));
    }
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn group_write_receipts_and_targets_are_bound_to_the_original_sender() {
    let request = || GroupSendRequest {
        group: "123@g.us".into(),
        review: "review".into(),
        operation: "operation".into(),
        text: "private text".to_owned().into(),
    };
    let (client, host) = client(0, "account-b");
    assert!(matches!(
        complete(client.send(request())),
        Err(SendError::Uncertain)
    ));
    assert_eq!(host.calls.load(Ordering::SeqCst), 1);
    let (client, host) = self::client(0, "account-a");
    for member in ["123@g.us", "123:4@lid", "name@lid"] {
        assert!(matches!(
            complete(client.change(GroupChangeRequest {
                group: "123@g.us".into(),
                review: "review".into(),
                operation: "operation".into(),
                change: GroupChange::Remove(member.into())
            })),
            Err(SendError::InvalidRequest)
        ));
    }
    let mut empty = request();
    empty.text = String::new().into();
    assert!(complete(client.send(empty)).is_err());
    let mut direct = request();
    direct.group = "123@lid".into();
    assert!(complete(client.send(direct)).is_err());
    assert_eq!(
        host.calls.load(Ordering::SeqCst),
        0,
        "invalid writes must never reach the provider"
    );
}
