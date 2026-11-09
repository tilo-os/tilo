"""Types for the apps a workspace has, written by `tilo sdk gen`.

`tilo sdk gen --lang python -w <workspace>` writes a `.pyi` per app's
resource kinds here, each class a `tilo.Resource` with typed properties and
one method per action. Until then every resource still works through
`ws.resource(path)` and `ws.do(path, **args)`.
"""
