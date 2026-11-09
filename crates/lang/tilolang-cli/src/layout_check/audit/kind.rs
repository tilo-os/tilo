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
    /// item, its title standing once, in the field that edits it.
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
}

impl Kind {
    pub const ALL: [Self; 38] = [
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
        }
    }
}
