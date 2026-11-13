//! A channel's page as a view of a resource: who it is, and its videos.

use crate::i18n::{arg, t_args};
#[cfg(test)]
use tilo_view::Effect;
use tilo_view::ui::{self, Gap};
use tilo_view::{Node, ResourceRef, ViewCx, ViewResource};
use tilo_view_macros::view;

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Channel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub handle: String,
    #[serde(default)]
    pub subscribers: u64,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub seed: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub videos: Vec<ChannelVideo>,
    #[serde(default)]
    pub subscribed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ChannelVideo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub views: String,
    #[serde(default)]
    pub published: String,
}

impl Channel {
    #[must_use]
    pub fn resource(&self) -> ResourceRef {
        ResourceRef {
            id: format!("tube.channel:{}", self.id),
            ty: <Self as ViewResource>::KIND.to_owned(),
        }
    }

    #[must_use]
    pub fn from_model(model: &crate::model::TubeModel, id: &str) -> Option<Self> {
        let channel = model.channels.iter().find(|c| c.id == id)?;
        let videos = model
            .videos
            .iter()
            .filter(|v| v.channel_id == channel.id)
            .map(|v| ChannelVideo {
                id: v.id.clone(),
                title: v.title.clone(),
                views: views_label(v.views),
                published: v.published.clone(),
            })
            .collect();
        Some(Self {
            id: channel.id.clone(),
            name: channel.name.clone(),
            handle: channel.handle.clone(),
            subscribers: channel.subscribers,
            verified: channel.verified,
            description: channel.description.clone(),
            seed: channel.seed.clone(),
            avatar_url: channel.avatar_url.clone(),
            videos,
            subscribed: model.subscriptions.iter().any(|s| s == &channel.id),
        })
    }
}

impl ViewResource for Channel {
    const KIND: &'static str = "tube.channel";
    const APP: &'static str = "tube";
}

fn views_label(views: u64) -> String {
    if views >= 1_000_000 {
        format!("{:.1}M views", views as f64 / 1_000_000.0)
    } else if views >= 1_000 {
        format!("{:.0}K views", views as f64 / 1_000.0)
    } else {
        format!("{views} views")
    }
}

fn subscribers_label(subscribers: u64) -> String {
    if subscribers >= 1_000_000 {
        format!("{:.1}M subscribers", subscribers as f64 / 1_000_000.0)
    } else if subscribers >= 1_000 {
        format!("{:.0}K subscribers", subscribers as f64 / 1_000.0)
    } else {
        format!("{subscribers} subscribers")
    }
}

#[view(default)]
fn page(c: &Channel, cx: &ViewCx) -> Node {
    let mut children = vec![header(c, cx)];
    if !c.description.trim().is_empty() {
        children.push(
            ui::TextBuilder::new(&cx.key("about"), &c.description)
                .secondary(true)
                .into(),
        );
    }
    children.push(videos(c, cx));
    ui::stack(&cx.key("page"), Gap::Sibling, children)
}

fn header(c: &Channel, cx: &ViewCx) -> Node {
    let avatar = ui::AvatarBuilder::new(&cx.key("avatar"))
        .name(&c.name)
        .square(true)
        .into();

    let mut facts: Vec<Node> = vec![
        ui::TextBuilder::new(&cx.key("name"), &c.name)
            .role(ui::TextRole::LargeTitle)
            .into(),
    ];
    if c.verified {
        facts.push(ui::IconBuilder::new(&cx.key("verified"), "circle-check").into());
    }
    if !c.handle.is_empty() {
        facts.push(
            ui::TextBuilder::new(&cx.key("handle"), &c.handle)
                .role(ui::TextRole::Caption)
                .secondary(true)
                .into(),
        );
    }
    if c.subscribers > 0 {
        facts.push(
            ui::TextBuilder::new(&cx.key("subscribers"), subscribers_label(c.subscribers))
                .role(ui::TextRole::Caption)
                .secondary(true)
                .into(),
        );
    }
    let facts = ui::stack(&cx.key("facts"), Gap::Tight, facts);

    let subscribe = ui::ButtonBuilder::new(
        &cx.key("subscribe"),
        if c.subscribed {
            "Subscribed"
        } else {
            "Subscribe"
        },
    )
    .role(if c.subscribed {
        ui::ButtonRole::Secondary
    } else {
        ui::ButtonRole::Primary
    })
    .toggled(c.subscribed)
    .into();
    let press = cx.action(&subscribe, "press", "subscribe");
    let subscribe = tilo_view::node::on(subscribe, "press", press);

    let head = ui::row(&cx.key("head"), Gap::Related, vec![avatar, facts]);
    ui::stack(&cx.key("header"), Gap::Related, vec![head, subscribe])
}

