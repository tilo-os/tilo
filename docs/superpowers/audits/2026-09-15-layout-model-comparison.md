# QoS layout compared with React and SwiftUI

Reviewed 2026-09-15. “Swift” here means SwiftUI, Apple's UI framework; Swift itself is a language. React comparison below is React for the web.

## What actually computes the layout?

| System | Application describes | Geometry is computed by | Custom composition |
| --- | --- | --- | --- |
| QoS | Retained Rust widgets and their composition | Taffy, measured with the shared TextRenderer; Vello draws the result | Existing Row/Column/Grid and Widget APIs; ManualContext measures retained children and Manual places them; ManualInvalidation supports retained input changes |
| React web | A component tree that renders DOM elements | Browser layout from HTML/CSS | CSS layouts and custom components; React itself is not the browser layout engine |
| SwiftUI | Views, layout containers and modifiers | SwiftUI's layout system | Built-in stacks/grids or a Layout implementation using subview measurement/placement proxies |

React calls components, then commits the necessary DOM changes; the browser subsequently paints. That separation matters: choosing React alone does not select a pleasant spacing, width, or grouping policy. See [React's render and commit explanation](https://react.dev/learn/render-and-commit).

SwiftUI's Layout protocol separates reporting the composite size (`sizeThatFits`) from assigning child positions (`placeSubviews`). Subview proxies allow measurement and placement without exposing concrete views. This is a useful model for QoS's manual authoring surface, rather than requiring app authors to manipulate Taffy node maps. See [Apple's Layout documentation](https://developer.apple.com/documentation/swiftui/layout).

## The QoS lifecycle

A widget builds its retained children into the host Taffy tree. The shared solver measures text/controls and settles allocation-dependent styles. The host applies final rectangles to the widgets, then paints. Input, focus and accessibility consume those same retained widgets. On viewport changes the shared lifecycle also settles geometry-dependent composition before paint.

Relevant implementation: [shared solver](/Users/xaver/dev/qos-macos-shell/ui/src/widget/layout.rs), [Column](/Users/xaver/dev/qos-macos-shell/ui/src/widget/column.rs), [headless host](/Users/xaver/dev/qos-macos-shell/ui/src/headless.rs). Framework integration matters: the Store's former CardGrid created geometry while painting in a private tree, so its buttons were missing from normal host focus traversal even though they looked clickable.

## How to make pleasant layout require less work

Reliability comes first: a requested gap must work, resize must update composition, and measured text must match drawn text. Above those contracts, reusable patterns can own decisions that developers otherwise repeat: page gutters, readable width, shared label columns, field/section spacing, narrow stacking, and action arrangement.

The implemented automatic FormPage/Form/Section/Field path needs content and grouping, with no caller layout pixels or responsive branch in the pilot. Guided overrides let an author change a supported property and reset it independently. The required manual path keeps all standard controls and allows authored geometry. These are QoS design choices; neither React nor SwiftUI guarantees a good information hierarchy merely by being used.

The framework can enforce concrete contracts such as geometry consistency. It can supply reviewed defaults for composition. It cannot guarantee that arbitrary application content, ordering or fixed dimensions produce a pleasant screen. Human review remains necessary, and the current source-decision inventory is not a measured developer-productivity result.

See the [original Store findings](/Users/xaver/dev/qos-macos-shell/docs/superpowers/audits/2026-09-15-store-layout-framework-review.md), [architecture proposal](/Users/xaver/dev/qos-macos-shell/docs/superpowers/specs/2026-09-15-ui-quality-design.md), and [implementation progress](/Users/xaver/dev/qos-macos-shell/docs/superpowers/audits/2026-09-15-framework-layout-progress.md).
