//! [`MediaGrid`], [`MediaCard`] and [`MediaTransport`] as described components: what plays
//! and what a library shows, each steered by the view's own events.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use crate::arriving::Arrival;
use crate::component::{
    Accepts, Component, Described, Event, PartType, ResourceId, SharedRows, Source,
};
use crate::icons::Icon;
use crate::pictures::Pictures;
use crate::tokens::Rhythm;
use crate::widget::arrange::{Fit, Part, Row, Sizing};
use crate::widget::media_card::{CardSlot, MediaSize};
use crate::widget::virtual_list::{ListModel, ListModelChange};
use crate::widget::{Element, MediaAspect, MediaCard, MediaGrid, MediaSource, VirtualList};

// The `MediaAspect` choice is declared beside the enum, in `media_card.rs`.

/// The row height a host-rendered grid reckons a band of cards at before it is measured.
const BAND_ESTIMATE: f32 = 160.0;

/// Cards in a grid: a library, an album's covers, a shelf of books, a set of channels.
#[derive(Component, Default)]
#[component(name = "MediaGrid", role = "Grid")]
pub struct MediaGridProps {
    /// How large each card is: how wide a column holds.
    pub size: Option<MediaSize>,
    /// The cards are photos at their own shape: justified rows that span the width, rather
    /// than equal columns.
    pub justified: bool,
    /// The cards, in order: given whole, or built by the host as the grid shows them, so a
    /// library of ten thousand photographs costs the cards on screen.
    #[component(part(default))]
    pub items: DescribedCards,
    /// A range of its cards was taken: carries the cards it stood on, each by the item it shows,
    /// in the order they stand. A press with shift takes the cards from the one pressed last to
    /// the one pressed now; a press held and dragged over the cards takes the ones it sweeps.
    #[component(event(value = Vec<ResourceId>))]
    pub select: Event,
}

/// The cards of a grid: given whole, or a host rendering them as it shows them.
#[derive(Default)]
pub struct DescribedCards {
    held: CardsHeld,
}

#[derive(Default)]
enum CardsHeld {
    #[default]
    Nothing,
    /// Cards a host renders as they are shown.
    Rendered(SharedRows),
    /// Cards given whole, in order.
    Given(Vec<MediaCardProps>),
}

