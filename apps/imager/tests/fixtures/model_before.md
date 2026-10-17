# Installation-media model compatibility fixture

`model_before.rs` preserves the model definitions from before native-engine extraction,
including serde attributes, resource metadata, field names, descriptions and source ID
rules. It is test input, not another production model. Its original UI-only icon and
formatting methods remain here so the comparison also checks presentation compatibility.

The production model is `imager_backend::imager_model`; the app reexports its types.
`model::compatibility_tests` compares actual serialized JSON, derived JSON schemas,
CRDT model values and schemas, typed field paths and stable source IDs against these
frozen definitions. Do not regenerate the fixture to make an incompatible change pass.