fn videos(c: &Channel, cx: &ViewCx) -> Node {
    if c.videos.is_empty() {
        return ui::EmptyStateBuilder::new(&cx.key("empty"), "No videos yet").into();
    }
    let rows: Vec<Node> = c
        .videos
        .iter()
        .map(|video| {
            let key = cx.key(format!("video-{}", video.id));
            let reach = if video.published.trim().is_empty() {
                video.views.clone()
            } else {
                t_args(
                    "tube-views-and-age",
                    &[
                        arg("views", video.views.as_str()),
                        arg("age", video.published.trim()),
                    ],
                )
            };
            let row = ui::ListRowBuilder::new(&key)
                .title(&video.title)
                .secondary(reach)
                .item(ResourceRef {
                    id: format!("tube.video:{}", video.id),
                    ty: "tube.video".to_owned(),
                })
                .into();
            let target = ResourceRef {
                id: format!("tube.video:{}", video.id),
                ty: "tube.video".to_owned(),
            };
            let open = cx.open(&row, "activate", target, None);
            tilo_view::node::on(row, "activate", open)
        })
        .collect();
    ui::stack(&cx.key("videos"), Gap::Tight, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_lists_the_channels_videos_and_its_subscribe_action() {
        let channel = Channel {
            id: "ch_lowlevel".to_owned(),
            name: "Low Level Learning".to_owned(),
            handle: "@lowlevellearning".to_owned(),
            subscribers: 742_000,
            verified: true,
            description: "Systems programming.".to_owned(),
            seed: "ch_lowlevel".to_owned(),
            avatar_url: String::new(),
            videos: vec![ChannelVideo {
                id: "vid_rust_async".to_owned(),
                title: "Async Rust".to_owned(),
                views: "1.2M views".to_owned(),
                published: "3 days ago".to_owned(),
            }],
            subscribed: false,
        };
        let cx = ViewCx::new(
            channel.resource(),
            "page",
            tilo_view::Principal {
                id: "Pat".to_owned(),
                name: "Pat".to_owned(),
            },
            std::collections::BTreeMap::new(),
        );
        let tree = page(&channel, &cx);
        assert_tree(&tree, &cx);
    }

    fn assert_tree(tree: &Node, cx: &ViewCx) {
        let mut texts = Vec::new();
        collect(tree, &mut texts);
        for expected in ["Low Level Learning", "Async Rust", "Subscribe"] {
            assert!(
                texts.iter().any(|t| t == expected),
                "{expected} shows: {texts:?}"
            );
        }
        let effects = cx.recorded();
        assert!(
            effects.values().any(|effect| matches!(
                effect,
                Effect::Action { action, .. } if action == "subscribe"
            )),
            "the page offers the channel's subscribe action: {effects:?}"
        );
    }

    fn collect(node: &Node, out: &mut Vec<String>) {
        for value in node.props.values() {
            if let tilo_view::Value::Text(text) = value {
                out.push(text.clone());
            }
        }
        for child in &node.children {
            collect(child, out);
        }
    }
}
