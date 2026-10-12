# Automatic, guided, and manual composition

Use `FormPage`, `Form`, `Section`, `Field`, and `Actions` for ordinary forms. They
supply readable width, gutters, spacing, label alignment, wrapping, scrolling,
and action arrangement. See [Form composition](FORM_COMPOSITION.md) for the
content-first API and independent guided overrides.

Every region can instead use existing Row/Column/Grid builders or `Manual` to
measure ordinary children and choose their logical rectangles. Manual layout
needs no exception approval. Standard controls retain their rendering, focus,
keyboard interaction, bindings, hit testing, and accessibility.

## Measure, then place

```rust
use taffy::Size;
use vello::kurbo::Rect;
use ui::widget::{Manual, MeasureConstraint as M, Text, TextInput};

let region = Manual::new(|ctx| {
    let width = (ctx.available().width - 32.0).max(0.0);
    let label = ctx.measure(0, Size {
        width: M::Exact(width), height: M::Intrinsic,
    })?.size;
    ctx.place(0, Rect::new(16.0, 16.0,
        f64::from(width) + 16.0, f64::from(label.height) + 16.0))?;
    let y = f64::from(label.height) + 24.0;
    ctx.place(1, Rect::new(16.0, y, f64::from(width) + 16.0, y + 40.0))?;
    Ok(Size { width: ctx.available().width, height: y as f32 + 56.0 })
})
.child(Box::new(Text::new("Name")))
.child(Box::new(TextInput::local().with_id("name")
    .with_accessible_name("Name")));
```

The closure returns the desired region size, places every child once, and may
measure a child multiple times. Indices follow `.child()` declaration order;
that order also remains the keyboard/accessibility traversal order. The closure
may run several times before paint. Keep it deterministic for its inputs and
keep topology unchanged. `available()` is the current allocated size, so an
explicit responsive rule can use its width. Avoid deriving a desired height
from its own previous height unless that rule converges.

`measure(index, proposal)` solves the child's **current retained subtree** with
the actual text renderer. It works for leaf controls, Row/Column, automatic
Forms, and nested manual regions. Each axis independently accepts:

| Proposal | Meaning |
| --- | --- |
| `Intrinsic` | Natural size; root fill/percentage sizing becomes intrinsic. Fixed dimensions remain authored. |
| `AtMost(n)` | Fit within a nonnegative logical bound, allowing a smaller natural size. |
| `Exact(n)` | Allocate that logical size. |

A measurement reports `size` and `content_size` separately. Content can exceed
its allocation. Probes use cloned Taffy topology and the shared renderer-aware
policy settlement. They do not apply geometry, replace widgets, or rewrite
retained node identities. They may refresh measurement caches and policy
metadata; the final host pass restores decisions for the actual allocation.
Hosts that rebuild topology from previous geometry (`NavShell` showing or
hiding a column) settle that topology after actual placement. A probe does not
predict every hypothetical topology of such a host.

## Changing retained policy inputs

Keep a clone of `Manual::invalidation()` when a policy captures mutable input:

```rust
use std::{cell::Cell, rc::Rc};
use ui::widget::Manual;
let offset = Rc::new(Cell::new(0.25_f64));
let policy_offset = offset.clone();
let region = Manual::new(move |ctx| {
    // Measure/place retained children using policy_offset.get().
    let _x = policy_offset.get();
    Ok(taffy::Size { width: ctx.available().width, height: 100.0 })
});
let invalidation = region.invalidation();
// Later, in the owning widget's input/update handling:
offset.set(5.75);
invalidation.invalidate();
// Return true from the owning Widget::update to request a normal App layout
// frame, or use the custom host's ordinary layout-frame scheduling mechanism.
```

The handle marks policy work; it does not wake a window or observe arbitrary
`Rc<Cell<_>>` changes automatically. Both invalidation and normal frame scheduling
are required. A same-viewport layout frame then solves and places the existing
children, preserving their identities, entry text, selection and focus. No
structural rebuild is required. Changes to viewport/child measurement continue
to trigger their existing layout paths.

Only actual placement acknowledges the revision successfully solved. Hypothetical
measurement probes, failed solves, and a newer invalidation during policy
evaluation do not clear pending work. Once applied, unchanged paint or scroll
frames reuse solved geometry without evaluating the policy again.

## Exact geometry and conflicts

