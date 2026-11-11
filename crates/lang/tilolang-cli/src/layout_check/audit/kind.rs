//! The kinds of thing a laid-out window can be wrong about, and how each is named in a
//! report.

/// A kind of layout problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Content reaches past the box it is in, and is cut off or spills over what is next to it.
    Overflow,
    /// Text needs more room than its box gives it, so part of it is not shown.
    ClippedText,
    /// Two siblings cover each other.
    Overlap,
    /// Something a person presses or types into is smaller than the design system's minimum.
    SmallHitTarget,
    /// Content is wider than the window or its scroll view, so the page scrolls sideways.
    ScrollsSideways,
    /// A scroll view with no room on the way it scrolls: what it holds cannot be scrolled into
    /// view, so it is lost (a board lane whose height came from its content).
    LostContent,
    /// An edge between two device pixels at the display's scale, drawn blurred across both.
    OffPixel,
    /// A page whose content is one row stretched over the column: a row is a line, and the page
    /// around it holds it.
    BareRow,
    /// The rows of one box stand on different columns: their titles start at different x, or
    /// their content ends at different edges, or a nesting step does not indent by one gutter.
    RaggedColumns,
    /// Two selection shapes at once: a selected box inside another selected box, the same choice
    /// filled twice.
    DoubleSelection,
    /// A screen made for a narrow window holds its content nearer than a gutter to the edge.
    CrampedGutter,
    /// One screen offers the same way to add something several times: two add affordances in one
    /// list, or the same add label on two controls.
    SeveralAdds,
    /// A name a screen reader announces reads as a code identifier rather than as words.
    CodedName,
    /// A destructive control says so with a verb of its own, where every screen must use one.
    UnsharedVerb,
    /// A surface drawn inside another surface of the same kind: a card in a card.
    NestedCard,
    /// A page's "…" standing in a column rather than at the trailing edge of its title's line:
    /// the door takes a line of its own and shows a lone glyph under the title.
    LoneDoor,
    /// A heading with nothing in it: a band's height taken over the content, where a heading is
    /// the words its section goes by.
    EmptyHeading,
    /// A count shown as zero. A count means something wants attention, so zero is no badge at
    /// all, and no room kept for one.
    ZeroCount,
    /// A column's header drawing a count the sidebar beside it already draws for the place
    /// shown: the same number twice on one screen.
    RepeatedCount,
    /// A surface hanging over the window, a popover or a sheet, reaching past the window's
    /// edges: what it says there is cut off or cannot be pressed.
    PastWindow,
    /// A column's header row that broke the row's own rule: a title cut off while the row could
    /// have given it more (room standing empty beside it, or an action keeping a label it could
    /// show as its icon), or a row that dropped the app's primary action.
    TitleClippedWithRoom,
    /// A word the window shows from a catalogue in another language than the one it speaks: a
    /// window says everything in one language, so a German word in an English window (or the
    /// source standing in for a translation the window's own catalogues lack) is a fault.
    MixedLanguage,
    /// An item standing open where it stands, outside the list that shows it: the row that
    /// opened it is no row of a list the person sees, or more than one row of one list stands
    /// open at a time.
    OpenOutsideList,
    /// A surface with nothing in it: a filled block standing where a person reads it as a card,
    /// a page or a band, and holding no words and nothing laid out of its own.
    EmptySurface,
    /// A column's header naming the page while the page shows the same words as its own
    /// heading, or in the field it edits its title in: one title on screen, and the page's.
    RepeatedPageTitle,
    /// A back control standing beside the sidebar toggle in the same bar row: one corner
    /// offering two ways to navigate, where a screen that went somewhere deeper has one, its
    /// way back at the leading edge.
    DoubleNavigation,
    /// A list or tree whose first level starts more than one nesting step right of the title its
    /// section goes by: a section's title stands on the column its content does, and a first
    /// level keeps no more than the one step a folding chevron hangs in front of it.
    FirstLevelIndent,
    /// One bar showing one control twice: the same action under two ids, the same icon saying
    /// the same words twice, or a second "…" beside the bar's own. One bar shows each control
    /// once, and has one door to what does not fit in it.
    DuplicateBarControl,
    /// An action that asks for its words standing as its icon alone in a bar with room for those
    /// words: a glyph nobody can name, where the bar could have said what it does.
    IconOnlyBarAction,
    /// A pane that only says nothing is chosen, taking the window beside the list it is chosen
    /// from: with nothing chosen the list lists for the whole window, and the pane appears when
    /// something opens in it.
    PlaceholderPane,
    /// A selection lift a list painted across the space around what its row shows: the person's
    /// choice filled edge to edge, gutters and all, where a row paints it around its content.
    SelectionBleed,
    /// A window that shows its bar and, under it, a second header row of its own: two stacked
    /// header rows where one bar carries the app's title, navigation and actions, and the
    /// window's controls stand at its end.
    StackedHeaderRows,
    /// A window whose header names the place it shows while no place in its sidebar stands
    /// chosen: the sidebar offers the place, and the person cannot see which one they are in.
    UnchosenPlace,
    /// A list scrolling inside the page's own scroller: the page scrolls its rows, and the list
    /// inside it keeps a scrollbar of its own and cuts the rows it holds at its edge.
    NestedScroller,
    /// A row's title written a second time in the item the row opened into: the row became the
    /// item, its title standing once, in the field that edits it. The words a box is announced by
    /// are not written, so a row named after its item's own title is no second title on screen.
    RepeatedRowTitle,
    /// A page given a column of its own inside another page: the gutter is counted twice, so
    /// what it holds stands inset from the rows beside it and narrower than them.
    PageInPage,
    /// A window whose view declares one main action that does not stand in its bar as the
    /// primary button: the thing a person is most likely to do looks like every other control,
    /// or is not in the row at all.
    UnmarkedBarPrimary,
    /// Two words painted in one place: labels whose ink stands on each other, neither on a
    /// surface of its own. Whatever draws them — a widget or the bar itself — a person reads one
    /// string of letters, not two.
    TextOnText,
    /// A bar docked at the foot of a column standing wider than the column the content above it
    /// is read at: an entry that adds to a list spans the window beside the list it adds to.
    DockedColumn,
    /// A surface with rounded corners standing against the edge of the column or window it is
    /// in: the curve is cut by the edge, or the surface reads as part of the frame rather than
    /// as a card inside it. A rounded surface keeps a gutter on every side.
    RoundedOnEdge,
    /// A card or a notice standing closer to the sibling above or below it than the toolkit's
    /// section gap: the two read as one block, and a list pressed against a notice says the
    /// notice is a row of it.
    CrampedCard,
    /// A sidebar standing beside content narrower than a column: the pane beside it is a phone's
    /// screen, so the sidebar folds behind the bar's toggle there and the content takes the
    /// window.
    ContentBesideSidebar,
    /// One item shown twice: the document of a row standing open lists an item that stands in
    /// the list again. An item stands in one place — a row of its list, or in the document of
    /// the row it is under, never both.
    ItemShownTwice,
    /// A group heading taking more room over its first row than one heading band, and a finger's
    /// target where the window is held in the hand: the room a heading needs stands over its
    /// line, so what it names stands as close to it at every width.
    HeadingRoom,
    /// A row indented under nothing: the first row a group's heading heads stands a nesting step
    /// in from the level its list starts at, so the row it stands under is no row of its group.
    /// An item the grouping put in another group stands at the group's first level and names the
    /// item it belongs to instead.
    OrphanedRow,
    /// A section's heading with no more room over it than under it: nothing tells it from the
    /// block it follows, so a person reads it as that block's last line rather than as the title
    /// of what comes next.
    CrowdedHeading,
    /// A list standing beside the page it opens that highlights another row than the one the page
    /// shows: the tree says where the person is, and the page says it somewhere else.
    DetailRowNotChosen,
    /// A sidebar whose header row — the words that name its places and the buttons that add to
    /// them — stands only while the window's bar is out: at rest the sidebar starts straight
    /// with its rows, so what names it and adds to it is off the window until the person reaches
    /// for the bar. The sidebar's header is the app's own navigation and stands at every time.
    SidebarHeaderOnReveal,
    /// A name tag presence draws — who is there, what an agent is doing — over a word of the
    /// window, or a second tag for one actor: the word is lost, and one actor reads as two.
    PresenceTag,
}

