"""Types for the apps this workspace has, written by `tilo sdk gen`.

`tilo sdk gen --lang python -w <workspace>` writes them from the apps installed
there; without `-w`, from the apps of the checkout it runs in. Each app has a module
of its own, holding a class per resource kind: a `tilo.Resource` named by the
resource's path, whose fields are the app's fields and whose methods are its actions.

```python
from tilo.types.pdf_viewer import Document

form = await Document(ws, "pdf-viewer/application.pdf").load()
fields = await form.read_form_fields()
```
"""
