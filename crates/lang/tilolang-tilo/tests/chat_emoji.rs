//! Chat with the workspace's own emoji: Ada adds a picture under `:party_parrot:` through the
//! workspace's emoji record, reacts to a message with it from the message's picker, and the
//! reaction strip draws the picture where the name was. The shared picker offers it beside the
//! standard set, with the emoji the person uses most in a row of their own. Someone who did not
//! add it, and does not moderate, cannot take it away.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use tilo_resource::AppNamed;
use tilo_resource::{ReactionGroup, Reactor, Replica};
use tilo_ui_base::{
    workspace_emoji::CustomEmoji, workspace_emoji::EmojiSource, workspace_emoji::Small,
    workspace_emoji::WorkspaceEmoji, workspace_emoji::make_small,
};
use tilo_workspace_emoji::{EmojiImage, WorkspaceEmoji as Record, workspace_emoji};
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value, ViewInstance};
use ui::kurbo::{Point, Rect};

use crate::fixtures::{Replica as Device, Workspace, reactions, target};

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const APP: &str = "so.tilo.chat";

fn call(r: &Device, action: &str, args: Vec<Value>) -> Value {
    let meta = r.host.package(APP).unwrap().meta;
    let id = meta
        .actions
        .iter()
        .find(|a| a.name == action)
        .unwrap_or_else(|| panic!("no action {action}"))
        .id
        .clone();
    match r
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(&r.name)),
            (id.as_str(), args),
        )
        .unwrap()
    {
        InvokeOutcome::Done(v) => v,
        other => panic!("{action}: {other:?}"),
    }
}

/// The transcript's rows, each its node.
fn rows(view: &mut ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let list = view
        .tree()
        .first_of_kind("ui.VirtualList")
        .expect("a transcript")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| {
            let key = view.row_key(&list, i);
            view.menu_row(&list, key.as_deref());
            view.poll_patches().unwrap();
            view.row(&list, i).unwrap().cloned()
        })
        .collect()
}

/// The workspace's emoji record as a window holds it for its views: what the picker and the
/// reaction strip draw from, and where the picker's "Add emoji" adds through the record's
/// typed action, as the person.
struct Held {
    record: RefCell<Replica<Record, AppNamed>>,
    revision: Cell<u64>,
}

impl EmojiSource for Held {
    fn emoji(&self) -> Rc<WorkspaceEmoji> {
        let state = self.record.borrow().state().expect("the record reads");
        Rc::new(WorkspaceEmoji::new(
            state
                .emoji
                .iter()
                .map(|(name, emoji)| CustomEmoji {
                    name: name.clone(),
                    image: Arc::from(emoji.image.bytes()),
                })
                .collect(),
        ))
    }

    fn revision(&self) -> u64 {
        self.revision.get()
    }

    fn can_manage(&self) -> bool {
        true
    }

    fn add(&self, name: &str, image: &[u8]) -> Result<(), String> {
        let Small::Ready(png) = make_small(image) else {
            return Err("not a picture".into());
        };
        self.record
            .borrow_mut()
            .apply(Record::ADD_EMOJI.with(workspace_emoji::AddEmoji {
                name: name.into(),
                image: EmojiImage(png).to_base64(),
            }))
            .map_err(|error| error.to_string())?;
        self.revision.set(self.revision.get().wrapping_add(1));
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        self.record
            .borrow_mut()
            .apply(Record::REMOVE_EMOJI.with(workspace_emoji::RemoveEmoji { name: name.into() }))
            .map_err(|error| error.to_string())?;
        self.revision.set(self.revision.get().wrapping_add(1));
        Ok(())
    }

    fn choose_and_add(&self) {}
}

/// The test picture every capture uses, as its bytes.
fn picture() -> Vec<u8> {
    let encoded = ui::headless::FIXTURE_PICTURE
        .strip_prefix("data:image/png;base64,")
        .expect("a PNG data address");
    EmojiImage::from_base64(encoded)
        .expect("base64")
        .bytes()
        .to_vec()
}

/// The width of each pill a strip of `emoji` lays out, one reaction each.
fn pill_widths(emoji: &[&str]) -> Vec<f64> {
    let groups: Vec<ReactionGroup> = emoji
        .iter()
        .map(|emoji| ReactionGroup {
            emoji: (*emoji).to_owned(),
            reactors: vec![Reactor {
                by: tilo_resource::MemberId::person("ada"),
                persona: None,
                name: None,
            }],
            first_at: 0,
        })
        .collect();
    let theme = tilo_ui_base::Theme::dark();
    ui::widget::reaction_strip::layout(
        &groups,
        &[],
        None,
        true,
        (Point::ZERO, 800.0),
        &mut tilo_ui_base::text_render::TextRenderer::new(),
        &ui::widget::reaction_strip::StripMetrics::of(&theme),
    )
    .pills()
    .iter()
    .map(|pill| pill.rect.width())
    .collect()
}

