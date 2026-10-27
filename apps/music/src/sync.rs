//! Test-only collaboration scenarios for the library document.
//!
//! Music's collaborative library state is owned by the platform's canonical
//! app document and `ActionHost`. The platform supplies persistence, peer
//! replication, history, and collaboration; this application must not offer a
//! second pairing or relay protocol. These scenarios drive the platform's
//! fake net in unit tests only.

#[cfg(test)]
use automerge::AutoCommit;

/// Bring two replicas of the library document into agreement.
///
/// The platform's fake net is the transport: one device offers its replica,
/// the net delivers, and the two are checked change for change. Music supplies
/// no transport of its own, so a test that wants two devices says so with two
/// names and one `FakeDocumentNet`, exactly as a production peer would.
#[cfg(test)]
pub(crate) fn converge(a: &mut AutoCommit, b: &mut AutoCommit) {
    use tilo_test_env::FakeDocumentNet;

    let mut net = FakeDocumentNet::new();
    let first = net.peer("a");
    let second = net.peer("b");
    net.publish(&first, a);
    net.publish(&second, b);
    net.deliver_into(&mut [(&first, a), (&second, b)]);
    net.converged(&first, a, &second, b, &[]).assert_holds();
}

#[cfg(test)]
mod tests {
    use automerge::AutoCommit;
    use tilo_test_env::FakeDocumentNet;
    use ui::tilo_resource::{Invoke, Replica};

    use crate::model::Library;
    use crate::resource::library::CreatePlaylist;

    fn seeded() -> Replica<Library> {
        let mut library = Replica::<Library>::new();
        for action in crate::seed::sample_actions() {
            library.apply(action).unwrap();
        }
        library
    }

    #[test]
    fn a_library_edit_on_one_device_reaches_the_other() {
        let mut net = FakeDocumentNet::new();
        let laptop = net.peer("laptop");
        let phone = net.peer("phone");
        let mut on_laptop = seeded();
        let mut on_phone = Replica::<Library>::from_doc(AutoCommit::new());
        net.publish(&laptop, on_laptop.doc_mut());
        net.deliver_into(&mut [(&laptop, on_laptop.doc_mut()), (&phone, on_phone.doc_mut())]);

        on_laptop
            .apply(Invoke::edit(&Library::like("tk_v2", true)).unwrap())
            .unwrap();
        on_phone
            .apply(Library::CREATE_PLAYLIST.with(CreatePlaylist {
                name: "Phone picks".into(),
                track: None,
            }))
            .unwrap();
        net.publish(&laptop, on_laptop.doc_mut());
        net.publish(&phone, on_phone.doc_mut());
        net.deliver_into(&mut [(&laptop, on_laptop.doc_mut()), (&phone, on_phone.doc_mut())]);
        net.converged(
            &laptop,
            on_laptop.doc_mut(),
            &phone,
            on_phone.doc_mut(),
            &[],
        )
        .assert_holds();

        for library in [on_laptop.state().unwrap(), on_phone.state().unwrap()] {
            assert!(library.is_liked("tk_v2"));
            assert!(
                library
                    .playlists()
                    .any(|playlist| playlist.name == "Phone picks")
            );
        }
    }

    #[test]
    fn a_playlist_made_off_the_net_lands_when_it_reconnects() {
        let mut net = FakeDocumentNet::new();
        let laptop = net.peer("laptop");
        let phone = net.peer("phone");
        let mut on_laptop = seeded();
        let mut on_phone = Replica::<Library>::from_doc(AutoCommit::new());
        net.publish(&laptop, on_laptop.doc_mut());
        net.deliver_into(&mut [(&laptop, on_laptop.doc_mut()), (&phone, on_phone.doc_mut())]);

        net.net().partition(&[phone.name()]);
        on_phone
            .apply(Library::CREATE_PLAYLIST.with(CreatePlaylist {
                name: "Written on the train".into(),
                track: None,
            }))
            .unwrap();
        net.publish(&phone, on_phone.doc_mut());
        net.deliver_into(&mut [(&laptop, on_laptop.doc_mut()), (&phone, on_phone.doc_mut())]);
        assert!(
            !on_laptop
                .state()
                .unwrap()
                .playlists()
                .any(|playlist| playlist.name == "Written on the train"),
            "nothing reaches the laptop while the phone is off the net",
        );

        net.net().heal(&[phone.name()]);
        net.publish(&phone, on_phone.doc_mut());
        net.deliver_into(&mut [(&laptop, on_laptop.doc_mut()), (&phone, on_phone.doc_mut())]);
        net.converged(
            &laptop,
            on_laptop.doc_mut(),
            &phone,
            on_phone.doc_mut(),
            &[],
        )
        .assert_holds();
        assert!(
            on_laptop
                .state()
                .unwrap()
                .playlists()
                .any(|playlist| playlist.name == "Written on the train"),
            "the playlist the phone made off the net reaches the laptop",
        );
    }
}
