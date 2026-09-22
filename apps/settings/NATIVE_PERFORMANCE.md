# Native Settings diagnostics

Use the normal `settings` entrypoint with isolated data, configuration, and cache
roots. The observer must retain production hardware/backend routing; screenshot
hardware or category overrides are not part of this workload.

The proposed search workload clicks the actual `settings.search` bounds during
warmup, then alternates a character and Backspace. Every measured input must
change the search text and its actual retained sidebar results. Setup, category
navigation, scrolling, and resize require separate cases; this search case does
not qualify the whole application.

The root observer distinguishes local search state changed immediately by input
from `synchronized_search_query` consumed by the next sync. It reads the real
search widget's text, caret byte, focus, and logical bounds. Rendered category
IDs come from actual retained `SidebarRow` widgets, not a second query engine.
The `rendered_category_ids` name denotes the retained result rows, including
rows outside the viewport; it does not assert visible pixels. Root bounds,
selected category, sidebar visibility, and narrow mode describe the
actual application shell. Physical pointer positions must be divided by the
native window's recorded scale factor before comparing logical bounds.

`effective_document_heads` are computed on a disposable document clone, including
pending operations. They are not the original document's committed heads. The
observer must not commit or otherwise mutate the original. Only change hashes,
root identity, and original pending-operation count are exposed; settings values
are not logged. This is opt-in diagnostic work whose cost needs attribution in
native runs, not a claim of free observation.

Tests exercise the production Settings widget event and structural rebuild path
using the existing search-focus harness pattern. They do not provide native GPU,
presentation, frame cadence, or all-app qualification evidence.

## Search pane retention regression

The first real-input observer test found that filtering reconstructed the active
Network pane. Its fresh synchronization seeded document bindings again, changing
actual document heads despite search being local state. Query changes now replace
only sidebar navigation; category selection still replaces the active content.
The strict character/Backspace test verifies focus, caret, actual result rows,
and unchanged document heads across filtering. A separate test verifies that the
observer's disposable-clone witness leaves the original pending transaction and
original document debug representation untouched.

The scoped Settings app library suite passed 23 tests after this fix. Native
frame and presentation measurements have not yet been collected for Settings.
Existing integration tests also passed: 3 search-focus cases and 19 category
navigation cases, including consecutive category changes.

Additional retention regressions verify the production Network pane's identity
survives filtering and category selection replaces it. A real local TextInput
content probe retains edited text, caret state, and widget identity across the
same query path. Narrow-mode coverage reaches the list through the actual Back
control and preserves search focus through `z`/Backspace. The expanded app-local
suite passed 26 tests. Its harness now drains initial responsive rebuilds and
routes action-bound control intents like the live host; without those steps the
old wide-search-only harness could not exercise Back correctly.
