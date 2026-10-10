# Emoji picker interrupted selection

Production base fa40ba0eb4203f952db011a9da28b7b3c094cab6 reproduces two
unexpected insertions with production sources unchanged: release after focus
loss inserts pizza; release after correcting pizza search to thumbs up inserts
the replacement result. The picker remembers the pressed cell's index across
focus and result changes.

The repair cancels the existing domain gesture when focus is lost, grid content
or layout changes, or the grid scrolls. The existing long-press skin-tone flow
remains. SearchField and ToggleGroup continue to provide the shared search and
segmented faces; no new control, ink, material or geometry is introduced.

The production pointer and keyboard event paths are exercised before and after,
including cancelled releases, fresh explicit picks, Escape/reopen, resize,
scrolling and long press. Native shell tests use the existing headless overlay
host with no real client field, account or device effects.

The capture example paints the production picker after actual WindowEvents in
dark/light, enlarged text, high contrast, combined UI/text scaling and a 320 px
host. Before captures visibly close after both interrupted picks; after captures
retain the intended search. Idle images stay unchanged. Public inspected images
and exact-head qualification are attached to the ready PR. Installed-device
acceptance is separate.

Screenshot inspection also found preexisting footer/hint overlap at 200% text
(the skin-tone hint overlaps even at 800 px), plus group-icon elision at 320 px. That distinct measured-layout follow-up remains
in the desktop adoption inventory; it is not repaired by gesture cancellation.
