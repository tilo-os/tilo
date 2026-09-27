# Automatic and guided forms

`FormPage` owns the viewport, one outer gutter, centered readable width, and vertical scrolling. `Form`, `FormSection` and `Field` hug their contents and work inside ordinary manual rows, columns and containers.

```rust
use ui::widget::{Actions, Button, Field, Form, FormPage, FormSection, TextInput};

let page = FormPage::new("Account", Form::new()
    .section(FormSection::new("Profile")
        .field(Field::new("Name", TextInput::local().with_id("name")))
        .field(Field::new("Email", TextInput::local().with_id("email"))
            .with_help("Used for account recovery")
            .with_error("Enter a valid email address")))
    .with_actions(Actions::new(Button::new("save", "Save", None).primary())));
```

Labels align within each section. The framework measures the actual localized labels and control minima; when the pair cannot fit, labels move above controls. Expandable controls that report no intrinsic width receive a comfortable minimum based on the theme's active type size. Help and validation wrap with the control. Sections and actions retain their declared order. `FormSection::content(Element)` accepts ordinary labelled checkboxes or a manually composed region.

Visible field labels and control names use the same localization key through the existing `FormControl` trait. Validation copy is retained live text; this API does not add AccessKit label/description relationships. Existing `labeled_field` and `labeled_check` helpers are unchanged.

## Independent overrides and reset

| Region | Supported setters |
| --- | --- |
| FormPage | `with_max_content_width(Option<f32>)` |
| Form | `with_density(Option<Density>)`, `with_section_spacing(Option<f32>)`, `with_field_spacing(Option<f32>)`, `with_label_placement(Option<LabelPlacement>)` |
| FormSection | density, field spacing, label placement |
| Field | label placement |

Every setter accepts `None` to restore inheritance. Width is a fluid maximum, excluding the outer gutters. Numeric spacing is logical pixels, clamped to zero; nonfinite values reset to inheritance. Density coordinates gaps without shrinking control targets.

```rust
use ui::widget::{Density, Form, LabelPlacement};
let form = Form::new()
    .with_density(Some(Density::Compact))
    .with_label_placement(Some(LabelPlacement::Above))
    .with_section_spacing(Some(40.0))
    .with_section_spacing(None); // Reset spacing; keep density and label choice.
```

An explicit Field placement wins the nearest FormSection/Form region choice, then the automatic pattern. An explicit spacing value is independent of density, including when inherited from Form. Removing the spacing override restores the appropriate density's theme gap. Forced `Beside` stays beside even in insufficient space; authors own this deliberate constraint and can inspect the required width. Standard children retain their explicit dimensions and may overflow when those dimensions exceed available space.

`Widget::layout_info()` and concrete read-only `effective_layout()` methods expose resolved values and their Theme/Pattern/Form/Section/Field/Page origins. Field information also includes actual available width, required beside width and resolved placement. Values are available after build/settlement through `FrameHarness::inspection()` / `inspection_json()` and the tilo-dev inspection flow; see [Manual layout](MANUAL_LAYOUT.md#existing-preview-and-inspection).

Manual hosts should use `compute_widget_layout`; App and FrameHarness already do. The renderer-aware adjustment hook measures owned children through normal references and callback bindings. It does not create another layout tree, replace retained children, or establish geometry during painting.

## Runnable account fixture and authoring inventory

```sh
cargo run -p ui-examples --example form-composition-shots -- /tmp/form-composition
```

The registered example produces automatic, guided and equivalent primitive sources/captures: wide, narrow, 200% text, narrow 200%, scrolled end, and resized automatic narrow. The populated account form has two sections, name/email, supporting and validation copy, a labelled checkbox, and secondary/primary actions. All captures use FrameHarness.

The equivalent primitive fixture has the same content, controls, order, themed page background, heading hierarchy and broad adaptive intent; exact geometry parity is not claimed. In particular, the primitive Row centers its label against the full control/support stack; the automatic policy measures and aligns to the first control. The comparison counts **16 distinct composition concerns** explicitly authored in `primitive`/`primitive_field`, grouping repeated calls for the same concern:

| # | Primitive source decision | Source expression |
| --- | --- | --- |
| 1 | Scroll ownership | `Scrollable::new_vertical` |
| 2 | Natural container heights | `with_intrinsic_height` |
| 3 | Suppress repeated inner padding | `with_no_padding` |
| 4 | Outer gutters on both axes | vertical/horizontal padding 24 |
| 5 | Centered readable maximum | `with_centered_max_width(720)` |
| 6 | Title, heading, label, body and supporting hierarchy | explicit TextRole choices |
| 7 | Label placement rule | width < 680 or text scale > 1 |
| 8 | Wide label track | `with_width(240)` |
| 9 | Control uses remaining width | `with_flex_grow` |
| 10 | Stacked label/control gap | column gap 8 |
| 11 | Supporting-copy gap | body gap 4 |
| 12 | Within-section spacing | gap 24 |
| 13 | Between sections/actions | gap 32 |
| 14 | Title/description spacing | gap 8 |
| 15 | Heading group/form separation | gap 32 |
| 16 | Complete page surface | filling Container with themed `bg_app` |

`account`/`automatic` supply none of those 16 composition settings: **zero caller layout pixel values and zero viewport branches**. The guided source adds **two selected decisions**, compact density and labels above. Both paths use shared Actions, so its internal button sizing/alignment is not counted as a primitive-only decision.

This is a reproducible source-decision inventory, not measured developer time, human visual acceptance, or a productivity ratio. Application adoption is explicit: Store now uses content-sized cards and shared Actions, while form adoption remains a choice for each appropriate region. For shared child measurement, exact placement, mixed fixtures, existing inspector metadata, and the automatic starter scaffold, see [Manual layout](MANUAL_LAYOUT.md).
