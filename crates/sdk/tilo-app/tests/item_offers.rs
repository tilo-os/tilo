//! An item resource offers the views its record declares for it, and a
//! labelled item offers its renaming: the field edit of its label, asking
//! for the new name.

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Channel {
    #[model(label)]
    pub name: String,
    pub topic: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Channels {
    pub channels: Collection<Channel>,
}

fn channel_views(field: &str, _: &Id) -> Vec<tilo_actions::ResourceViewSpec> {
    match field {
        "channels" => vec![tilo_actions::ResourceViewSpec::new(
            "channel", "Channel", "open",
        )],
        _ => Vec::new(),
    }
}

#[resource(kind = "test.channels", label = "Channels", item_views = channel_views)]
impl Channels {
    /// Add a channel.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, name: String) {
        self.channels.push(
            cx.id(),
            Channel {
                name,
                topic: String::new(),
            },
        );
    }
}

struct ChannelsApp;

impl ResourceApp for ChannelsApp {
    type Resource = Channels;
    const ID: &'static str = "so.tilo.test-channels";
    const TITLE: &'static str = "Channels";

    fn view(cx: &mut ViewCx<Channels>) -> Element {
        cx.on_open_item(|_| Ok(()));
        Box::new(Column::new())
    }
}

fn find<'a>(
    resources: &'a [tilo_actions::Resource],
    id: &str,
) -> Option<&'a tilo_actions::Resource> {
    resources.iter().find_map(|resource| {
        (resource.id.as_str() == id)
            .then_some(resource)
            .or_else(|| find(&resource.children, id))
    })
}

#[test]
fn an_item_offers_its_views_and_its_renaming() {
    let mut app = TestApp::<ChannelsApp>::new().unwrap();
    app.invoke(Channels::ADD.with("random")).unwrap();
    let id = app.state().channels.ids().next().cloned().unwrap();
    let item = tilo_resource::items::item_resource_id::<Channels>("channels", &id);

    let resources = app.harness().resources();
    let published = find(&resources, &item).expect("the channel is published");
    let views: Vec<&str> = published
        .views
        .iter()
        .map(|view| view.id.as_str())
        .collect();
    assert_eq!(views, ["channel"]);
    let rename = published
        .actions
        .iter()
        .find(|action| action.id == "rename")
        .expect("a labelled item can be renamed");
    assert_eq!(
        rename
            .params()
            .iter()
            .map(|param| param.name.to_string())
            .collect::<Vec<_>>(),
        ["name"],
        "only the new name is asked"
    );

    app.call_on(&item, "rename", serde_json::json!({ "name": "general" }))
        .unwrap();
    let state = app.state();
    let channel = state.channels.get(&id).unwrap();
    assert_eq!(channel.name, "general");
    assert_eq!(channel.topic, "", "only the label changed");
}
