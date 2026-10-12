//! Small offline workflows, embedded from their canonical repository pages.

use clap::ValueEnum;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Topic {
    Start,
    Actions,
    #[value(alias = "undo", alias = "history", alias = "sync")]
    Collaborate,
    #[value(alias = "roles", alias = "access")]
    Permissions,
    #[value(alias = "http", alias = "fetch", alias = "metered", alias = "offline")]
    Network,
    Verify,
    Publish,
}

impl Topic {
    const ALL: [Self; 7] = [
        Self::Start,
        Self::Actions,
        Self::Collaborate,
        Self::Permissions,
        Self::Network,
        Self::Verify,
        Self::Publish,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Actions => "actions",
            Self::Collaborate => "collaborate",
            Self::Permissions => "permissions",
            Self::Network => "network",
            Self::Verify => "verify",
            Self::Publish => "publish",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Start => "Create an app and reload it",
            Self::Actions => "Resources, controls and agent actions",
            Self::Collaborate => "Collaboration, history, undo and device sync",
            Self::Permissions => "Live roles, permission-aware views and refused actions",
            Self::Network => "Portable HTTP, network hints and external effects",
            Self::Verify => "Diagnose, test and inspect the production view",
            Self::Publish => "Signing, registration, reusable CI and publication",
        }
    }

    fn source(self) -> String {
        format!("docs/developers/tasks/{}.md", self.id())
    }

    fn content(self) -> &'static str {
        match self {
            Self::Start => include_str!("../../../../docs/developers/tasks/start.md"),
            Self::Actions => include_str!("../../../../docs/developers/tasks/actions.md"),
            Self::Collaborate => include_str!("../../../../docs/developers/tasks/collaborate.md"),
            Self::Permissions => include_str!("../../../../docs/developers/tasks/permissions.md"),
            Self::Network => include_str!("../../../../docs/developers/tasks/network.md"),
            Self::Verify => include_str!("../../../../docs/developers/tasks/verify.md"),
            Self::Publish => include_str!("../../../../docs/developers/tasks/publish.md"),
        }
    }
}

pub fn run(topic: Option<Topic>, human: bool) -> Value {
    if let Some(topic) = topic {
        if human {
            print!("{}", topic.content());
        }
        json!({
            "topic": topic.id(), "title": topic.title(), "source": topic.source(),
            "format": "markdown", "content": topic.content(),
        })
    } else {
        if human {
            println!("Offline task guides: tilo-app guide <topic>\n");
            for topic in Topic::ALL {
                println!("  {:12} {}", topic.id(), topic.title());
            }
        }
        json!({"topics": Topic::ALL.map(|topic| json!({
            "id": topic.id(), "title": topic.title(), "source": topic.source(),
        }))})
    }
}