/// A picker leads its frequent row with the emoji used most, after the workspace's own and the
/// add tile; the standard set follows, and a search finds one first.
fn frequent_row_leads_with_the_most_used() {
    let mut strip = ui::widget::ReactionStripWidget::new(Vec::new(), None);
    for emoji in ["🎉", ":party_parrot:", ":party_parrot:"] {
        strip.toggle(emoji);
    }
    let mut picker = ui::widget::EmojiPicker::framed();
    picker.open_at(
        Some(Rect::new(100.0, 40.0, 130.0, 70.0)),
        Rect::new(0.0, 0.0, 1280.0, 800.0),
        &tilo_ui_base::Theme::dark(),
    );
    let items = picker.items();
    assert_eq!(
        items[..4],
        [
            Some(":party_parrot:"),
            None,
            Some(":party_parrot:"),
            Some("🎉"),
        ],
        "the workspace's emoji with the add tile, then the most used: {items:?}"
    );
    assert!(items.len() > 4, "the standard set follows");
    picker.set_query("parrot");
    assert_eq!(
        picker.items().first().copied().flatten(),
        Some(":party_parrot:"),
        "searching finds it first"
    );
}

#[test]
fn a_workspace_emoji_is_added_reacted_with_drawn_offered_and_kept_from_others() {
    // Ada adds the picture as `:party_parrot:` through the record's action.
    let held = Rc::new(Held {
        record: RefCell::new(
            Replica::<Record>::new()
                .in_app("so.tilo.model", "Model")
                .as_author("ada")
                .in_app(tilo_workspace_emoji::OWNER, "Chat"),
        ),
        revision: Cell::new(0),
    });
    held.add("party_parrot", &picture()).expect("Ada adds it");
    let state = held.record.borrow().state().unwrap();
    assert_eq!(state.names(), ["party_parrot"]);
    assert_eq!(
        state.get("party_parrot").unwrap().added_by,
        tilo_resource::MemberId::person("ada")
    );

    // Ben, who neither added it nor moderates, cannot remove it as his own.
    let mut ben = held.record.borrow_mut().fork().as_author("ben");
    assert!(
        ben.apply(Record::REMOVE_EMOJI.with(workspace_emoji::RemoveEmoji {
            name: "party_parrot".into()
        }))
        .is_err(),
        "only who added it removes it as their own"
    );
    assert_eq!(ben.state().unwrap().names(), ["party_parrot"]);

    // Ada's window draws the workspace's emoji from the record.
    let scope = ui::WakeScope::new();
    let _in = scope.enter();
    tilo_ui_base::workspace_emoji::register(
        &scope.handle(),
        Rc::clone(&held) as Rc<dyn EmojiSource>,
    );

    // She reacts to a message with it from the message's picker.
    let (ada, _) = Device::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let root = Value::resource(&ada.host.root_of(APP).unwrap());
    ada.commit();
    let channel = call(&ada, "new_channel", vec![root, Value::text("design")]);
    ada.commit();
    call(
        &ada,
        "send",
        vec![
            channel.clone(),
            Value::Rich(RichValue::from_text("The new logo is in")),
            Value::none(),
        ],
    );
    ada.commit();
    let mut view = ada.host.open_resource(&target(&channel), None).unwrap();
    let row = rows(&mut view).remove(0);
    let picker = row
        .first_of_kind("ui.EmojiPicker")
        .expect("a reaction picker on a message")
        .key
        .clone();
    assert!(matches!(
        view.fire_with(&picker, "pick", Some(&Value::text(":party_parrot:")))
            .unwrap(),
        InvokeOutcome::Done(_)
    ));
    ada.commit();
    let row = rows(&mut view).remove(0);
    let shown = reactions(&row);
    assert_eq!(
        shown,
        [(
            ":party_parrot:".to_owned(),
            vec![Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
                "ada"
            ))]
        )],
        "the message holds the reaction by its name"
    );

    // The strip draws the picture in an emoji's square, not the name's words; one the
    // workspace does not have stays its words.
    let [parrot, party, unknown] = pill_widths(&[":party_parrot:", "🎉", ":nobody_has_this:"])[..]
    else {
        panic!("three pills");
    };
    assert!(
        (parrot - party).abs() < 0.5,
        "the picture is drawn as wide as an emoji: {parrot} vs {party}"
    );
    assert!(
        unknown > party + 20.0,
        "a name without a picture is its words: {unknown}"
    );

    // Reacting is using: the emoji used most leads the picker's frequent row, after the
    // workspace's own and before the standard set.
    frequent_row_leads_with_the_most_used();
}
