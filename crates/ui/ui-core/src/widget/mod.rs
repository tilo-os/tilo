//! The toolkit's widgets above its base: each one's module, and what it exports.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the base's widget contract, for the widgets declared here"
)]
pub use tilo_ui_base::widget::*;

#[cfg(any(test, feature = "test-support"))]
mod test_controls;
#[cfg(any(test, feature = "test-support"))]
pub use {test_controls::Unattached, test_controls::test_value_action_binding};
#[cfg(feature = "model")]
pub mod action_proposal;
#[cfg(feature = "model")]
pub mod collaborative;
#[cfg(feature = "model")]
pub use collaborative::{Collaborative, Continuable, handle_collab_event, take_collab_requests};
#[cfg(feature = "model")]
pub use {
    action_proposal::ActionProposal, action_proposal::ActionProposalView,
    action_proposal::DiffLine, action_proposal::DiffTag, action_proposal::ProposalDecision,
    action_proposal::ProposalTarget,
};
mod composed;
/// What a widget publishes as resources, and the walk that runs a control's queued intent
/// against them.
pub mod resources;
pub use resources::ActionDirectoryResourcePage;
pub mod search_request;
pub use composed::Composed;
pub mod decorator;
pub mod document_offers;
pub use {document_offers::DocumentOffer, document_offers::PaintedOffers};
mod button;
#[cfg(any(test, feature = "test-support"))]
pub mod replaced_tripwire;
pub use {
    button::Button, button::ButtonFace, button::ButtonProps, button::ButtonRole,
    button::REST_BORDER_WIDTH, button::face_colors, button::focus_ring_reach,
};
mod control_size;
pub use control_size::ControlSize;
mod bool_control;
pub(crate) use bool_control::BoolAction;
mod checkbox;
pub use {
    checkbox::CheckGlyph, checkbox::CheckMotion, checkbox::CheckPose, checkbox::Checkbox,
    checkbox::CheckboxFace, checkbox::CheckboxProps,
};
mod column;
pub use column::Column;
mod composition;
mod flow;
mod form;
mod form_composition;
pub use {
    form::CheckControl, form::FormControl, form::Labelled, form::LabelledProps,
    form::labeled_check, form::labeled_field,
};
pub use {
    form_composition::Field, form_composition::Form, form_composition::FormPage,
    form_composition::FormSection, form_composition::Page,
};
mod embedded;
pub use embedded::Embedded;
mod row;
pub use row::Row;
mod face_motions;
pub use face_motions::FaceMotions;
mod toggle;
pub use {
    toggle::SwitchMotion, toggle::SwitchPose, toggle::Toggle, toggle::ToggleColors,
    toggle::ToggleFace, toggle::ToggleProps, toggle::toggle_accessibility,
    toggle::toggle_face_colors,
};
mod stack;
pub use stack::Stack;
mod container;
pub use container::Container;
mod document_view;
pub use document_view::ModelView;
mod divider;
pub use {divider::Divider, divider::DividerAxis, divider::RuleEdge, divider::paint_rule};
mod rule;
pub use {rule::Rule, rule::RuleProps};
mod link;
pub use {link::Link, link::LinkProps};
mod flap;
pub use {flap::FlapState, flap::SplitLayout};
mod bottom_sheet;
pub use {
    bottom_sheet::BottomSheet, bottom_sheet::SheetDetent, bottom_sheet::SheetFrame,
    bottom_sheet::SheetGeometry, bottom_sheet::SheetInput,
};
mod page_slide;
pub use {page_slide::PageSlide, page_slide::PageSlideProps};
mod fullscreen_view;
pub use {fullscreen_view::Fullscreen, fullscreen_view::FullscreenProps, fullscreen_view::Screen};
mod stage;
pub use {stage::Stage, stage::StagePinProps, stage::StageProps};
mod sidebar;
pub use {
    sidebar::DEFAULT_DRAWER_BREAKPOINT, sidebar::DEFAULT_DRAWER_DOCKED_WIDTH,
    sidebar::DEFAULT_DRAWER_WIDTH, sidebar::SidebarDrawer, sidebar::SidebarSide,
};
mod nav_shell;
#[cfg(feature = "native")]
pub use nav_shell::memory::remember_for_app as remember_sidebar_widths_for_app;
pub mod window_controls;
pub mod window_header;
pub mod window_toolbar_row;
pub use {
    nav_shell::HeldNotice, nav_shell::InspectorFrame, nav_shell::InspectorLink,
    nav_shell::InspectorPlacement, nav_shell::NavGeometry, nav_shell::NavMode, nav_shell::NavPane,
    nav_shell::NavShell, nav_shell::NavShellProps, nav_shell::OverviewPlacement,
    nav_shell::SidebarPlacement, nav_shell::WINDOW_BACK, nav_shell::WINDOW_SIDEBAR,
    nav_shell::WindowBarLink,
};
pub use {
    window_header::ColumnFrame, window_header::HISTORY_CONTROL_ID, window_header::HeaderStatus,
    window_header::HistoryControl, window_header::HistoryOpener, window_header::STATUS_ACTION_ID,
    window_header::StatusAction, window_header::WindowBar, window_header::WindowBarPart,
    window_header::WindowHeader, window_header::WindowHistory, window_header::WindowStatus,
    window_header::merged_row_holds_search, window_header::merged_row_width,
    window_header::merged_row_width_in,
};
mod nav_list;
pub use nav_list::nav_row;
pub use {
    nav_list::NavAction, nav_list::NavArt, nav_list::NavDrop, nav_list::NavEntry,
    nav_list::NavFeed, nav_list::NavItem, nav_list::NavItemProps, nav_list::NavItemStatus,
    nav_list::NavList, nav_list::NavListProps, nav_list::NavPerson, nav_list::NavSearchTerm,
    nav_list::NavSection, nav_list::NavSectionProps,
};
mod document_viewport;
pub use {
    document_viewport::ContentDraw, document_viewport::DEFAULT_MAX_ZOOM,
    document_viewport::DEFAULT_MIN_ZOOM, document_viewport::DEFAULT_PADDING,
    document_viewport::DEFAULT_PAN_STEP, document_viewport::DocumentViewport,
    document_viewport::PAN_SLOP, document_viewport::SmallContentAlign,
    document_viewport::ViewportState, document_viewport::ViewportZoom,
};
mod floating_window;
pub use {
    floating_window::FloatingWindow, floating_window::MIN_HEIGHT, floating_window::MIN_WIDTH,
    floating_window::TITLE_BAR_HEIGHT,
};
mod text;
pub use {text::Text, text::TextProps};
mod relative_time;
pub use {
    relative_time::RelativeTime, relative_time::TimeProps, relative_time::clock_text,
    relative_time::next_change, relative_time::relative_text,
};
mod money;
pub use {money::Money, money::MoneyProps};
mod clock;
pub use {
    clock::ClockProps, clock::ClockReading, clock::WallClock, clock::reading as clock_reading,
};
mod countdown;
mod timer;
pub use {
    countdown::Countdown, countdown::CountdownProps, countdown::CountdownReading,
    countdown::reading as countdown_reading,
};
pub use {timer::Timer, timer::TimerProps, timer::set_window_shown, timer::window_shown};
mod when;
pub use {when::Absent, when::Fitted, when::When, when::optional};
mod part;
pub use {part::Part, part::PartHandle, part::Switch};
mod columns;
pub use columns::Columns;
/// The layout primitives: `Stack`, `Row`, `Grid`, `Split`, `Page`, `Layers`.
pub mod arrange;
mod image;
pub use {
    image::Image, image::ImageDecodeError, image::ImageFit, image::ImageProps, image::PictureSource,
};
mod image_crop;
pub use {
    image_crop::CropEditor, image_crop::CropSelection, image_crop::ImageCropEditor,
    image_crop::ImageCropEditorProps,
};
mod slider;
pub use {
    slider::Slider, slider::SliderFace, slider::SliderProps, slider::SliderTouch,
    slider::SliderTouchStep, slider::paint_held_readout,
};
mod text_input;
pub use {
    text_input::AccessoryAction, text_input::FieldFace, text_input::PlaceholderFit,
    text_input::PromptFit, text_input::SpanHighlighter, text_input::SpellingMenu,
    text_input::TextActionRequest, text_input::TextActionResult, text_input::TextInput,
    text_input::TextInputKind, text_input::TextInputProps, text_input::paint_invalid_edge,
    text_input::paint_preedit_underline,
};
pub mod code_editor;
pub use {
    code_editor::CodeEditor, code_editor::CodeEditorProps, code_editor::Highlighter,
    code_editor::HlState, code_editor::Language, code_editor::LineSpans, code_editor::Span,
    code_editor::TokenKind,
};
mod editable_label;
pub use {
    editable_label::EditOutcome, editable_label::EditableLabel, editable_label::EditableLabelProps,
};
pub mod autocomplete;
pub mod filter_chip;
pub use {
    autocomplete::Autocomplete, autocomplete::AutocompleteOutcome, autocomplete::AutocompleteProps,
    autocomplete::CheckColumn, autocomplete::Suggester, autocomplete::Suggestion,
    autocomplete::SuggestionList, autocomplete::SuggestionProps,
};
pub mod mention_picker;
pub use {mention_picker::MentionCandidate, mention_picker::MentionQuery};
pub mod completion;
pub(crate) mod completion_card;
pub mod text_completion;
pub use {text_completion::EmojiQuery, text_completion::TextCompletion};
mod number_input;
pub use {
    number_input::NumberInput, number_input::NumberInputProps, number_input::NumberStepperFace,
    number_input::StepperPart,
};
pub mod marks;
pub use {marks::Mark, marks::MarkSpan};
pub mod document_line;
pub mod filter_menu;
pub mod markdown_input;
pub mod paint_damage;
pub use filter_menu::FilterMenu;
pub mod link_popover;
pub use {link_popover::LinkPopover, link_popover::PageEntry};
pub mod page_sidebar;
pub use {
    page_sidebar::DeclaredRail, page_sidebar::PageSidebar, page_sidebar::PageSidebarEvent,
    page_sidebar::PageSidebarProps,
};
mod dropdown;
pub use {
    dropdown::Dropdown, dropdown::DropdownFace, dropdown::DropdownProps,
    dropdown::FILTER_MIN_OPTIONS,
};
pub mod tabs;
pub use {
    tabs::AgentMark, tabs::AppKind, tabs::PresenceFaceProps, tabs::TabAuthorProps, tabs::TabBar,
    tabs::TabDescriptor, tabs::TabFaceSpec, tabs::TabHolder, tabs::TabItemProps, tabs::TabMark,
    tabs::TabStrip, tabs::TabStripProps, tabs::draw_app_mark, tabs::pack_point, tabs::unpack_point,
};
pub mod scrollbar;
pub use {scrollbar::PressOutcome, scrollbar::Scrollbar, scrollbar::TrackMark};
pub mod board;
pub use {
    board::Board, board::BoardCardProps, board::BoardDrop, board::BoardListProps, board::BoardProps,
};
pub use {drag_bus::InFlight, drag_bus::Landed};
pub use {
    drag_run::DragDrop, drag_run::DragRun, drag_run::RunAxis, drag_run::RunGroup, drag_run::RunItem,
};
pub use {drag_run_component::DragItemProps, drag_run_component::DragRunProps};
pub use {drag_source::DragSource, drag_source::DragSourceProps, drag_source::RESOURCE_PAYLOAD};
pub use {drop_target::DropTarget, drop_target::DropTargetProps};
pub mod drag;
pub mod drag_bus;
pub mod drag_run;
pub mod drag_run_component;
pub mod drag_source;
pub mod drop_target;
pub use {
    drag::Axis as DragAxis, drag::DragController, drag::ListMetrics, drag::REORDER_THRESHOLD_PX,
};
mod scrollable;
pub use {scrollable::ScrollProps, scrollable::Scrollable};
mod scroll_region;
pub use scroll_region::ScrollRegion;
pub use scrollbar::BarFade;
mod overlay_scrollbar;
pub use overlay_scrollbar::OverlayScrollbar;
#[cfg(feature = "model")]
mod tile_host;
#[cfg(feature = "model")]
pub use {
    tile_host::TileContent, tile_host::TileHost, tile_host::hold_windows,
    tile_host::release_windows, tile_host::resource_unavailable_title,
};
mod app_widget;
#[cfg(feature = "model")]
#[cfg(feature = "test-support")]
pub use app_widget::BoundAppWidget;
#[cfg(feature = "model")]
#[cfg(not(feature = "test-support"))]
pub use app_widget::BoundAppWidget;

