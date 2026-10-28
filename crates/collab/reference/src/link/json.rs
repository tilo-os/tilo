//! The JSON form the page reads and the corpus is written in. Secrets are
//! never part of it: the page receives them only from the scrubber.

use serde::{Deserialize, Serialize};

use super::{
    AppId, HostOrigin, Link, LinkError, LinkTarget, Mode, RecordSegment, ResourceAddress, Scope,
    Slug,
};
use crate::{Span, SpanUnit};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeJson {
    Desktop,
    App,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ScopeJson {
    Own,
    Room {
        host: String,
        room: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slug: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpanUnitJson {
    Chars,
    Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpanJson {
    pub start: u64,
    pub end: u64,
    pub unit: SpanUnitJson,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceJson {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<SpanJson>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetJson {
    pub scope: ScopeJson,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// `""` is the package's primary record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceJson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discuss: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteJson {
    pub mode: ModeJson,
    pub target: TargetJson,
}

impl From<&LinkTarget> for TargetJson {
    fn from(target: &LinkTarget) -> Self {
        Self {
            scope: match &target.scope {
                Scope::Own => ScopeJson::Own,
                Scope::Room { host, room, slug } => ScopeJson::Room {
                    host: host.as_str().to_owned(),
                    room: room.hyphenated().to_string(),
                    slug: slug.as_ref().map(|slug| slug.as_str().to_owned()),
                },
            },
            app: target.app.as_ref().map(|app| app.as_str().to_owned()),
            record: target
                .record
                .as_ref()
                .map(|record| record.provider_part().to_owned()),
            resource: target.resource.as_ref().map(|resource| ResourceJson {
                kind: resource.kind.clone(),
                id: resource.id.clone(),
                span: resource.span.map(|span| SpanJson {
                    start: u64::try_from(span.start).unwrap_or(u64::MAX),
                    end: u64::try_from(span.end).unwrap_or(u64::MAX),
                    unit: match span.unit {
                        SpanUnit::Chars => SpanUnitJson::Chars,
                        SpanUnit::Bytes => SpanUnitJson::Bytes,
                    },
                }),
            }),
            view: target.view.clone(),
            discuss: target
                .discuss
                .as_ref()
                .map(|discuss| discuss.as_str().to_owned()),
        }
    }
}

impl TryFrom<TargetJson> for LinkTarget {
    type Error = LinkError;

    fn try_from(json: TargetJson) -> Result<Self, LinkError> {
        let scope = match json.scope {
            ScopeJson::Own => Scope::Own,
            ScopeJson::Room { host, room, slug } => {
                let parsed = uuid::Uuid::try_parse(&room).map_err(|_| LinkError::BadRoom)?;
                if parsed.hyphenated().to_string() != room {
                    return Err(LinkError::BadRoom);
                }
                Scope::Room {
                    host: HostOrigin::new(&host)?,
                    room: parsed,
                    slug: slug.as_deref().map(Slug::parse).transpose()?,
                }
            }
        };
        let offset = |value: u64| usize::try_from(value).map_err(|_| LinkError::BadSpan);
        let target = Self {
            scope,
            app: json.app.map(AppId::new).transpose()?,
            record: json.record.as_deref().map(RecordSegment::new).transpose()?,
            resource: json
                .resource
                .map(|resource| -> Result<ResourceAddress, LinkError> {
                    Ok(ResourceAddress {
                        kind: resource.kind,
                        id: resource.id,
                        span: resource
                            .span
                            .map(|span| -> Result<Span, LinkError> {
                                let unit = match span.unit {
                                    SpanUnitJson::Chars => SpanUnit::Chars,
                                    SpanUnitJson::Bytes => SpanUnit::Bytes,
                                };
                                Ok(Span {
                                    start: offset(span.start)?,
                                    end: offset(span.end)?,
                                    unit,
                                })
                            })
                            .transpose()?,
                    })
                })
                .transpose()?,
            view: json.view,
            discuss: json.discuss.as_deref().map(Slug::parse).transpose()?,
        };
        target.check()?;
        Ok(target)
    }
}

impl From<&Link> for RouteJson {
    fn from(link: &Link) -> Self {
        Self {
            mode: match link.mode {
                Mode::Desktop => ModeJson::Desktop,
                Mode::App => ModeJson::App,
            },
            target: TargetJson::from(&link.target),
        }
    }
}
