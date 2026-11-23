# Installation-media model compatibility fixture

`model_before.json` is the frozen contract of the installation-media model as it was
before native-engine extraction: serialized JSON, derived JSON schemas, CRDT model
values and schemas, typed field paths, stable source ids and display formatting. It is
test input, not another production model.

The production model is `imager_backend::imager_model`; the app reexports its types.
`model::compatibility_tests` renders the same facts from today's model and compares
them to this frozen value. A change to any serialized field, schema, path, id or
formatting the fixture records fails the test. Do not regenerate the fixture to make an
incompatible change pass: a deliberate change updates it together with the model and
explains why in the commit.
