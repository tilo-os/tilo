# One example, every language

An example is a whole program: it creates what it needs, and reaches the API at
`TILO_DOOR` with the key in `TILO_API_KEY`. Consecutive blocks with one id are
the same example in each language, so a reader picks the SDK they use.

```python example=create-a-workspace
"""A workspace for one customer, and the workspaces that key reaches."""

import asyncio

from tilo import AsyncTilo


async def main() -> None:
    async with AsyncTilo() as tilo:  # the key is TILO_API_KEY, the door TILO_DOOR
        created = await tilo.workspaces.create("Example customer")
        listing = await tilo.workspaces.list()
        print(created.name)
        print([workspace.name for workspace in listing.workspaces])


asyncio.run(main())
```

```typescript example=create-a-workspace
import { Tilo } from "tilo-sdk";

const tilo = new Tilo(); // the key is TILO_API_KEY, the door TILO_DOOR
const created = await tilo.workspaces.create("Example customer");
const listing = await tilo.workspaces.list();
console.log(created.name);
console.log(listing.workspaces.map((workspace) => workspace.name));
```

```sh example=create-a-workspace
tilo account workspaces create "Example customer"
tilo account workspaces ls
```

A shell block that only shows a command needs no id:

```sh
tilo account workspaces ls
```

A snippet that is not a program says `fragment`:

```python fragment
workspace = await tilo.workspaces.create("Example customer")
```
