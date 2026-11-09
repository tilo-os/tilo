"""Drive Tilo workspaces from Python.

```python
from tilo import AsyncTilo

async with AsyncTilo() as tilo:
    workspace = tilo.workspace("jane-doe")
    await workspace.ls()
    page = await workspace.read("notes/Todo")
    print(page.title)
    await workspace.do("notes/Todo#append-paragraph", text="Buy milk")
```
"""

from ._client import AccountKeys, AsyncTilo, SignIn, Tilo, Workspaces
from ._errors import AskPending, ProtocolError, Refused
from ._link import RunGroup
from ._models import (
    Event,
    Hit,
    Host,
    Key,
    Listing,
    MintedKey,
    Person,
    ProgramAnswer,
    Row,
    WorkspaceInfo,
    WorkspaceList,
)
from ._resource import FieldMap, Resource
from ._run import Run
from ._workspace import Files, RunOpening, Workspace, WorkspaceKeys

__version__ = "0.1.0"

__all__ = [
    "AccountKeys",
    "AskPending",
    "AsyncTilo",
    "Event",
    "FieldMap",
    "Files",
    "Hit",
    "Host",
    "Key",
    "Listing",
    "MintedKey",
    "Person",
    "ProgramAnswer",
    "ProtocolError",
    "Refused",
    "Resource",
    "Row",
    "Run",
    "RunGroup",
    "RunOpening",
    "SignIn",
    "Tilo",
    "Workspace",
    "WorkspaceInfo",
    "WorkspaceKeys",
    "WorkspaceList",
    "Workspaces",
    "__version__",
]
