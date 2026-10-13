# Browsing an app's managed History

Managed History offers Resource, Person or agent, and Task group pickers. Choices
come from the action record's published resource and group labels and attributed
author names. Unclassified changes remain visible with All selected; they do not
match a resource or task-group lens. The existing timeline's action-kind and
name filters combine with these lenses. Clear filters resets all lenses.

A lens filters **entries**, not document ownership. Selecting a matching change
previews the document captured by the registered source. It never opens another
resource's document merely because an entry mentions that resource. Changing a
lens clears the selected version and any restore review. Scope controls continue
to select declared sources, including checked file history; they are not resource
filters.

The local canonical `tilo.host:history` resource exposes `filter` with optional
`resource`, `actor`, and `group` text parameters. An empty argument object clears
these lenses. Resource and group parameters accept the same labels or stable
addresses as the shared HistoryFilter model. This is local presentation state,
not an edit or a new remote permission bypass.

Selecting changes opens Compare pinned versions with a changed-action summary.
Before and After switch between independently retained, read-only app views at
the exact causal frontiers. This is not a pixel-diff renderer or a side-by-side
view. Live updates do not move the selected versions. The comparison describes
the owning document, not historical pixels for every resource mentioned by an
action. Primitive/model adapters and media can only display the historical data
they support; missing historical media remains unavailable. Restore still uses
the existing reviewed-frontier, owner and permission checks.