/// A component's widget as a window's managed root holds it, for the tests above the toolkit.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn bound_app_widget(
    component: crate::application::AppComponent,
) -> Box<dyn tilo_ui_base::widget::Widget> {
    Box::new(app_widget::BoundAppWidget::new(component))
}
pub use {
    app_widget::AppCommand, app_widget::AppProcessIntent, app_widget::AppWidget,
    app_widget::DevRoot, app_widget::HostShellAllow, app_widget::IconDescriptor,
    app_widget::WindowPublication,
};
pub mod input_hint;
pub mod shortcut_recorder;
pub use {
    input_hint::InputDeviceGlyph, input_hint::InputHint, input_hint::InputHintProps,
    input_hint::LegendConventions,
};
pub mod canvas_navigation_hint;
pub use {canvas_navigation_hint::CanvasNavigation, canvas_navigation_hint::CanvasNavigationHint};
pub use {shortcut_recorder::ShortcutRecorder, shortcut_recorder::ShortcutRecorderProps};
pub mod context_menu;
pub mod floating_list;
pub use {
    floating_list::FloatingList, floating_list::FloatingMetrics, floating_list::FloatingRow,
    floating_list::RowState,
};
pub mod floating_surface;
pub mod primitives;
pub use {
    context_menu::ContextMenu, context_menu::MenuButton, context_menu::MenuButtonProps,
    context_menu::MenuItem, context_menu::MenuItemProps, context_menu::MenuPick,
    context_menu::note_shift, context_menu::shift_held, context_menu::shift_skips_confirmation,
};
mod row_menu;
pub use chrome_body::paint_bar_body;
pub use row_menu::RowMenu;
pub use {
    primitives::BarChip, primitives::BarChipFace, primitives::BarChipLayout,
    primitives::BarChipProps, primitives::BarControlFace, primitives::DeclaredModal,
    primitives::DeclaredPanel, primitives::DeclaredPopover, primitives::DeclaredPopoverButton,
    primitives::DensityRegion, primitives::DisclosureGroup, primitives::DisclosureRow,
    primitives::DisclosureRowProps, primitives::DisclosureState, primitives::Fact,
    primitives::FactMark, primitives::FactProps, primitives::Facts, primitives::FactsFace,
    primitives::FactsProps, primitives::FloatingPanel, primitives::FloatingPanelFace,
    primitives::FloatingPanelLayout, primitives::FloatingPanelProps, primitives::IconButton,
    primitives::KeyHint, primitives::KeyHintProps, primitives::KeyHints, primitives::KeyHintsFace,
    primitives::KeyHintsProps, primitives::LinkKind, primitives::LinkStatus,
    primitives::ModalProps, primitives::NotificationListControl,
    primitives::NotificationListLayout, primitives::NotificationListRow,
    primitives::NotificationListView, primitives::NotificationRowControls,
    primitives::PopoverProps, primitives::StatusCluster, primitives::StatusClusterFace,
    primitives::StatusClusterLayout, primitives::StatusPart, primitives::Surface,
    primitives::SurfaceLevel, primitives::TextLineAlign, primitives::TextLineFace,
    primitives::TextStack, primitives::TextStackFace, primitives::TextStackProps,
    primitives::badge_label, primitives::badge_size, primitives::control_fill,
    primitives::control_radius, primitives::paint_control_row, primitives::paint_see_through,
    primitives::paint_severity_rail, primitives::paint_surface, primitives::paint_unread_badge,
    primitives::paint_window_card,
};
pub mod context_menu_area;
pub use {context_menu_area::ContextMenuArea, context_menu_area::MenuEntry};
pub mod send_to;
pub use {send_to::SendToBox, send_to::SendToFocus};
pub mod attached;
mod hover_toolbar;
pub use hover_toolbar::HoverToolbar;
mod canvas_place;
pub use {canvas_place::CanvasPlace, canvas_place::PLACE_BAND, canvas_place::PlaceProps};
mod swipe_row;
pub use {
    attached::Attachments, attached::ContextMenuProps, attached::EmojiBarProps,
    attached::HoverToolbarProps, attached::MenuPart, attached::MenuSeparatorProps,
    attached::SwipeProps,
};
mod chat_parts;
pub use swipe_row::SwipeRow;
pub use {
    chat_parts::EmojiPickerButton, chat_parts::EmojiPickerProps, chat_parts::Quote,
    chat_parts::QuoteProps, chat_parts::ReactionsProps,
};
mod pacer;
pub use pacer::Pacer;
pub mod data_table;
pub mod sheet;
pub(crate) mod sheet_component;
pub use sheet_component::SheetProps;
pub use {sheet::Peer, sheet::Sheet, sheet::SheetCell, sheet::SheetSize};
mod data_table_component;
pub use {
    data_table_component::DataTableCellProps, data_table_component::DataTableColumnProps,
    data_table_component::DataTableProps, data_table_component::DataTableRowProps,
    data_table_component::DescribedTable,
};
// The measures a strip of windows is laid out with.
#[cfg(feature = "model")]
mod strip_geometry;
#[cfg(feature = "model")]
pub use {
    strip_geometry::DEFAULT_STRIP_USABLE_PX, strip_geometry::HANDLE_SLOT_PX,
    strip_geometry::STRIP_BOTTOM_GUTTER_PX, strip_geometry::STRIP_EDGE_GUTTER_PX,
    strip_geometry::STRIP_TOP_GAP_PX, strip_geometry::StripSection, strip_geometry::WIDTH_FLOOR_PX,
    strip_geometry::WIDTH_MAX_PERMILLE, strip_geometry::WIDTH_MIN_PERMILLE,
    strip_geometry::WIDTH_NARROW_PERMILLE, strip_geometry::WIDTH_NORMAL_PERMILLE,
    strip_geometry::WIDTH_WIDE_PERMILLE, strip_geometry::WindowWidth, strip_geometry::max_group_px,
    strip_geometry::min_group_px, strip_geometry::permille_for_px,
};
pub mod edge_resize;
pub use edge_resize::WindowEdge;
pub mod edge_feedback;
pub use {
    edge_feedback::DWELL as EDGE_DWELL, edge_feedback::Lit as EdgeLit,
    edge_feedback::ResizeFeedback, edge_feedback::Side as EdgeSide, edge_feedback::paint_edge,
    edge_feedback::paint_size_chip, edge_feedback::size_label,
};
mod overflow_button;
pub(crate) use overflow_button::give_way;
pub use {overflow_button::OverflowButton, overflow_button::plan as plan_overflow};
mod finger_path;
pub mod tooltip;
pub use {tooltip::Tooltip, tooltip::TooltipHost};
pub mod permission_prompt;
pub mod toast;
pub use {
    permission_prompt::PROMPT_HEIGHT, permission_prompt::PermissionPrompt,
    permission_prompt::PromptChoice,
};
pub mod file_picker;
pub mod file_table;
pub use {
    file_picker::FilePicker, file_picker::FilePickerEntry, file_picker::FilePickerMode,
    file_picker::FilePickerOutcome,
};
pub mod progress_bar;
pub use {
    progress_bar::ProgressBar, progress_bar::ProgressBarFace, progress_bar::ProgressBarProps,
};
pub mod page_preview;
pub use {page_preview::PagePreview, page_preview::PagePreviewProps};
pub mod spinner;
pub use {spinner::Spinner, spinner::SpinnerProps};
pub mod date_field;
pub mod date_picker;
pub mod enum_field;
pub mod person_field;
pub use {date_field::DateField, date_field::DateFieldProps};
pub use {
    date_picker::DateFeed, date_picker::DatePicker, date_picker::DatePickerProps,
    date_picker::DateShown, date_picker::ExternalDatePickerHit,
};
pub use {enum_field::EnumField, enum_field::EnumFieldProps, enum_field::EnumOptionProps};
pub use {person_field::PersonField, person_field::PersonFieldProps};
pub mod time_field;
pub mod time_picker;
pub mod timeline_density;
pub use timeline_density::TimelineDensity;
pub use {time_field::TimeField, time_field::TimeFieldProps};
pub use {time_picker::TimePicker, time_picker::TimePickerProps, time_picker::TimeValue};
pub mod timeline;
pub mod track_timeline;
pub use {
    timeline::Ghost, timeline::MonthFace, timeline::MonthReport, timeline::MonthTarget,
    timeline::Timeline, timeline::TimelineFace, timeline::TimelineGeom, timeline::TimelineItem,
    timeline::TimelineItemProps, timeline::TimelineMode, timeline::TimelineProps,
    timeline::TimelineReport, timeline::TimelineTarget, timeline::TimelineText,
    timeline::TimelineTokens, timeline::TimelineTone,
};
pub use {
    track_timeline::TimelineMarkerProps, track_timeline::TimelineSpanProps,
    track_timeline::TimelineTrackProps, track_timeline::TrackTimeline,
    track_timeline::TrackTimelineProps,
};
pub mod color_picker;
pub mod color_swatch;
mod curve_editor;
pub use {
    color_picker::ColorPicker, color_picker::ColorPickerOutcome, color_picker::ColorPickerProps,
};
pub use {
    color_swatch::ColorSwatch, color_swatch::ColorSwatchFace, color_swatch::ColorSwatchPicker,
    color_swatch::ColorSwatchPickerProps, color_swatch::SwatchPainter,
};
pub use {curve_editor::CurveEditor, curve_editor::CurveEditorProps};
pub mod picture;
pub mod picture_choice;
pub use {
    picture_choice::ColourFilter, picture_choice::Picture, picture_choice::PictureChoice,
    picture_choice::PictureChoiceProps, picture_choice::PictureKind, picture_choice::PictureOption,
    picture_choice::PictureOptionProps, picture_choice::TouchpadGesture,
};
pub mod radio_group;
pub use {
    radio_group::RadioGroup, radio_group::RadioGroupProps, radio_group::RadioOption,
    radio_group::RadioProps,
};
pub mod shown;
pub use {shown::InState, shown::InStateProps};
pub mod popover;
pub use {
    popover::PanelAlign, popover::Popover, popover::PopoverButton, popover::PopoverPrompt,
    popover::PopoverSide,
};
pub mod emoji_picker;
pub use {emoji_picker::EmojiPicker, emoji_picker::PickerEvent};
mod emoji_pane_component;
pub use {emoji_pane_component::EmojiPane, emoji_pane_component::EmojiPaneProps};
pub mod reaction_strip;
pub use {
    reaction_strip::ReactionStrip, reaction_strip::ReactionStripWidget,
    reaction_strip::ReactionToggle, reaction_strip::StripHit,
};
pub mod modal_dialog;
pub use {
    modal_dialog::ModalAction, modal_dialog::ModalDialog, modal_dialog::ModalEmphasis,
    modal_dialog::ModalHit, modal_dialog::ModalLayout, modal_dialog::ModalSheet,
    modal_dialog::ModalSpec, modal_dialog::SheetResponse,
};
pub mod secret_prompt;
pub use {
    secret_prompt::FieldRole, secret_prompt::FieldSpec, secret_prompt::NoticeTone,
    secret_prompt::Presentation, secret_prompt::PromptOutcome, secret_prompt::SecretAnswer,
    secret_prompt::SecretKind, secret_prompt::SecretPrompt,
};
pub mod toolbar;
pub use {
    toolbar::Toolbar, toolbar::ToolbarButtonProps, toolbar::ToolbarPart, toolbar::ToolbarProps,
    toolbar::ToolbarSeparatorProps, toolbar::ToolbarSpacerProps,
};
pub mod undo_button;
pub use undo_button::UndoButton;
pub mod breadcrumb;
pub use {breadcrumb::Breadcrumb, breadcrumb::BreadcrumbProps, breadcrumb::BreadcrumbSegmentProps};
pub mod avatar;
pub use {avatar::Avatar, avatar::AvatarProps};
pub mod badge;
pub use {badge::Badge, badge::BadgeContent, badge::BadgeFace, badge::BadgeProps};
pub mod icon;
pub use {icon::IconProps, icon::IconSize, icon::IconWidget};
pub mod signal_meter;
pub use {
    signal_meter::SIGNAL_BARS, signal_meter::SignalLevel, signal_meter::SignalMeter,
    signal_meter::SignalMeterFace, signal_meter::SignalMeterProps,
};
pub mod stroke_preview;
pub use {
    stroke_preview::BrushCharacter, stroke_preview::StrokePreview,
    stroke_preview::StrokePreviewFace, stroke_preview::StrokePreviewProps,
};
pub mod level_meter;
pub use {
    level_meter::LevelMeter, level_meter::LevelMeterFace, level_meter::LevelMeterProps,
    level_meter::MeterState,
};
pub mod drawing;
pub use {
    drawing::Drawing, drawing::DrawingProps, drawing::Ink, drawing::Meaning, drawing::Shape,
    drawing::Weight,
};
pub mod hex_grid;
pub use {hex_grid::HexColumn, hex_grid::HexGrid, hex_grid::HexGridProps};
pub mod key_pad;
pub use {
    key_pad::KeyPad, key_pad::KeyPadProps, key_pad::PadKey, key_pad::PadSize, key_pad::PadTone,
};
pub mod diff_grid;
pub use {diff_grid::DiffGrid, diff_grid::DiffGridProps, diff_grid::DiffSide};
pub mod paint_surface;
pub use {paint_surface::PaintSurface, paint_surface::PaintSurfaceProps};
pub mod gauge;
pub use {
    gauge::BarGauge, gauge::BarGaugeProps, gauge::GaugeReading, gauge::GaugeStyle,
    gauge::GaugeThresholds, gauge::ProgressRing, gauge::ProgressRingFace, gauge::ProgressRingProps,
    gauge::ProgressRingSize, gauge::RingReading,
};
pub mod split_handle;
pub use {split_handle::SplitHandle, split_handle::SplitOrientation, split_handle::SplitRatio};
pub mod video_player;
pub use {
    video_player::DeclaredVideo, video_player::PLAYBACK_RATES, video_player::VideoPlayer,
    video_player::VideoPlayerProps,
};
pub mod terminal;
pub use {terminal::GridEvents, terminal::Terminal, terminal::TerminalProps};
#[cfg(feature = "webview")]
pub mod webview;
#[cfg(feature = "webview")]
pub use {webview::WebView, webview::WebViewProps};
pub mod image_viewer;
mod image_viewer_component;
pub use image_viewer::ImageViewer;
pub use {image_viewer_component::ImageViewerProps, image_viewer_component::ZoomableImage};
mod slideshow_component;
pub use {slideshow_component::Slideshow, slideshow_component::SlideshowProps};
mod tree_component;
pub use {
    tree_component::DescribedTreeView, tree_component::RemovedNodes,
    tree_component::TREE_ROW_DOUBLE_HEIGHT, tree_component::TREE_ROW_HEIGHT,
    tree_component::TreeTone, tree_component::TreeViewNodeProps, tree_component::TreeViewProps,
    tree_component::TreeViewRowProps,
};
mod row_selection;
mod rows;
pub mod virtual_list;
pub use {
    virtual_list::AnyModel, virtual_list::DescribedList, virtual_list::DescribedRows,
    virtual_list::DuplicateListKey, virtual_list::ItemRow, virtual_list::ListEdges,
    virtual_list::ListItemProps, virtual_list::ListKey, virtual_list::ListModel,
    virtual_list::PaintedRowLifts, virtual_list::RowActionRects, virtual_list::RowBuild,
    virtual_list::RowContentBox, virtual_list::RowGroup, virtual_list::RowLift,
    virtual_list::RowRenameField, virtual_list::RowSelection, virtual_list::RowSelectionSink,
    virtual_list::TREE_INDENT, virtual_list::TreeMarks, virtual_list::TreeMove,
    virtual_list::TreeRow, virtual_list::VecListModel, virtual_list::VirtualList,
    virtual_list::VirtualListProps, virtual_list::VirtualRows, virtual_list::row_content_box,
};
/// What makes a composite one pressable target: its press, its bound action and the controls it
/// publishes, for a widget above the toolkit as for the toolkit's own rows and cards.
pub mod activation;
mod dismissal;
#[cfg(feature = "model")]
#[cfg(feature = "model")]
pub use activation::take_presses;
pub use {activation::Interaction, activation::paint_disabled, activation::paint_focus_ring};
pub mod media_card;
mod media_grid_component;
pub use {
    media_card::ArtPainter, media_card::MediaArt, media_card::MediaBadge, media_card::MediaCard,
    media_card::MediaCardFace, media_card::MediaGrid, media_card::MediaSize,
    media_card::MediaSource, media_card::OwnedBadge, media_card::justified_rows,
};
pub use {
    media_grid_component::DescribedCards, media_grid_component::MediaCardProps,
    media_grid_component::MediaGridProps,
};
pub mod capture_button;
pub use {capture_button::CaptureButtonFace, capture_button::CaptureKind};
pub mod call_tile;
pub use {
    call_tile::CallSource, call_tile::CallTile, call_tile::CallTileProps, call_tile::call_address,
    call_tile::call_slot,
};
pub mod transport;
pub use {transport::TransportControl, transport::TransportFace};
pub mod media_transport;
mod media_transport_component;
pub use {
    media_transport::MediaTransport, media_transport::TransportActions,
    media_transport::TransportArt, media_transport::TransportEvent, media_transport::TransportFeed,
    media_transport::TransportRepeat, media_transport::TransportState,
    media_transport::TransportTriggers,
};
pub use {
    media_transport_component::DeclaredTransport, media_transport_component::MediaTransportProps,
};
pub mod now_playing;
pub use {now_playing::Scrub, now_playing::Song, now_playing::SongFace};
pub mod drop_zone;
pub use {drop_zone::DropZone, drop_zone::DropZoneFace, drop_zone::DropZoneProps};
pub mod file_open;
pub use {file_open::FileOpenEvent, file_open::FileOpenSurface};
pub mod skeleton;
pub use {
    skeleton::Skeleton, skeleton::SkeletonFace, skeleton::SkeletonForm, skeleton::SkeletonProps,
    skeleton::skeleton_tone,
};
pub mod backdrop;
pub use {backdrop::Backdrop, backdrop::BackdropProps, backdrop::Ground as BackdropGround};
pub mod arrival_failure;
pub use {arrival_failure::WindowFailure, arrival_failure::arrival_failure};
pub mod arriving_rows;
pub mod arriving_view;
pub use {arriving_rows::ArrivingRows, arriving_rows::RowFade};
pub use {
    arriving_view::ArrivalFace, arriving_view::ArrivingView, arriving_view::ArrivingViewOf,
    arriving_view::AwaitsContent, arriving_view::ReservedShape, arriving_view::Showing,
};
pub mod swapped;
pub use swapped::Swapped;
pub mod stepper;
pub use {stepper::Stepper, stepper::StepperFace, stepper::StepperProps};
pub mod guided_page;
pub use {guided_page::GuidedPage, guided_page::GuidedPageProps};
pub mod property_list;
pub use {
    property_list::Property, property_list::PropertyColumn, property_list::PropertyList,
    property_list::PropertyListFace, property_list::PropertyListProps,
    property_list::PropertyProps, property_list::PropertyRow, property_list::PropertyValue,
    property_list::property_label_column,
};
pub mod list_row;
pub use {
    list_row::CARD_TITLE_LINES, list_row::CardBody, list_row::ControlRow,
    list_row::ControlRowProps, list_row::ListRow, list_row::ListRowFace, list_row::ListRowProps,
    list_row::RowAction, list_row::RowAuthor, list_row::RowLeading, list_row::RowShapeFace,
    list_row::RowStatus, list_row::RowValue, list_row::RowWriter, list_row::ShownChange,
    list_row::SwipeAction, list_row::SwipeActionProps, list_row::badge_column_width,
    list_row::mark_text_inset,
};
pub mod toggle_group;
pub use {
    toggle_group::Segment, toggle_group::SegmentProps, toggle_group::SegmentedFace,
    toggle_group::SegmentedStyle, toggle_group::SegmentedTouch, toggle_group::SegmentedTouchGroup,
    toggle_group::SegmentedTouchResult, toggle_group::ToggleGroup, toggle_group::ToggleGroupProps,
    toggle_group::ViewTabsProps, toggle_group::segment_colors,
};
pub mod view_tabs;
pub use view_tabs::ViewTabs;
pub mod chrome_body;
pub use {
    chrome_body::ChromeBody, chrome_body::ChromeEdge, chrome_body::ChromeProps,
    chrome_body::ChromeShape,
};
pub mod magnify;
pub use {magnify::Magnify, magnify::MagnifyProps};
pub mod docked;
pub use {docked::Docked, docked::DockedProps};
pub mod map;
pub use {
    map::MAP_ZOOM_MAX, map::MAP_ZOOM_MIN, map::Map, map::MapCamera, map::MapMarkerKind,
    map::MapMarkerProps, map::MapPinProps, map::MapProps, map::MapTrackProps, map::MapTrackTone,
    map::PinPlace,
};
pub mod window_well;
pub use {window_well::Wells, window_well::WindowWell, window_well::WindowWellProps};
pub mod window_preview;
pub use window_preview::{Previews, WindowPreview, WindowPreviewProps};
pub mod peer_pointers;
pub use peer_pointers::{PeerPointerProps, PeerPointers, PeerPointersProps};
pub mod follow_frame;
pub use follow_frame::{FollowFrame, FollowFrameProps};
pub mod screen_mirror;
pub use screen_mirror::{ScreenMirror, ScreenMirrorProps, ScreenMirrorWindowProps};
pub mod window_frame;
pub use {window_frame::WindowFrame, window_frame::WindowFrameProps};
pub mod empty_state;
pub use {
    empty_state::EmptyState, empty_state::EmptyStateContent, empty_state::EmptyStateLayout,
    empty_state::EmptyStateProps, empty_state::PaintedStack,
};
pub mod find_bar;
pub use {
    find_bar::FindAbilities, find_bar::FindBar, find_bar::FindMatch, find_bar::FindMode,
    find_bar::FindOptions, find_bar::FindScope, find_bar::FindSession, find_bar::Findable,
    find_bar::OwnFind, find_bar::paint_find_match, find_bar::with_findable,
};
pub mod find_controls;
pub use {find_controls::FindControls, find_controls::FindControlsProps};
pub mod search_field;
pub use {
    search_field::FilterCount, search_field::Filterable, search_field::Filtered,
    search_field::SearchField, search_field::apply_filter, search_field::sync_filter,
    search_field::with_filterable,
};
pub mod window_search;
pub use {
    window_search::InPlace, window_search::QueryView, window_search::Queryable,
    window_search::SearchDriver, window_search::SearchHandoff, window_search::SearchReach,
    window_search::SearchableWindow, window_search::WindowSearch,
    window_search::WindowSearchRequest, window_search::with_queryable,
};
pub mod search_results;
pub use search_results::SearchResultsView;
pub mod banner;
pub use {
    banner::Banner, banner::BannerContent, banner::BannerHit, banner::BannerKind,
    banner::BannerNotice, banner::BannerProps,
};
mod disclosure_band;
pub use {disclosure_band::DisclosureBand, disclosure_band::DisclosureBandProps};
pub mod chip;
pub use {
    chip::Chip, chip::ChipColors, chip::ChipFace, chip::ChipLead, chip::ChipProps, chip::ChipStyle,
    chip::REFERENCE_LEAD,
};
pub mod change_mark;
pub use {change_mark::ChangeMark, change_mark::ChangeMarkProps};
pub mod conversation;
pub use conversation::*;
pub mod card;
pub use {
    card::Actions, card::Card, card::CardFace, card::CardGrid, card::CardGridCardProps,
    card::CardGridProps, card::CardProps, card::CardWidth, card::DescribedCardGrid,
    card::card_column_width, card::card_columns,
};
mod section;
pub use {
    section::ObjectHeader, section::ObjectHeaderProps, section::Section, section::SectionHeader,
    section::SectionHeaderFace, section::SectionProps, section::StatusLine,
    section::StatusLineProps, section::band_height, section::heading_count,
};
pub mod capture_studio;
pub use {
    capture_studio::AudioCaptureMode, capture_studio::CaptureDestination,
    capture_studio::CaptureIntent, capture_studio::CaptureMode, capture_studio::CapturePreview,
    capture_studio::CaptureStudio, capture_studio::CaptureTarget, capture_studio::LiveCapture,
    capture_studio::PipLayout, capture_studio::RecordingCommand, capture_studio::RegionAnchor,
    capture_studio::ReplayCommand, capture_studio::ScreenshotCommand,
    capture_studio::ScreenshotKept, capture_studio::ScreenshotSendDevice,
    capture_studio::ScreenshotSendTarget, capture_studio::SourceMode,
};
pub mod screenshot_markup;
pub mod selection_handles;
pub use {
    screenshot_markup::MarkupExport, screenshot_markup::MarkupStroke,
    screenshot_markup::MarkupTool, screenshot_markup::ScreenshotMarkup,
    screenshot_markup::ScreenshotMarkupCommand,
};
pub use {
    selection_handles::Handle as SelectionHandle, selection_handles::SelectionHandles,
    selection_handles::TextGrip, selection_handles::TextGrips,
};
// History's views: the timeline of a resource's changes, read from its history.
#[cfg(feature = "model")]
pub mod activity_timeline;
#[cfg(feature = "model")]
mod activity_timeline_component;
#[cfg(feature = "model")]
pub mod history;
#[cfg(feature = "model")]
pub mod history_face;
#[cfg(feature = "model")]
pub mod history_group;
pub mod name_field;
#[cfg(feature = "model")]
pub use {activity_timeline::ActivityTimeline, activity_timeline::RowParts};
#[cfg(feature = "model")]
pub use {
    activity_timeline_component::ActivityArtifact, activity_timeline_component::ActivityEntryProps,
    activity_timeline_component::ActivityTimelineProps,
    activity_timeline_component::DescribedActivityTimeline,
};
#[cfg(feature = "model")]
pub use {
    history::AuthorRelation, history::EntryKind, history::EntryTarget, history::HistoryAttribution,
    history::HistoryAuthor, history::HistoryClassification, history::HistoryEntry,
    history::HistoryFilter, history::RedactionDisplay, history::format_relative_time,
    history::hue_to_rgb,
};
#[cfg(feature = "model")]
pub use {history_face::EntryBody, history_face::EntryFace, history_face::HistoryEntryPresenter};
pub mod qr;
mod qr_component;
pub mod resource_actions;
pub use qr::QrWidget;
pub use qr_component::QrProps;
#[cfg(test)]
mod a11y_tree_tests;
#[cfg(any(test, feature = "test-support"))]
pub mod effect_fixture;
pub mod keyboard_map;
pub use {
    keyboard_map::KeyboardMap, keyboard_map::KeyboardMapInput, keyboard_map::KeyboardMapKey,
    keyboard_map::KeyboardMapSelection,
};
/// Logical screen topology canvas shared by local and native peer displays.
mod graph_canvas;
pub use {
    graph_canvas::Connection, graph_canvas::GraphCanvas, graph_canvas::GraphCanvasConnection,
    graph_canvas::GraphCanvasNode, graph_canvas::GraphCanvasProps, graph_canvas::GraphNodeProps,
    graph_canvas::connection_description,
};

