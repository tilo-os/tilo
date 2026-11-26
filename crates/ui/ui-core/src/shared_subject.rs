//! What a person has selected in one window, as a subject to send somewhere: the resource
//! it is in, where in it, and the words a chip reads for it.

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionResource, SelectionAnchor, SelectionPlace};

/// A person's selection in one window: the resource it is in, when known, and where in
/// it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ActionResource>,
    pub anchor: SelectionAnchor,
    /// The kind of the resource the place's item belongs to, which picks the published
    /// resource when [`Self::resource`] is absent.
    #[serde(skip)]
    pub resource_kind: Option<String>,
    /// Where the selection is drawn, in the window's logical coordinates: the selected
    /// text's lines, else the text block holding it.
    #[serde(skip)]
    pub bounds: Option<vello::kurbo::Rect>,
    /// The glyph the selected item shows where it is listed, when the app knows one,
    /// so a subject chip leads with the same mark as the row it names; `None` for the
    /// app's own glyph.
    #[serde(skip)]
    pub icon: Option<crate::icons::Icon>,
    /// A file the box carries as it is, already kept where it goes (the recording and picture a
    /// report carries): it is sent as the file, never resolved against what the window shows.
    #[cfg(feature = "model")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<tilo_reference::Attached>,
}

impl SharedSelection {
    /// Selected `text`, with no place.
    #[must_use]
    pub fn of_text(text: &str) -> Self {
        Self {
            anchor: SelectionAnchor::of_text(text),
            ..Self::default()
        }
    }

    /// Several whole items selected, each `(id, label)` in the order shown (files in a
    /// folder, rows of a list): every item named as a place and the excerpt listing their
    /// labels.
    #[must_use]
    pub fn of_items<I, L>(items: impl IntoIterator<Item = (I, L)>) -> Self
    where
        I: Into<String>,
        L: AsRef<str>,
    {
        Self {
            anchor: SelectionAnchor::of_items(items),
            ..Self::default()
        }
    }

    /// The file `file` (an image, a video or any other file), as a subject of its own: its chip
    /// reads its name after the glyph of what it is, and it is sent as the file. Anything else
    /// carries no file and is no subject.
    #[cfg(feature = "model")]
    #[must_use]
    pub fn carrying(file: tilo_reference::Attached) -> Option<Self> {
        use tilo_reference::Attached;
        let icon = match &file {
            Attached::Image { .. } => crate::icons::Icon::Image,
            Attached::Video { .. } => crate::icons::Icon::Film,
            Attached::File { .. } => crate::icons::Icon::Paperclip,
            Attached::Resource { .. } | Attached::Text { .. } => return None,
        };
        Some(Self {
            anchor: SelectionAnchor::of_text(file.label()),
            icon: Some(icon),
            file: Some(file),
            ..Self::default()
        })
    }

    /// This selection at `place`.
    #[must_use]
    pub fn at(mut self, place: SelectionPlace) -> Self {
        self.anchor = self.anchor.at(place);
        self
    }

    /// This selection in `resource`.
    #[must_use]
    pub fn in_resource(mut self, resource: ActionResource) -> Self {
        self.resource = Some(resource);
        self
    }

    /// This selection in a resource of `kind`, resolved against the app's publication.
    #[must_use]
    pub fn in_kind(mut self, kind: impl Into<String>) -> Self {
        self.resource_kind = Some(kind.into());
        self
    }