impl Kind {
    pub const ALL: [Self; 49] = [
        Self::Overflow,
        Self::ClippedText,
        Self::Overlap,
        Self::SmallHitTarget,
        Self::ScrollsSideways,
        Self::LostContent,
        Self::OffPixel,
        Self::BareRow,
        Self::RaggedColumns,
        Self::DoubleSelection,
        Self::CrampedGutter,
        Self::SeveralAdds,
        Self::CodedName,
        Self::UnsharedVerb,
        Self::NestedCard,
        Self::LoneDoor,
        Self::EmptyHeading,
        Self::ZeroCount,
        Self::RepeatedCount,
        Self::PastWindow,
        Self::TitleClippedWithRoom,
        Self::MixedLanguage,
        Self::OpenOutsideList,
        Self::EmptySurface,
        Self::RepeatedPageTitle,
        Self::DoubleNavigation,
        Self::FirstLevelIndent,
        Self::DuplicateBarControl,
        Self::IconOnlyBarAction,
        Self::PlaceholderPane,
        Self::SelectionBleed,
        Self::StackedHeaderRows,
        Self::UnchosenPlace,
        Self::NestedScroller,
        Self::RepeatedRowTitle,
        Self::PageInPage,
        Self::UnmarkedBarPrimary,
        Self::TextOnText,
        Self::DockedColumn,
        Self::RoundedOnEdge,
        Self::CrampedCard,
        Self::ContentBesideSidebar,
        Self::ItemShownTwice,
        Self::HeadingRoom,
        Self::OrphanedRow,
        Self::CrowdedHeading,
        Self::DetailRowNotChosen,
        Self::SidebarHeaderOnReveal,
        Self::PresenceTag,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::ClippedText => "clipped-text",
            Self::Overlap => "overlap",
            Self::SmallHitTarget => "small-hit-target",
            Self::ScrollsSideways => "scrolls-sideways",
            Self::LostContent => "lost-content",
            Self::OffPixel => "off-pixel",
            Self::BareRow => "bare-row",
            Self::RaggedColumns => "ragged-columns",
            Self::DoubleSelection => "double-selection",
            Self::CrampedGutter => "cramped-gutter",
            Self::SeveralAdds => "several-adds",
            Self::CodedName => "coded-name",
            Self::UnsharedVerb => "unshared-verb",
            Self::NestedCard => "nested-card",
            Self::LoneDoor => "lone-door",
            Self::EmptyHeading => "empty-heading",
            Self::ZeroCount => "zero-count",
            Self::RepeatedCount => "repeated-count",
            Self::PastWindow => "past-window",
            Self::TitleClippedWithRoom => "title-clipped-with-room",
            Self::MixedLanguage => "mixed-language",
            Self::OpenOutsideList => "open-outside-list",
            Self::EmptySurface => "empty-surface",
            Self::RepeatedPageTitle => "repeated-page-title",
            Self::DoubleNavigation => "double-navigation",
            Self::FirstLevelIndent => "first-level-indent",
            Self::DuplicateBarControl => "duplicate-bar-control",
            Self::IconOnlyBarAction => "icon-only-bar-action",
            Self::PlaceholderPane => "placeholder-pane",
            Self::SelectionBleed => "selection-bleed",
            Self::StackedHeaderRows => "stacked-header-rows",
            Self::UnchosenPlace => "unchosen-place",
            Self::NestedScroller => "nested-scroller",
            Self::RepeatedRowTitle => "repeated-row-title",
            Self::PageInPage => "page-in-page",
            Self::UnmarkedBarPrimary => "unmarked-bar-primary",
            Self::TextOnText => "text-on-text",
            Self::DockedColumn => "docked-column",
            Self::RoundedOnEdge => "rounded-on-edge",
            Self::CrampedCard => "cramped-card",
            Self::ContentBesideSidebar => "content-beside-sidebar",
            Self::ItemShownTwice => "item-shown-twice",
            Self::HeadingRoom => "heading-room",
            Self::OrphanedRow => "orphaned-row",
            Self::CrowdedHeading => "crowded-heading",
            Self::DetailRowNotChosen => "detail-row-not-chosen",
            Self::SidebarHeaderOnReveal => "sidebar-header-on-reveal",
            Self::PresenceTag => "presence-tag",
        }
    }
}
