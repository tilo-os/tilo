//! Small offline workflows, embedded from their canonical repository pages.

use clap::ValueEnum;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Topic {
    Start,
    Actions,
    Verify,
    Publish,
}

impl Topic {
    const ALL: [Self; 4] = [Self::Start, Self::Actions, Self::Verify, Self::Publish];

    fn id(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Actions => "actions",
            Self::Verify => "verify",
            Self::Publish => "publish",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Start => "Create an app and reload it",
            Self::Actions => "Resources, controls and agent actions",
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
                println!("  {:8} {}", topic.id(), topic.title());
            }
        }
        json!({"topics": Topic::ALL.map(|topic| json!({
            "id": topic.id(), "title": topic.title(), "source": topic.source(),
        }))})
    }
}
