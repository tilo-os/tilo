//! What a read lists of a collection: its rows as the children they name.
use super::*;

#[test]
fn a_collections_rows_are_read_as_children_by_the_item_their_collection_publishes() {
    // A chain's read answers its collection's JSON form, `{id, value}`: each
    // row is a child, named by its value's words and read by the resource id
    // its collection publishes it under.
    let page = json!({
        "kind": "chat.messages",
        "messages": [
            {"id": "919bc8e7", "value": {"text": "message 44 of the long conversation\nmore"}},
            {"id": "b76f4317", "value": {"text": "message 43 of the long conversation"}},
        ],
        "nextCursor": "2",
    });
    let read = ResourceRead::of(&page).at("/chat/Messages", None);
    assert_eq!(read.cursor.as_deref(), Some("2"), "{read:?}");
    assert!(
        read.fields.get("messages").is_none(),
        "the rows are children, not fields: {:?}",
        read.fields
    );
    let named: Vec<(&str, &str, &str)> = read
        .children
        .iter()
        .map(|child| {
            (
                child.list.as_str(),
                child.name.as_str(),
                child.item.as_str(),
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            (
                "messages",
                "message 44 of the long conversation",
                "chat.messages/messages/919bc8e7"
            ),
            (
                "messages",
                "message 43 of the long conversation",
                "chat.messages/messages/b76f4317"
            ),
        ],
        "{read:?}"
    );
    assert_eq!(
        read.children[0].path,
        "/chat/Messages/chat.messages\\/messages\\/919bc8e7"
    );
}