impl PartType for DescribedCards {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(MediaCardProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        if let Some(rows) = source.rows(part) {
            self.held = CardsHeld::Rendered(rows);
            return Ok(());
        }
        let mut cards = Vec::new();
        source.components(part, &mut |child| {
            cards.push(MediaCardProps::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        self.held = CardsHeld::Given(cards);
        Ok(())
    }
}

impl DescribedCards {
    /// The cards given whole, in order: what a grid of a handful shows without a host.
    #[cfg(test)]
    pub(crate) fn given(cards: Vec<MediaCardProps>) -> Self {
        Self {
            held: CardsHeld::Given(cards),
        }
    }

    /// The cards given whole, read by a test: empty for one a host renders.
    #[cfg(test)]
    pub(crate) fn held(&self) -> &[MediaCardProps] {
        match &self.held {
            CardsHeld::Given(cards) => cards,
            CardsHeld::Nothing | CardsHeld::Rendered(_) => &[],
        }
    }
}

/// The rows of a host-rendered grid as the shared list reads them: a row is a band of `columns`
/// cards, and the band's cards are the list's items, rendered as the band is shown.
struct Bands {
    rows: SharedRows,
    /// How many cards a band holds, as the grid's last layout worked out from its width.
    columns: Rc<Cell<usize>>,
    /// The columns the rows were last read at: a change re-reads them.
    seen: Cell<usize>,
}

impl Bands {
    /// The cards of band `index`, by their place in the grid.
    fn band(&self, index: usize) -> Range<usize> {
        let columns = self.columns.get().max(1);
        let len = self.rows.borrow().len();
        let start = index.saturating_mul(columns).min(len);
        start..start.saturating_add(columns).min(len)
    }
}

impl ListModel for Bands {
    type Key = usize;

    fn len(&self) -> usize {
        let columns = self.columns.get().max(1);
        self.rows.borrow().len().div_ceil(columns)
    }

    fn key_at(&self, index: usize) -> Option<usize> {
        (index < self.len()).then_some(index)
    }

    fn index_of(&self, key: &usize) -> Option<usize> {
        (*key < self.len()).then_some(*key)
    }

    /// The cards of the bands the list shows are what the host is asked to render.
    fn request_range(&mut self, range: Range<usize>, _open: Option<&usize>) {
        let columns = self.columns.get().max(1);
        let len = self.rows.borrow().len();
        let start = range.start.saturating_mul(columns).min(len);
        let end = range.end.saturating_mul(columns).min(len);
        if start < end {
            self.rows.borrow_mut().request_range(start..end, None);
        }
    }

    /// A band is shown once every card in it is rendered; until then the list keeps its place.
    fn row_state(&self, index: usize) -> Arrival<(), &str> {
        let rows = self.rows.borrow();
        for at in self.band(index) {
            if let Some(key) = rows.key_at(at)
                && !rows.rendered(&key)
            {
                return Arrival::local();
            }
        }
        Arrival::Ready(())
    }

    fn poll_changes(&mut self) -> Vec<ListModelChange<usize>> {
        let columns = self.columns.get().max(1);
        let changed = self.seen.get() != columns;
        self.seen.set(columns);
        let mut rows = self.rows.borrow_mut();
        let arrived = !rows.take_changes().is_empty();
        if changed || arrived {
            vec![ListModelChange::Reset]
        } else {
            Vec::new()
        }
    }

    fn needs_poll(&self) -> bool {
        self.seen.get() != self.columns.get().max(1) || self.rows.borrow().has_changes()
    }
}

/// What a grid whose cards a host builds keeps to take a range of them: their items by the
/// place each stands in, and the place the pointer is over as the card under it says.
#[derive(Default)]
struct GridRange {
    order: Rc<std::cell::RefCell<Vec<Option<ResourceId>>>>,
    hovered: Rc<Cell<Option<usize>>>,
}

/// A band of cards as a row: each card the share of the width a column gets.
fn build_band(bands: &Bands, index: usize, gap: Rhythm, range: &GridRange) -> Element {
    let mut line = Row::row().fill_width().gap(gap);
    // The band's cards are read before the rows are borrowed to read them: the range is the
    // band's own, worked out from the same rows.
    let band = bands.band(index);
    let rows = Rc::clone(&bands.rows);
    let mut held = rows.borrow_mut();
    for at in band {
        let Some(key) = held.key_at(at) else {
            continue;
        };
        let mut read = None;
        let rendered = held.read(&key, &mut |source| {
            read = Some(MediaCardProps::from_source(source).map_err(|error| error.to_string())?);
            Ok(())
        });
        let props = match rendered {
            Ok(true) => read,
            _ => None,
        };
        let props = props.unwrap_or_else(|| MediaCardProps {
            key: key.clone(),
            ..MediaCardProps::default()
        });
        // The grid takes a range of its cards by their places: each card says the place it
        // stands in, and the item it shows is what the range reports.
        {
            let mut order = range.order.borrow_mut();
            let len = order.len();
            order.resize((at + 1).max(len), None);
            if let Some(held) = order.get_mut(at) {
                held.clone_from(&props.item);
            }
        }
        let card = card(props).slot(CardSlot {
            hovered: Rc::clone(&range.hovered),
            index: at,
        });
        let mut part = Part::new(Box::new(card) as Element);
        part.fit = Fit::hug().width(Sizing::fill(1.0));
        line = line.push_part(part);
    }
    Box::new(line)
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
        Event::View(intent) => card.on_open(intent.sender()),
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

    fn from_props(key: &str, props: MediaGridProps) -> Self {
        let MediaGridProps {
            size,
            justified,
            items,
            select,
        } = props;
        let size = size.unwrap_or_default();
        match items.held {
            // A host renders the cards: the grid is the shared list, a band of cards to a row,
            // and costs the bands on screen rather than the library.
            CardsHeld::Rendered(rows) => {
                let columns = Rc::new(Cell::new(1));
                let model = Bands {
                    rows: Rc::clone(&rows),
                    columns: Rc::clone(&columns),
                    seen: Cell::new(1),
                };
                let range = Rc::new(GridRange::default());
                let swept = Rc::clone(&range);
                let gap = Rhythm::Section;
                let list = VirtualList::new(
                    model,
                    BAND_ESTIMATE,
                    move |bands: &Bands, index: usize, _key: &usize| {
                        build_band(bands, index, gap, &swept)
                    },
                )
                .filling()
                .with_id(key);
                let mut grid = MediaGrid::hosting(Box::new(list));
                grid.set_size(size);
                grid.set_columns(columns);
                grid.set_range(Rc::clone(&range.order), Rc::clone(&range.hovered));
                grid.set_select(select);
                grid
            }
            CardsHeld::Given(cards) => {
                let order: Rc<std::cell::RefCell<Vec<Option<ResourceId>>>> = Rc::default();
                let hovered: Rc<Cell<Option<usize>>> = Rc::default();
                *order.borrow_mut() = cards.iter().map(|props| props.item.clone()).collect();
                let cards: Vec<MediaCard> = cards
                    .into_iter()
                    .enumerate()
                    .map(|(index, props)| {
                        card(props).slot(CardSlot {
                            hovered: Rc::clone(&hovered),
                            index,
                        })
                    })
                    .collect();
                let grid = if justified {
                    MediaGrid::justified(cards)
                } else {
                    MediaGrid::new(cards)
                };
                let mut grid = grid.with_size(size);
                grid.set_range(order, hovered);
                grid.set_select(select);
                grid
            }
            CardsHeld::Nothing => {
                let mut grid = MediaGrid::new(Vec::new()).with_size(size);
                grid.set_select(select);
                grid
            }
        }
    }

    /// The cards, their size and how they stand change where the grid stands.
    fn set_props(&mut self, props: MediaGridProps) -> bool {
        let MediaGridProps {
            size,
            justified,
            items,
            select,
        } = props;
        let cards = match items.held {
            // A host-rendered grid is built again over the rows it is given.
            CardsHeld::Rendered(_) => return false,
            CardsHeld::Given(cards) => cards,
            CardsHeld::Nothing => Vec::new(),
        };
        self.set_justified(justified);
        self.set_size(size.unwrap_or_default());
        self.set_select(select);
        let (order, hovered) = self.range_cells();
        *order.borrow_mut() = cards.iter().map(|props| props.item.clone()).collect();
        self.set_cards(
            cards
                .into_iter()
                .enumerate()
                .map(|(index, props)| {
                    card(props).slot(CardSlot {
                        hovered: Rc::clone(&hovered),
                        index,
                    })
                })
                .collect(),
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::RowSource;
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    /// A host rendering ten thousand cards, recording which cards it is asked for: the grid's
    /// rows are what it renders, so the cards it is asked for are the cards laid out.
    struct FakeRows {
        len: usize,
        rendered: RefCell<BTreeSet<usize>>,
        asked: Rc<RefCell<Vec<Range<usize>>>>,
    }

    /// The rows, and the ranges they were asked for, which the test reads beside the trait
    /// object.
    fn fake_rows(len: usize) -> (SharedRows, Rc<RefCell<Vec<Range<usize>>>>) {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let rows: SharedRows = Rc::new(RefCell::new(FakeRows {
            len,
            rendered: RefCell::new(BTreeSet::new()),
            asked: Rc::clone(&asked),
        }));
        (rows, asked)
    }

    impl RowSource for FakeRows {
        fn len(&self) -> usize {
            self.len
        }

        fn key_at(&self, index: usize) -> Option<String> {
            (index < self.len).then(|| index.to_string())
        }

        fn index_of(&self, key: &str) -> Option<usize> {
            key.parse().ok().filter(|index| *index < self.len)
        }

        fn request_range(&mut self, range: Range<usize>, _open: Option<String>) {
            self.asked.borrow_mut().push(range.clone());
            let mut rendered = self.rendered.borrow_mut();
            for index in range {
                rendered.insert(index);
            }
        }

        fn rendered(&self, key: &str) -> bool {
            key.parse()
                .ok()
                .is_some_and(|index| self.rendered.borrow().contains(&index))
        }

        fn read(
            &mut self,
            _key: &str,
            _read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
        ) -> Result<bool, String> {
            Ok(false)
        }

        fn key_presenting(&self, _id: &str) -> Option<String> {
            None
        }

        fn take_changes(&mut self) -> Vec<ListModelChange<String>> {
            Vec::new()
        }

        fn has_changes(&self) -> bool {
            false
        }
    }

    /// Ten thousand photographs in a five-column grid: the bands the list shows are asked for,
    /// and each band is one range of five cards, never the library.
    #[test]
    fn a_ten_thousand_card_grid_asks_for_only_the_cards_it_shows() {
        let (rows, asked) = fake_rows(10_000);
        let mut bands = Bands {
            rows: Rc::clone(&rows),
            columns: Rc::new(Cell::new(5)),
            seen: Cell::new(5),
        };
        assert_eq!(bands.len(), 2_000, "ten thousand cards in bands of five");
        bands.request_range(0..12, None);
        bands.request_range(100..103, None);
        let asked = asked.borrow().clone();
        assert_eq!(
            asked,
            vec![0..60, 500..515],
            "a band of five cards, by its range"
        );
        let laid_out: usize = asked.iter().map(|range| range.end - range.start).sum();
        assert_eq!(laid_out, 75, "75 cards of 10,000 are asked for");
        assert!(matches!(bands.row_state(0), Arrival::Ready(())));
        assert!(
            !matches!(bands.row_state(500), Arrival::Ready(())),
            "a band the host has not rendered is on its way, not shown"
        );
    }

    /// A host-rendered grid is the shared list over the cards, so opening it builds no card: a
    /// card is built when its band is shown.
    #[test]
    fn a_host_rendered_grid_holds_the_list_and_no_card() {
        use crate::widget::Widget;
        let grid = MediaGrid::from_props(
            "library",
            MediaGridProps {
                size: None,
                justified: false,
                items: DescribedCards {
                    held: CardsHeld::Rendered(fake_rows(10_000).0),
                },
                select: Default::default(),
            },
        );
        assert!(grid.hosted());
        assert_eq!(
            grid.children().len(),
            0,
            "no band is built until it is shown"
        );
    }
}
