//! Who may read content by its address.
//!
//! A record names its content by hash, and a hash is no capability: whoever may read the content
//! reads a resource of the workspace that names it, its readers include them, and their app's
//! rules let them read it. The store's index keeps, per content address, the resources whose
//! fields name it ([`crate::Store::naming_content`]). This is the one rule every reader of content
//! by its address follows — a host answering a member's ask, and an app reading what it is told
//! to — so a refusal is decided in one place.

/// Whether the content at `address` is one the asker reads.
///
/// `names` answers, for the address ([`tilo_actions::named_content`]), the resources of the
/// workspace that name it and whose readers include the asker, each by the app that declares its
/// kind, its kind and its id: the store's index, resolved to the apps the workspace declares.
/// `reads` is the policy's read decision for one of those resources. Content no resource the asker
/// reads names, and content nothing names, are refused by the same lookup, so a refusal says
/// nothing of whether the workspace keeps it.
pub fn may_read_content(
    address: &str,
    names: impl FnOnce(&str) -> Vec<(String, String, String)>,
    reads: impl Fn(&str, &str, &str) -> bool,
) -> bool {
    let Some(address) = tilo_actions::named_content(address) else {
        return false;
    };
    names(&address)
        .into_iter()
        .any(|(app, kind, id)| reads(&app, &kind, &id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hash as the index files the content a resource names.
    const HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    #[test]
    fn content_is_read_through_one_of_the_resources_that_names_it() {
        let address = format!("tilo.content:{HASH}");
        assert!(may_read_content(
            &address,
            |address| {
                assert_eq!(address, HASH, "the index is read by the content's hash");
                vec![
                    (
                        "test.app".to_owned(),
                        "test.app.A".to_owned(),
                        "a".to_owned(),
                    ),
                    (
                        "test.app".to_owned(),
                        "test.app.B".to_owned(),
                        "b".to_owned(),
                    ),
                ]
            },
            |_, kind, _| kind == "test.app.B",
        ));
        assert!(
            !may_read_content(
                &address,
                |_| vec![(
                    "test.app".to_owned(),
                    "test.app.A".to_owned(),
                    "a".to_owned()
                )],
                |_, _, _| false,
            ),
            "a resource the policy refuses names nothing the asker reads"
        );
        assert!(
            !may_read_content(&address, |_| Vec::new(), |_, _, _| true),
            "content nothing names is refused"
        );
        assert!(
            !may_read_content(
                &"not a content address".repeat(4),
                |_| { panic!("an address that names no content is not looked up") },
                |_, _, _| true
            ),
            "a value that names no content is refused"
        );
    }
}
