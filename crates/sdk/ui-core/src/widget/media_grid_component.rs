//! [`MediaGrid`], [`MediaCard`] and [`MediaTransport`] as described components: what plays
//! and what a library shows, each steered by the view's own events.

use crate::component::{Component, Described, Event, ResourceId, SlotType, Source};
use crate::icons::Icon;
use crate::pictures::Pictures;
use crate::widget::media_card::MediaSize;
use crate::widget::{MediaAspect, MediaCard, MediaGrid, MediaSource};

// The `MediaAspect` choice is declared beside the enum, in `media_card.rs`.

/// Cards in a grid: a library, an album's covers, a shelf of books, a set of channels.
#[derive(Component, Default)]
#[component(name = "MediaGrid", role = "Grid")]
pub struct MediaGridProps {
    /// How large each card is: how wide a column holds.
    pub size: Option<MediaSize>,
    /// The cards are photos at their own shape: justified rows that span the width, rather
    /// than equal columns.
    pub justified: bool,
    /// The cards, in order.
    #[component(slot(default))]
    pub items: Vec<MediaCardProps>,
}

/// One card of a grid: the art (a cover, a thumbnail, a photo) with its title and up to two
/// meta lines under it.
#[derive(Component, Default)]
#[component(
    name = "MediaCard",
    role = "ListItem",
    label = "title",
    presents = "item"
)]
pub struct MediaCardProps {
    /// The card's stable key.
    #[component(key)]
    pub key: String,
    /// The resource the card stands for: what a press opens.
    #[component(required, positional)]
    pub item: Option<ResourceId>,
    /// What the card is called, under its art. Without one it is art only (a photo).
    pub title: Option<String>,
    /// Its art: a picture by its address, loaded as it arrives.
    pub picture: Option<crate::widget::PictureSource>,
    /// The icon of a kind of thing, on the neutral tile: a podcast, a folder.
    pub icon: Option<Icon>,
    /// One line under the title (an artist, a channel, an author). Give two for two lines.
    pub meta: Vec<String>,
    /// The shape of its art, `square` unless said.
    pub aspect: Option<MediaAspect>,
    /// Whether it is the selected card.
    pub selected: bool,
    /// It was pressed: open what it stands for.
    #[component(event(press))]
    pub activate: Event,
    /// The window's pictures, through which a picture loads.
    #[component(skip)]
    pub pictures: Option<Pictures>,
}

impl SlotType for Vec<MediaCardProps> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::Component(MediaCardProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let mut read = Vec::new();
        source.components(slot, &mut |child| {
            read.push(MediaCardProps::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        *self = read;
        Ok(())
    }
}

/// The retained card of `props`, its art loaded through `pictures`.
pub(crate) fn card(props: MediaCardProps) -> MediaCard {
    let MediaCardProps {
        key,
        item: _,
        title,
        picture,
        icon,
        meta,
        aspect,
        selected,
        activate,
        pictures,
    } = props;
    let pictures = pictures.unwrap_or_else(Pictures::window);
    let mut card = MediaCard::new(key, art_of(picture, icon, &pictures));
    if let Some(title) = title {
        card = card.title(title);
    }
    for line in meta {
        card = card.meta(line);
    }
    if let Some(aspect) = aspect {
        card = card.aspect(aspect);
    }
    card = card.selected(selected);
    match activate {
        Event::None => card,
        Event::Action(binding) => card.on_action(binding),
        Event::View(intent) => card.on_view(intent.sender()),
    }
}

/// Where a card takes its art from, as the view gives it.
fn art_of(
    picture: Option<crate::widget::PictureSource>,
    icon: Option<Icon>,
    pictures: &Pictures,
) -> MediaSource {
    match (picture, icon) {
        (Some(picture), _) if !picture.is_empty() => MediaSource::Url {
            store: pictures.store().clone(),
            url: picture.src,
        },
        (_, Some(icon)) => MediaSource::Kind(icon),
        _ => MediaSource::Placeholder,
    }
}

impl Described for MediaGrid {
    type Props = MediaGridProps;

    fn from_props(_key: &str, props: MediaGridProps) -> Self {
        let MediaGridProps {
            size,
            justified,
            items,
        } = props;
        let cards: Vec<MediaCard> = items.into_iter().map(card).collect();
        let mut grid = if justified {
            Self::justified(cards)
        } else {
            Self::new(cards)
        };
        if let Some(size) = size {
            grid = grid.with_size(size);
        }
        grid
    }

    /// The cards, their size and how they stand change where the grid stands.
    fn set_props(&mut self, props: MediaGridProps) -> bool {
        let MediaGridProps {
            size,
            justified,
            items,
        } = props;
        self.set_justified(justified);
        self.set_size(size.unwrap_or_default());
        self.set_cards(items.into_iter().map(card).collect());
        true
    }
}
