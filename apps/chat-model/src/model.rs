//! The kinds the chat's resources store.

use serde::{Deserialize, Serialize};

/// A text channel holds a conversation; a voice channel holds a call.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    tilo_resource::Model,
    tilo_resource::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    #[default]
    Text,
    Voice,
}

impl std::str::FromStr for ChannelKind {
    type Err = std::convert::Infallible;

    /// Never fails: an unrecognised key defaults to [`ChannelKind::Text`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "voice" => ChannelKind::Voice,
            _ => ChannelKind::Text,
        })
    }
}

impl ChannelKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Voice => "voice",
            ChannelKind::Text => "text",
        }
    }
    /// How a channel of this kind is named in a label: `#name` for a text
    /// channel, "name, voice channel" for a voice channel.
    #[must_use]
    pub fn label(self, name: &str) -> String {
        match self {
            ChannelKind::Text => format!("#{name}"),
            ChannelKind::Voice => format!("{name}, voice channel"),
        }
    }
}