    /// This selection leading with `icon`, the glyph its item shows where it is listed.
    #[must_use]
    pub fn with_icon(mut self, icon: crate::icons::Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// This selection drawn within `bounds`, in window coordinates.
    #[must_use]
    pub fn within(mut self, bounds: vello::kurbo::Rect) -> Self {
        self.bounds = Some(bounds);
        self
    }

    /// Where the selection is drawn, as the shell protocol carries it: `None` when
    /// unknown or degenerate.
    #[must_use]
    pub fn protocol_bounds(&self) -> Option<[f64; 4]> {
        self.bounds
            .filter(|rect| {
                [rect.x0, rect.y0, rect.x1, rect.y1]
                    .iter()
                    .all(|value| value.is_finite())
                    && rect.width() > 0.0
                    && rect.height() > 0.0
            })
            .map(|rect| [rect.x0, rect.y0, rect.x1, rect.y1])
    }

    /// This selection as a thing to send: the record it is in, with the item or the run
    /// of characters it names, or — when the app publishes no resource to go with the
    /// words — the words quoted.
    #[must_use]
    #[cfg(feature = "model")]
    pub fn to_attached(
        &self,
        app: Option<&str>,
        record: Option<&str>,
    ) -> Option<tilo_reference::Attached> {
        use tilo_reference::{Anchor, Attached, ResourceAt, ResourceRef, Span};

        if let Some(file) = &self.file {
            return Some(file.clone());
        }
        let excerpt = self
            .anchor
            .excerpt
            .as_deref()
            .map(str::trim)
            .filter(|excerpt| !excerpt.is_empty());
        let place = self.anchor.place.as_ref();
        let ids: Vec<String> = self
            .anchor
            .items
            .iter()
            .map(|item| item.item.clone())
            .collect();
        let (Some(resource), Some(app)) = (self.resource.as_ref(), app) else {
            return excerpt.map(|text| Attached::Text {
                text: text.to_owned(),
                from: None,
            });
        };
        let span = place
            .and_then(|place| Some(Span::chars(place.start?, place.end?)))
            .filter(|span| !span.is_empty());
        let anchor = if ids.is_empty() {
            place.map(|place| Anchor::item(place.item.clone()))
        } else {
            Some(Anchor::items(ids))
        };
        let at = ResourceAt {
            app: app.to_owned(),
            record: record.map(str::to_owned),
            reference: ResourceRef {
                kind: resource.kind.clone(),
                id: resource.id.clone(),
                label: resource.label.clone(),
                span,
            },
        };
        if anchor.is_none() && excerpt.is_some() {
            // A selection the app reports only as words: quote them, and say which record
            // they came from so the chip can open it.
            return Some(Attached::Text {
                text: excerpt.unwrap_or_default().to_owned(),
                from: Some(at),
            });
        }
        // What the person saw on the box's chip: the item picked ("#1 Write the launch
        // notes"), never only the list it is in.
        let label = subject_label(self);
        (!label.is_empty()).then_some(Attached::Resource { at, anchor, label })
    }

    /// Whether there is nothing to ask about.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor.is_empty()
    }

    /// Name the published resource the selection is in when the widget did not: the
    /// resource of [`Self::resource_kind`], else the first one.
    pub fn resolve_resource(&mut self, published: &[tilo_actions::ResourceSpec]) {
        if self.resource.is_some() {
            return;
        }
        fn find<'a>(
            resources: &'a [tilo_actions::ResourceSpec],
            kind: &str,
            depth: usize,
        ) -> Option<&'a tilo_actions::ResourceSpec> {
            if depth > 4 {
                return None;
            }
            resources
                .iter()
                .find(|resource| resource.kind == kind)
                .or_else(|| {
                    resources
                        .iter()
                        .find_map(|resource| find(&resource.children, kind, depth + 1))
                })
        }
        let chosen = self
            .resource_kind
            .as_deref()
            .and_then(|kind| find(published, kind, 0))
            .or_else(|| published.first());
        self.resource = chosen.map(|resource| ActionResource {
            id: resource.id.0.clone(),
            kind: resource.kind.clone(),
            label: resource.label.clone(),
        });
    }
}

/// Whether `line` is the excerpt's own count of what it left out (`- and 3
/// more`), which names no subject.
fn is_overflow(line: &str) -> bool {
    line.strip_prefix("- and ")
        .and_then(|rest| rest.strip_suffix(" more"))
        .is_some_and(|count| !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()))
}

/// What a subject chip reads: the words selected — the first line of them, and
/// how many more there are — else what the resource is called, else the item it
/// is.
#[must_use]
pub fn subject_label(subject: &SharedSelection) -> String {
    if let Some(excerpt) = subject.anchor.excerpt.as_deref() {
        let lines: Vec<String> = excerpt
            .lines()
            .map(str::trim)
            .filter(|line| !is_overflow(line))
            .map(|line| {
                line.strip_prefix("- ")
                    .unwrap_or(line)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|line| !line.is_empty())
            .collect();
        if let Some(first) = lines.first() {
            // A list's items are what the selection says it holds; its excerpt
            // lists as many as it could fit.
            let more = subject
                .anchor
                .items
                .len()
                .max(lines.len())
                .saturating_sub(1);
            let mut label: String = first.chars().take(48).collect();
            if more > 0 {
                label.push_str(&format!(" +{more}"));
            }
            return label;
        }
    }
    if let Some(label) = subject
        .resource
        .as_ref()
        .map(|resource| resource.label.trim())
        .filter(|label| !label.is_empty())
    {
        return label.to_owned();
    }
    subject
        .anchor
        .place
        .as_ref()
        .map(|place| place.item.clone())
        .or_else(|| subject.anchor.items.first().map(|item| item.item.clone()))
        .unwrap_or_default()
}
