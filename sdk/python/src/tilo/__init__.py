"""Drive Tilo workspaces from Python.

```python
from tilo import AsyncTilo

async with AsyncTilo() as tilo:                  # this computer's daemon
    workspace = tilo.workspace()
    page = await workspace.read("notes/Todo")
    print(page.title)
    await workspace.do("notes/Todo#append-paragraph", text="Buy milk")
```

Every call is one tool call of Tilo's agent surface (`tilo_ls`, `tilo_read`,
`tilo_do`, …), carried either by this computer's daemon (the default) or, with
a key, by a hosted API server. `Carrier` is what a program may implement
itself.
"""

# The SDK's version, which its carriers name themselves with: set before they are imported.
__version__ = "0.1.0"

from ._carrier import Carrier, DaemonCarrier, KeyCarrier
from ._client import AccountKeys, AsyncTilo, Tilo, Workspaces
from ._endpoint import InvocationGroup
from ._errors import (
    AskPending,
    KeyExpired,
    KeyRefused,
    KeyRevoked,
    ProtocolError,
    Refused,
    SurfaceChanged,
    Unreachable,
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
    InvocationInfo,
    InvocationList,
    Usage,
    WorkspaceExport,
    WorkspaceInfo,
    WorkspaceList,
    WorkspaceUsage,
)
from ._resource import Entry, FieldMap, Resource, made
from ._invocation import Invocation
from ._workspace import (
    Files,
    InvocationOpening,
    Members,
    Vault,
    Workspace,
    WorkspaceKeys,
)

__all__ = [
    "AccountKeys",
    "AskPending",
    "AsyncTilo",
    "Carrier",
    "DaemonCarrier",
    "Entry",
    "Event",
    "FieldMap",
    "Files",
    "Hit",
    "Invitation",
    "Key",
    "KeyCarrier",
    "KeyExpired",
    "KeyRefused",
    "KeyRevoked",
    "Listing",
    "made",
    "Member",
    "Members",
    "MintedKey",
    "Person",
    "ProgramAnswer",
    "ProtocolError",
    "Refused",
    "Resource",
    "Row",
    "Invocation",
    "InvocationGroup",
    "InvocationInfo",
    "InvocationList",
    "InvocationOpening",
    "SurfaceChanged",
    "Tilo",
    "Unreachable",
    "Usage",
    "Vault",
    "Workspace",
    "WorkspaceExport",
    "WorkspaceInfo",
    "WorkspaceKeys",
    "WorkspaceList",
    "WorkspaceUsage",
    "Workspaces",
    "__version__",
]