Rectangles are local to the Manual region. Coordinates use Taffy's `f32`
logical precision; quarter-pixel values remain quarter-pixel values. When an
admitted Manual region is present, the shared solver disables Taffy's global
pixel rounding for that host tree, including automatic siblings. Physical-pixel
rasterization still happens during drawing. Trees without Manual keep their
existing rounding setting. App and FrameHarness replace their Taffy tree on a
rebuild, so removing the last Manual restores their normal default. A custom
host that reuses its Taffy instance owns resetting that configuration.

Placement supplies absolute insets, removes child margins, and sets equal
minimum/maximum dimensions. A fit-content Button can still measure its label;
its own sizing policy cannot replace the authored allocation. Padding and
borders can make an extremely small allocation impossible. Inspection reports
the first conflicting child index and its requested/effective rectangles rather
than pretending the request was honored. Nonfinite/negative dimensions,
invalid indices, duplicate placement, and omitted children are errors. Style
settlement is limited to eight passes; nested measurement is limited to32
levels. Failed layout must not be applied or painted.

Row/Column report exact absolute allocations independently of overflowing
content; their drawing culling retains the larger content extent. Intentional
overflow is supported. Authors decide whether to scroll, clip, use a minimum
window size, or reflow below their supported width. A fixed rectangle is not a
quality defect by itself.

## Mixing policies and preserving state

- Put `Manual` in `Section::content` to make one authored region in an automatic
  page. Other fields keep their own automatic/guided policies.
- Put an intrinsic `Form` in a Manual child, measure it at the chosen width,
  then place it. The Form still measures its labels and applies local overrides.
- Retain the same controls and change a closure input using the invalidation
  handle above, or change a region override. Retained placement and the host
  rebuild/resize lifecycle preserve entry text, selection and focus.
  Saved focus restoration requires a semantic key that is **unique across the
  admitted tree**. Do not give support labels the input's key. Anonymous or
  ambiguous fresh controls deliberately do not inherit saved state.

App and FrameHarness already use `compute_widget_layout`. Authors of a custom
host can keep using that existing unsafe host API with its exact live-tree maps;
ordinary Manual authors never manage node pointers or admission maps. Policies
are extracted only from admitted widgets and execute inside the owner's existing
component lease, including all probes. Closed and cross-record owners are not
invoked. Probes retain original admission identities for their descendant maps.

## Existing preview and inspection

The existing `FrameHarness::inspection()` / `inspection_json()` and tilo-dev
inspection flow add an optional `layout` property from `Widget::layout_info()`.
Ordinary widgets without metadata omit it. Page/Form/Section/Field show actual
resolved values and property origins (`Theme`, `Pattern`, or nearest authored
region). Field metadata includes its measured required/available widths and
resolved label position. `Manual` is an explicit mode and reports allocation,
requested bounds, content bounds, overflow, and its first unsatisfied constraint.
Content bounds include solved descendant origins, including negative coordinates,
and respect per-axis Taffy overflow clipping. They are conservative layout/paint
extents where custom drawing or clipping is not represented in Taffy; they are
not a universal pixel-level paint diagnostic.
The normal child rectangles remain the effective alignment edges. Existing
`child_clip` describes actual clipping where available.

The runtime does not know arbitrary application source locations or every
builder value, so inspection does not invent them. Form properties with authored
origins identify guided choices; an ordinary Row/Column without metadata is not
misclassified as an automatic policy.

## Complete examples and evidence

```sh
TILO_UI_VELLO_CPU=1 cargo run -p ui-examples --example form-composition-shots -- /tmp/forms
TILO_UI_VELLO_CPU=1 cargo run -p ui-examples --example manual-composition-shots -- /tmp/manual
```

The manual example recreates the same Account form with individual title,
section labels, field labels, standard inputs, help, validation, checkbox, and
primary/secondary Buttons. It authors section spacing, label alignment, support
copy flow, width rules, and individual action placement. It does not place an
automatic Form/Actions and call that fully manual. Separate automatic-in-manual
and manual-in-automatic fixtures demonstrate mixed nesting. Captures include
wide/narrow and200% text plus scroll-end views. The source makes author effort
visible; it is not a measured development-time study.

`tilo-dev new --template starter` uses automatic composition and keeps the same
localization keys, typed bindings, resources, actions, and control IDs. Its
generated semantic test checks automatic topology at narrow width and exercises
name, checkbox, number, slider, dropdown, and derived greeting behavior. Existing
low-level widget exports remain available through the SDK facade.
