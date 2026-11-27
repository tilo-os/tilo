"""Drive Tilo workspaces from Python.

```python
from tilo import Tilo, run

async with Tilo() as tilo:                      # this computer's daemon
    workspace = tilo.workspace()                # the workspace it serves
    page = await workspace.resource("notes/Todo").load()
    page.title = "Done"                         # a field edit
    await page.flush()
    await run(workspace, {"path": "notes", "action": "new-note"}, {"title": "Next"})
```

`Tilo` and `Workspace` are the API's operations, written from its description by `tilo sdk
gen`: `workspace.list_resources`, `workspace.run_action`, `workspace.events`,
`workspace.put_content`, … Each goes through this computer's daemon (the `tilo._native`
extension), which answers `/v1` as whoever connected, or by URL with a key
(`Tilo(key=...)`).
"""

# The SDK's version, which its transports name themselves with: set before they are imported.
__version__ = "0.1.0"

from ._errors import Refused
from ._resource import Entry, FieldMap, Resource, exported, made, resource_proxy, rows
from ._client import InvocationGroup, Tilo, Workspace, run
from ._transport import (
    CallAsk,
    CallFrame,
    Daemon,
    DaemonConnection,
    DEFAULT_URL,
    NATIVE_MODULE,
    Send,
    VERSION,
)
from ._vault import CallLane, Vault

# The same client, under the name its async interface was first written with.
AsyncTilo = Tilo

__all__ = [
    "AsyncTilo",
    "CallAsk",
    "CallFrame",
    "CallLane",
    "Daemon",
    "DaemonConnection",
    "DEFAULT_URL",
    "Entry",
    "exported",
    "FieldMap",
    "InvocationGroup",
    "made",
    "NATIVE_MODULE",
    "Refused",
    "Resource",
    "resource_proxy",
    "rows",
    "run",
    "Send",
    "Tilo",
    "Vault",
    "VERSION",
    "Workspace",
    "__version__",
]