// The helpers a widget's own tests share with the app runner's tests.
#[cfg(any(test, feature = "test-support"))]
pub use context_menu_area::test_support as context_menu_area_test_support;

/// The widget modules' own surfaces: what a widget's tests saw as `super::*` while they lived
/// inside it. The tests that drive the app runner live in `tilo-ui-app` now, and reach the same
/// items here; nothing outside a test builds against this.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod internals {
    pub mod media_transport {
        pub use crate::widget::media_transport::metrics_local::{Form, Metrics, layout_for_test};
        pub use crate::widget::media_transport::*;
    }
    pub mod primitives {
        pub use crate::widget::primitives::surfaces_component::*;
        pub use crate::widget::primitives::*;
    }
    pub mod scroll_region {
        pub use crate::widget::scroll_region::*;
    }
    pub mod canvas_place {
        pub use crate::widget::canvas_place::*;
    }
    pub mod map {
        pub use crate::widget::map::*;
    }
    pub mod modal_dialog {
        pub use crate::widget::modal_dialog::*;
    }
    pub mod nav_list {
        pub use crate::widget::nav_list::model::{NavItemSpec, NavRowKind};
        pub use crate::widget::nav_list::row::NavPaint;
        pub use crate::widget::nav_list::*;
        pub mod model {
            pub use crate::widget::nav_list::model::*;
        }
    }
    pub mod nav_shell {
        pub use crate::widget::nav_shell::sidebar_resize::AdaptiveSidebar;
        pub use crate::widget::nav_shell::*;
        pub use crate::widget::toast::ToastKind;
        #[cfg(feature = "test-support")]
        pub use crate::widget::window_header::header_row::{test_measure_title, test_title_style};
    }
    pub mod page_sidebar {
        pub use crate::widget::page_sidebar::*;
    }
    pub mod page_slide {
        pub use crate::widget::page_slide::*;
    }
    pub mod property_list {
        pub use crate::widget::property_list::*;
    }
    pub mod screenshot_markup {
        pub use crate::widget::screenshot_markup::*;
    }
    pub mod scrollable {
        pub use crate::widget::scrollable::*;
    }
    pub mod section {
        pub use crate::widget::section::*;
    }
    pub mod shortcut_recorder {
        pub use crate::widget::shortcut_recorder::*;
    }
    pub mod slider {
        pub use crate::widget::slider::*;
    }
    pub mod stage {
        pub use crate::widget::stage::*;
    }
    pub mod text_input {
        pub use crate::widget::text_input::*;
    }
    pub mod timeline {
        pub use crate::widget::timeline::*;
    }
    pub mod toggle {
        pub use crate::widget::toggle::*;
    }
    pub mod toggle_group {
        pub use crate::widget::toggle_group::*;
    }
    pub mod virtual_list {
        pub use crate::widget::virtual_list::*;
    }
    pub mod chip {
        pub use crate::widget::chip::*;
    }
    pub mod virtual_list_component {
        pub use crate::widget::virtual_list::component::*;
    }
    pub mod text {
        pub use crate::widget::text::assert_glyph_run_centred_in_band;
    }
    pub mod tree_component {
        pub use crate::widget::tree_component::*;
    }
}
/// The fixtures and measures a test outside this crate reaches.
#[cfg(any(test, feature = "test-support"))]
pub use {
    board::card_estimate_for_tests,
    dropdown::{chevron_width_for_tests, menu_min_width_for_tests, pad_x_for_tests},
    image::fixture_image_path_for_tests,
};
