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
from ._endpoint import RunGroup
from ._errors import (
    AskPending,
    KeyExpired,
    KeyRefused,
    KeyRevoked,
    ProtocolError,
    RateLimited,
    Refused,
    SurfaceChanged,
    UnsupportedVersion,
)
from ._models import (
    Event,
    Hit,
    Invitation,
    Key,
    Listing,
    Member,
    MintedKey,
    Person,
    ProgramAnswer,
    Row,
    Usage,
    WorkspaceInfo,
    WorkspaceList,
    WorkspaceUsage,
)
from ._resource import FieldMap, Resource
from ._run import Run
from ._transport import HttpTransport, NativeTransport, Transport
from ._workspace import Files, Members, RunOpening, Workspace, WorkspaceKeys

__version__ = "0.1.0"

__all__ = [
    "AccountKeys",
    "AskPending",
    "AsyncTilo",
    "Event",
    "FieldMap",
    "Files",
    "Hit",
    "HttpTransport",
    "Invitation",
    "Key",
    "KeyExpired",
    "KeyRefused",
    "KeyRevoked",
    "Listing",
    "Member",
    "Members",
    "MintedKey",
    "NativeTransport",
    "Person",
    "ProgramAnswer",
    "ProtocolError",
    "RateLimited",
    "Refused",
    "Resource",
    "Row",
    "Run",
    "RunGroup",
    "RunOpening",
    "SignIn",
    "SurfaceChanged",
    "Tilo",
    "Transport",
    "UnsupportedVersion",
    "Usage",
    "Workspace",
    "WorkspaceInfo",
    "WorkspaceKeys",
    "WorkspaceList",
    "WorkspaceUsage",
    "Workspaces",
    "__version__",
]
