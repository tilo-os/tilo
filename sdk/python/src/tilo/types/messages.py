"""Types for Messages (`so.tilo.messages`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Messages` is the app's `tilo.messages` at `messages/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers an `Invocation`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one invocation group.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Entry, Invocation, Resource

__all__ = [
    "Conversation",
    "Message",
    "Messages",
]

Conversation = TypedDict("Conversation", {
    "with": "str",
    "messages": "list[Entry[Message]]",
})

Message = TypedDict("Message", {
    "from": "str",
    "to": "str",
    "body": "str",
    "date": "str",
    "at_ms": "int",
    "outgoing": "bool",
    "queued": "bool",
})

class Messages(Resource):
    """Messages — `tilo.messages`."""
    __surface__ = "so.tilo.messages@839f8a04619dec24"
    @property
    def conversations(self) -> list[Entry[Conversation]]:
        """Read only."""
        return self.fields["conversations"]

    def deliver_message(self,
        *,
        from_: str,
        body: str,
        to: str | None = None,
        date: str | None = None,
        conversation: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["from"] = from_
        if to is not None:
            args["to"] = to
        args["body"] = body
        if date is not None:
            args["date"] = date
        if conversation is not None:
            args["conversation"] = conversation
        return self.action("deliver-message", **args)

    def send_message(self,
        *,
        to: str,
        body: str,
        conversation: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["to"] = to
        args["body"] = body
        if conversation is not None:
            args["conversation"] = conversation
        return self.action("send-message", **args)

    def reply(self,
        *,
        conversation: str,
        body: str,
    ) -> Any:
        """Runs the app's action."""
        return self.action("reply", conversation=conversation, body=body)

    def list_groups(self,
        *,
        service: str,
        account: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("list-groups", service=service, account=account)

    def read_group_membership(self,
        *,
        service: str,
        account: str,
        group: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("read-group-membership", service=service, account=account, group=group)

    def read_group_history(self,
        *,
        service: str,
        account: str,
        group: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("read-group-history", service=service, account=account, group=group)

    def read_history(self, *, choice: str) -> None:
        """Runs the app's action."""
        return self.action("read-history", choice=choice)

    def read_older_history(self, *, choice: str) -> None:
        """Runs the app's action."""
        return self.action("read-older-history", choice=choice)

    def search_messages(self, *, query: str) -> None:
        """Runs the app's action."""
        return self.action("search-messages", query=query)

    def export_content(self,
        *,
        format: str | None = None,
        conversation: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if format is not None:
            args["format"] = format
        if conversation is not None:
            args["conversation"] = conversation
        return self.action("export-content", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"deliver_message", "send_message", "reply", "list_groups", "read_group_membership", "read_group_history", "read_history", "read_older_history", "search_messages", "export_content"})


