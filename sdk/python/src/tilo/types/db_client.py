"""Types for Database Client (`so.tilo.db-client`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `db-client/<resource>`, read with
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
    "Engine",
    "Tab",
    "Notice",
    "Connection",
    "SavedQuery",
    "Database",
    "TableInfo",
    "ColumnInfo",
    "KeyInfo",
    "IndexInfo",
    "Answer",
    "Progress",
    "Opened",
    "Secret",
    "Run",
    "Row",
    "Outcome",
    "Session",
    "OpenAsk",
    "QueryAsk",
    "BrowseAsk",
    "TableAsk",
    "Handle",
    "DbClient",
    "ConnectArgs",
    "DisconnectArgs",
    "AddConnectionArgs",
    "RemoveConnectionArgs",
    "RememberPasswordArgs",
    "SaveQueryArgs",
    "RemoveQueryArgs",
    "LoadQueryArgs",
    "LoadStarterArgs",
    "ChooseTabArgs",
    "ShowTabArgs",
    "BeginTransactionArgs",
    "CommitTransactionArgs",
    "RollbackTransactionArgs",
    "RunArgs",
    "ConfirmRunArgs",
    "DismissRunArgs",
    "RunQueryArgs",
    "RerunArgs",
    "StartStatementArgs",
    "CancelArgs",
    "BrowseArgs",
    "LoadStructureArgs",
    "LoadPageArgs",
    "NextPageArgs",
    "PreviousPageArgs",
    "RefreshCatalogArgs",
    "OrderByArgs",
    "DismissArgs",
    "JobDoneArgs",
    "OpenedArgs",
    "QueriedArgs",
    "ForgetPasswordArgs",
    "ClearTablesArgs",
    "ClearStructureArgs",
    "ClearColumnsArgs",
    "ClearRowsArgs",
    "FailedJobArgs",
]

Engine = Literal["sqlite", "postgres", "mysql"]
"""The engine a connection targets."""

Tab = Literal["query", "data", "structure"]
"""Which of the connection's three views the window shows."""

ConnectArgs = TypedDict("ConnectArgs", {
    "c": "Connection",
})
"""The arguments of `connect`."""

DisconnectArgs = TypedDict("DisconnectArgs", {
})
"""The arguments of `disconnect`."""

AddConnectionArgs = TypedDict("AddConnectionArgs", {
    "name": "str",
    "engine_index": "int",
    "host": "str",
    "port": "int",
    "database": "str",
    "user": "str",
    "file_path": "str",
    "password": "str",
})
"""The arguments of `add_connection`."""

RemoveConnectionArgs = TypedDict("RemoveConnectionArgs", {
    "c": "Connection",
})
"""The arguments of `remove_connection`."""

RememberPasswordArgs = TypedDict("RememberPasswordArgs", {
    "c": "Connection",
    "password": "str",
})
"""The arguments of `remember_password`."""

SaveQueryArgs = TypedDict("SaveQueryArgs", {
    "name": "str",
})
"""The arguments of `save_query`."""

RemoveQueryArgs = TypedDict("RemoveQueryArgs", {
    "q": "SavedQuery",
})
"""The arguments of `remove_query`."""

LoadQueryArgs = TypedDict("LoadQueryArgs", {
    "q": "SavedQuery",
})
"""The arguments of `load_query`."""

LoadStarterArgs = TypedDict("LoadStarterArgs", {
    "name": "str",
    "sql": "str",
})
"""The arguments of `load_starter`."""

ChooseTabArgs = TypedDict("ChooseTabArgs", {
    "index": "int",
})
"""The arguments of `choose_tab`."""

ShowTabArgs = TypedDict("ShowTabArgs", {
    "tab": "Tab",
})
"""The arguments of `show_tab`."""

BeginTransactionArgs = TypedDict("BeginTransactionArgs", {
})
"""The arguments of `begin_transaction`."""

CommitTransactionArgs = TypedDict("CommitTransactionArgs", {
})
"""The arguments of `commit_transaction`."""

RollbackTransactionArgs = TypedDict("RollbackTransactionArgs", {
})
"""The arguments of `rollback_transaction`."""

RunArgs = TypedDict("RunArgs", {
})
"""The arguments of `run`."""

ConfirmRunArgs = TypedDict("ConfirmRunArgs", {
})
"""The arguments of `confirm_run`."""

DismissRunArgs = TypedDict("DismissRunArgs", {
})
"""The arguments of `dismiss_run`."""

RunQueryArgs = TypedDict("RunQueryArgs", {
    "q": "SavedQuery",
})
"""The arguments of `run_query`."""

RerunArgs = TypedDict("RerunArgs", {
    "r": "Run",
})
"""The arguments of `rerun`."""

StartStatementArgs = TypedDict("StartStatementArgs", {
    "sql": "str",
})
"""The arguments of `start_statement`."""

CancelArgs = TypedDict("CancelArgs", {
})
"""The arguments of `cancel`."""

BrowseArgs = TypedDict("BrowseArgs", {
    "t": "TableInfo",
})
"""The arguments of `browse`."""

LoadStructureArgs = TypedDict("LoadStructureArgs", {
})
"""The arguments of `load_structure`."""

LoadPageArgs = TypedDict("LoadPageArgs", {
})
"""The arguments of `load_page`."""

NextPageArgs = TypedDict("NextPageArgs", {
    "at": NotRequired["int | None"],
})
"""The arguments of `next_page`."""

PreviousPageArgs = TypedDict("PreviousPageArgs", {
    "at": NotRequired["int | None"],
})
"""The arguments of `previous_page`."""

RefreshCatalogArgs = TypedDict("RefreshCatalogArgs", {
})
"""The arguments of `refresh_catalog`."""

OrderByArgs = TypedDict("OrderByArgs", {
    "column": "str",
})
"""The arguments of `order_by`."""

DismissArgs = TypedDict("DismissArgs", {
})
"""The arguments of `dismiss`."""

JobDoneArgs = TypedDict("JobDoneArgs", {
    "j": "Any",
})
"""The arguments of `job_done`."""

OpenedArgs = TypedDict("OpenedArgs", {
    "answer": "str",
})
"""The arguments of `opened`."""

QueriedArgs = TypedDict("QueriedArgs", {
    "answer": "str",
})
"""The arguments of `queried`."""

ForgetPasswordArgs = TypedDict("ForgetPasswordArgs", {
    "c": "Connection",
})
"""The arguments of `forget_password`."""

ClearTablesArgs = TypedDict("ClearTablesArgs", {
})
"""The arguments of `clear_tables`."""

ClearStructureArgs = TypedDict("ClearStructureArgs", {
})
"""The arguments of `clear_structure`."""

ClearColumnsArgs = TypedDict("ClearColumnsArgs", {
})
"""The arguments of `clear_columns`."""

ClearRowsArgs = TypedDict("ClearRowsArgs", {
    "result": "bool",
})
"""The arguments of `clear_rows`."""

FailedJobArgs = TypedDict("FailedJobArgs", {
    "why": "str",
})
"""The arguments of `failed_job`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    @property
    def notice_to(self) -> str:
        """to (`notice-to` on the wire)."""
        return self.fields["notice-to"]

    @notice_to.setter
    def notice_to(self, value: str) -> None:
        self.edit("notice-to", value=value)
    @property
    def notice_title(self) -> str:
        """title (`notice-title` on the wire)."""
        return self.fields["notice-title"]

    @notice_title.setter
    def notice_title(self, value: str) -> None:
        self.edit("notice-title", value=value)
    @property
    def notice_body(self) -> str:
        """body (`notice-body` on the wire)."""
        return self.fields["notice-body"]

    @notice_body.setter
    def notice_body(self, value: str) -> None:
        self.edit("notice-body", value=value)
    @property
    def notice_opens(self) -> Any:
        """opens (`notice-opens` on the wire)."""
        return self.fields["notice-opens"]

    @notice_opens.setter
    def notice_opens(self, value: Any) -> None:
        self.edit("notice-opens", value=value)
    @property
    def notice_seen(self) -> bool:
        """seen (`notice-seen` on the wire)."""
        return self.fields["notice-seen"]

    @notice_seen.setter
    def notice_seen(self, value: bool) -> None:
        self.edit("notice-seen", value=value)
    @property
    def notice_device(self) -> str:
        """device (`notice-device` on the wire)."""
        return self.fields["notice-device"]

    @notice_device.setter
    def notice_device(self, value: str) -> None:
        self.edit("notice-device", value=value)
    @property
    def notice_actions(self) -> list[str]:
        """actions (`notice-actions` on the wire)."""
        return self.fields["notice-actions"]

    @notice_actions.setter
    def notice_actions(self, value: list[str]) -> None:
        self.edit("notice-actions", value=value)


class Connection(Resource):
    """Connection — `tl.so.tilo.db-client.Connection`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    @property
    def _52sa65q7(self) -> str:
        """name (`52sa65q7` on the wire)."""
        return self.fields["52sa65q7"]

    @_52sa65q7.setter
    def _52sa65q7(self, value: str) -> None:
        self.edit("52sa65q7", value=value)
    cg25mito: Engine  # engine
    lsni3mrq: str  # host
    axk7b6ff: int  # port
    b3gtxaxj: str  # database
    @property
    def _3zatdgh2(self) -> str:
        """user (`3zatdgh2` on the wire)."""
        return self.fields["3zatdgh2"]

    @_3zatdgh2.setter
    def _3zatdgh2(self, value: str) -> None:
        self.edit("3zatdgh2", value=value)
    zt2vj4bb: str  # file_path
    @property
    def _7wyfzimv(self) -> str | None:
        """used (`7wyfzimv` on the wire)."""
        return self.fields["7wyfzimv"]

    @_7wyfzimv.setter
    def _7wyfzimv(self, value: str | None) -> None:
        self.edit("7wyfzimv", value=value)


class SavedQuery(Resource):
    """SavedQuery — `tl.so.tilo.db-client.SavedQuery`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    fxmhtvbz: str  # name
    rnkrjcin: str  # sql


class Database(Resource):
    """Database — `tl.so.tilo.db-client.Database`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    djghd3ar: list[Entry[Connection]]  # connections
    dbkiebu3: list[Entry[SavedQuery]]  # queries
    r77o5x22: str  # sql
    nvtukp2b: list[Entry[Session]]  # sessions

    def ensure_session(self) -> Session:
        """This person's window, made by their own first action so it is theirs to read and write."""
        return self.action("ensure_session")

    def connect(self, *, c: Connection) -> None:
        """Opens a saved connection: this device starts the engine on it and reads its catalog."""
        return self.action("connect", c=c)

    def connect_many(self, items: list[ConnectArgs]) -> list[dict[str, Any]]:
        """Runs `connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("connect", items)

    def disconnect(self) -> None:
        """Closes the open connection on this device."""
        return self.action("disconnect")

    def disconnect_many(self, items: list[DisconnectArgs]) -> list[dict[str, Any]]:
        """Runs `disconnect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("disconnect", items)

    def add_connection(self,
        *,
        name: str,
        engine_index: int,
        host: str,
        port: int,
        database: str,
        user: str,
        file_path: str,
        password: str,
    ) -> None:
        """Adds a connection to the workspace, keeps its password on this device, and opens it."""
        return self.action("add_connection", name=name, engine_index=engine_index, host=host, port=port, database=database, user=user, file_path=file_path, password=password)

    def add_connection_many(self, items: list[AddConnectionArgs]) -> list[dict[str, Any]]:
        """Runs `add_connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_connection", items)

    def remove_connection(self, *, c: Connection) -> None:
        """Removes a saved connection: its password goes with it on this device."""
        return self.action("remove_connection", c=c)

    def remove_connection_many(self, items: list[RemoveConnectionArgs]) -> list[dict[str, Any]]:
        """Runs `remove_connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_connection", items)

    def remember_password(self,
        *,
        c: Connection,
        password: str,
    ) -> None:
        """Keeps `password` for `c` on this device, or forgets it when it is empty."""
        return self.action("remember_password", c=c, password=password)

    def remember_password_many(self, items: list[RememberPasswordArgs]) -> list[dict[str, Any]]:
        """Runs `remember_password` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember_password", items)

    def save_query(self, *, name: str) -> None:
        """Saves the statement in the editor under `name`."""
        return self.action("save_query", name=name)

    def save_query_many(self, items: list[SaveQueryArgs]) -> list[dict[str, Any]]:
        """Runs `save_query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save_query", items)

    def remove_query(self, *, q: SavedQuery) -> None:
        """Removes a saved query."""
        return self.action("remove_query", q=q)

    def remove_query_many(self, items: list[RemoveQueryArgs]) -> list[dict[str, Any]]:
        """Runs `remove_query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_query", items)

    def load_query(self, *, q: SavedQuery) -> None:
        """Loads a saved query into the editor, ready to run."""
        return self.action("load_query", q=q)

    def load_query_many(self, items: list[LoadQueryArgs]) -> list[dict[str, Any]]:
        """Runs `load_query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_query", items)

    def load_starter(self,
        *,
        name: str,
        sql: str,
    ) -> None:
        """Puts a starter query in the editor, saved so the person can name it too."""
        return self.action("load_starter", name=name, sql=sql)

    def load_starter_many(self, items: list[LoadStarterArgs]) -> list[dict[str, Any]]:
        """Runs `load_starter` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_starter", items)

    def choose_tab(self, *, index: int) -> None:
        """Shows the view the segment at `index` names."""
        return self.action("choose_tab", index=index)

    def choose_tab_many(self, items: list[ChooseTabArgs]) -> list[dict[str, Any]]:
        """Runs `choose_tab` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose_tab", items)

    def show_tab(self, *, tab: Tab) -> None:
        """Shows one of the window's three views."""
        return self.action("show_tab", tab=tab)

    def show_tab_many(self, items: list[ShowTabArgs]) -> list[dict[str, Any]]:
        """Runs `show_tab` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_tab", items)

    def begin_transaction(self) -> None:
        """Starts a transaction on the open connection: what runs until the person commits is one change, and rolling it back takes all of it back."""
        return self.action("begin_transaction")

    def begin_transaction_many(self, items: list[BeginTransactionArgs]) -> list[dict[str, Any]]:
        """Runs `begin_transaction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin_transaction", items)

    def commit_transaction(self) -> None:
        """Commits the open transaction: what it changed is the database's from now on."""
        return self.action("commit_transaction")

    def commit_transaction_many(self, items: list[CommitTransactionArgs]) -> list[dict[str, Any]]:
        """Runs `commit_transaction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("commit_transaction", items)

    def rollback_transaction(self) -> None:
        """Rolls the open transaction back: every change it made is taken back."""
        return self.action("rollback_transaction")

    def rollback_transaction_many(self, items: list[RollbackTransactionArgs]) -> list[dict[str, Any]]:
        """Runs `rollback_transaction` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rollback_transaction", items)

    def run(self) -> None:
        """Runs the statement in the editor, asking first when it changes data."""
        return self.action("run")

    def run_many(self, items: list[RunArgs]) -> list[dict[str, Any]]:
        """Runs `run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("run", items)

    def confirm_run(self) -> None:
        """Runs the statement the person confirmed."""
        return self.action("confirm_run")

    def confirm_run_many(self, items: list[ConfirmRunArgs]) -> list[dict[str, Any]]:
        """Runs `confirm_run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("confirm_run", items)

    def dismiss_run(self) -> None:
        """Puts away the statement that was waiting for confirmation."""
        return self.action("dismiss_run")

    def dismiss_run_many(self, items: list[DismissRunArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_run", items)

    def run_query(self, *, q: SavedQuery) -> None:
        """Runs a saved query against the open connection."""
        return self.action("run_query", q=q)

    def run_query_many(self, items: list[RunQueryArgs]) -> list[dict[str, Any]]:
        """Runs `run_query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("run_query", items)

    def rerun(self, *, r: Run) -> None:
        """Runs a statement this window ran before."""
        return self.action("rerun", r=r)

    def rerun_many(self, items: list[RerunArgs]) -> list[dict[str, Any]]:
        """Runs `rerun` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rerun", items)

    def start_statement(self, *, sql: str) -> None:
        """Starts `sql` on the open connection as a job, so the window stays live and the statement can be stopped."""
        return self.action("start_statement", sql=sql)

    def start_statement_many(self, items: list[StartStatementArgs]) -> list[dict[str, Any]]:
        """Runs `start_statement` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_statement", items)

    def cancel(self) -> None:
        """Stops the statement in flight."""
        return self.action("cancel")

    def cancel_many(self, items: list[CancelArgs]) -> list[dict[str, Any]]:
        """Runs `cancel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel", items)

    def browse(self, *, t: TableInfo) -> None:
        """Opens the table `t`: what it is made of, and its first page of rows."""
        return self.action("browse", t=t)

    def browse_many(self, items: list[BrowseArgs]) -> list[dict[str, Any]]:
        """Runs `browse` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("browse", items)

    def load_structure(self) -> None:
        """Reads the selected table's keys and indexes; its columns came with the catalog."""
        return self.action("load_structure")

    def load_structure_many(self, items: list[LoadStructureArgs]) -> list[dict[str, Any]]:
        """Runs `load_structure` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_structure", items)

    def load_page(self) -> None:
        """Loads one page of the selected table's rows."""
        return self.action("load_page")

    def load_page_many(self, items: list[LoadPageArgs]) -> list[dict[str, Any]]:
        """Runs `load_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("load_page", items)

    def next_page(self,
        *,
        at: int | None = None,
    ) -> None:
        """Shows the next page of the selected table's rows."""
        args: dict[str, Any] = {}
        if at is not None:
            args["at"] = at
        return self.action("next_page", **args)

    def next_page_many(self, items: list[NextPageArgs]) -> list[dict[str, Any]]:
        """Runs `next_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("next_page", items)

    def previous_page(self,
        *,
        at: int | None = None,
    ) -> None:
        """Shows the page before the one shown."""
        args: dict[str, Any] = {}
        if at is not None:
            args["at"] = at
        return self.action("previous_page", **args)

    def previous_page_many(self, items: list[PreviousPageArgs]) -> list[dict[str, Any]]:
        """Runs `previous_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("previous_page", items)

    def refresh_catalog(self) -> None:
        """Reads the open connection's catalog again, so a table made elsewhere shows."""
        return self.action("refresh_catalog")

    def refresh_catalog_many(self, items: list[RefreshCatalogArgs]) -> list[dict[str, Any]]:
        """Runs `refresh_catalog` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("refresh_catalog", items)

    def order_by(self, *, column: str) -> None:
        """Orders the result by the column the person chose, or back to the statement's own order."""
        return self.action("order_by", column=column)

    def order_by_many(self, items: list[OrderByArgs]) -> list[dict[str, Any]]:
        """Runs `order_by` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("order_by", items)

    def copy_row(self, *, row: Row) -> None:
        """Copies the result row at `at` as one line of text."""
        return self.action("copy_row", row=row)

    def copy_result(self) -> None:
        """Copies the whole result as tab-separated text."""
        return self.action("copy_result")

    def export_csv(self) -> Any | None:
        """The statement's result as a CSV file, for a person to keep."""
        return self.action("export_csv")

    def dismiss(self) -> None:
        """Clears what the window last said."""
        return self.action("dismiss")

    def dismiss_many(self, items: list[DismissArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss", items)

    def job_done(self, *, j: Any) -> None:
        """The job in flight ended: its answer is read as what it is, or the window says why it failed or was stopped. A job this window no longer runs leaves the window as it is."""
        return self.action("job_done", j=j)

    def job_done_many(self, items: list[JobDoneArgs]) -> list[dict[str, Any]]:
        """Runs `job_done` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("job_done", items)

    def opened(self, *, answer: str) -> None:
        """Takes a connection's handle in, once the engine opened it, and reads its catalog."""
        return self.action("opened", answer=answer)

    def opened_many(self, items: list[OpenedArgs]) -> list[dict[str, Any]]:
        """Runs `opened` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("opened", items)

    def queried(self, *, answer: str) -> None:
        """Takes a statement's result in, with the words the window shows of it."""
        return self.action("queried", answer=answer)

    def queried_many(self, items: list[QueriedArgs]) -> list[dict[str, Any]]:
        """Runs `queried` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("queried", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"ensure_session", "connect", "connect_many", "disconnect", "disconnect_many", "add_connection", "add_connection_many", "remove_connection", "remove_connection_many", "remember_password", "remember_password_many", "save_query", "save_query_many", "remove_query", "remove_query_many", "load_query", "load_query_many", "load_starter", "load_starter_many", "choose_tab", "choose_tab_many", "show_tab", "show_tab_many", "begin_transaction", "begin_transaction_many", "commit_transaction", "commit_transaction_many", "rollback_transaction", "rollback_transaction_many", "run", "run_many", "confirm_run", "confirm_run_many", "dismiss_run", "dismiss_run_many", "run_query", "run_query_many", "rerun", "rerun_many", "start_statement", "start_statement_many", "cancel", "cancel_many", "browse", "browse_many", "load_structure", "load_structure_many", "load_page", "load_page_many", "next_page", "next_page_many", "previous_page", "previous_page_many", "refresh_catalog", "refresh_catalog_many", "order_by", "order_by_many", "copy_row", "copy_result", "export_csv", "dismiss", "dismiss_many", "job_done", "job_done_many", "opened", "opened_many", "queried", "queried_many"})


class TableInfo(Resource):
    """TableInfo — `tl.so.tilo.db-client.TableInfo`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    yofk7dul: str  # schema
    h5zenygt: str  # name
    tbo6xpjg: str  # kind
    @property
    def _7xr4zpm5(self) -> int | None:
        """row_count (`7xr4zpm5` on the wire)."""
        return self.fields["7xr4zpm5"]

    @_7xr4zpm5.setter
    def _7xr4zpm5(self, value: int | None) -> None:
        self.edit("7xr4zpm5", value=value)


class ColumnInfo(Resource):
    """ColumnInfo — `tl.so.tilo.db-client.ColumnInfo`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    ybqhn5bv: str  # schema
    mti4en3a: str  # table
    qmrcvjjn: str  # name
    ffkug4qg: str  # decl_type
    ifa6r5by: bool  # nullable
    sew352lt: bool  # primary_key
    @property
    def _4ycs3rch(self) -> str | None:
        """default_value (`4ycs3rch` on the wire)."""
        return self.fields["4ycs3rch"]

    @_4ycs3rch.setter
    def _4ycs3rch(self, value: str | None) -> None:
        self.edit("4ycs3rch", value=value)


class KeyInfo(Resource):
    """KeyInfo — `tl.so.tilo.db-client.KeyInfo`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    @property
    def _4sqibo4s(self) -> str:
        """column (`4sqibo4s` on the wire)."""
        return self.fields["4sqibo4s"]

    @_4sqibo4s.setter
    def _4sqibo4s(self, value: str) -> None:
        self.edit("4sqibo4s", value=value)
    hmchr5lj: str  # ref_table
    nse42ud4: str  # ref_column


class IndexInfo(Resource):
    """IndexInfo — `tl.so.tilo.db-client.IndexInfo`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    bmcdtfyt: str  # name
    co7n2x5l: bool  # unique
    ws3nxyhc: str  # columns


class Answer(Resource):
    """Answer — `tl.so.tilo.db-client.Answer`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    n7guubug: list[str]  # columns
    zmjmvjeu: list[list[str]]  # rows
    ztnqa552: list[list[bool]]  # nulls
    gysqh6ps: list[bool]  # numeric
    h24jm6lc: int | None  # rows_affected
    ydn3x76v: bool  # is_select
    @property
    def _7vfwgci5(self) -> float:
        """elapsed_ms (`7vfwgci5` on the wire)."""
        return self.fields["7vfwgci5"]

    @_7vfwgci5.setter
    def _7vfwgci5(self, value: float) -> None:
        self.edit("7vfwgci5", value=value)
    qf7pdvx7: str  # truncated


class Progress(Resource):
    """Progress — `tl.so.tilo.db-client.Progress`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    bpgwkoxz: str  # result


class Opened(Resource):
    """Opened — `tl.so.tilo.db-client.Opened`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    yheverkf: int  # connection


class Secret(Resource):
    """Secret — `tl.so.tilo.db-client.Secret`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    psin3vp7: str  # connection
    @property
    def _3twvmxwg(self) -> str:
        """password (`3twvmxwg` on the wire)."""
        return self.fields["3twvmxwg"]

    @_3twvmxwg.setter
    def _3twvmxwg(self, value: str) -> None:
        self.edit("3twvmxwg", value=value)


class Run(Resource):
    """Run — `tl.so.tilo.db-client.Run`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    @property
    def _6abruthm(self) -> str:
        """connection (`6abruthm` on the wire)."""
        return self.fields["6abruthm"]

    @_6abruthm.setter
    def _6abruthm(self, value: str) -> None:
        self.edit("6abruthm", value=value)
    nxawaqfc: str  # sql
    @property
    def _3fza7fgn(self) -> str:
        """summary (`3fza7fgn` on the wire)."""
        return self.fields["3fza7fgn"]

    @_3fza7fgn.setter
    def _3fza7fgn(self, value: str) -> None:
        self.edit("3fza7fgn", value=value)


class Row(Resource):
    """Row — `tl.so.tilo.db-client.Row`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    hxhdvye5: list[str]  # cells
    gc6jygal: list[bool]  # nulls


class Outcome(Resource):
    """Outcome — `tl.so.tilo.db-client.Outcome`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    qxwjsiht: int  # seq
    rym2yft2: list[str]  # columns
    m6wzfydf: list[bool]  # numeric
    @property
    def _2k7ob6vx(self) -> int:
        """affected (`2k7ob6vx` on the wire)."""
        return self.fields["2k7ob6vx"]

    @_2k7ob6vx.setter
    def _2k7ob6vx(self, value: int) -> None:
        self.edit("2k7ob6vx", value=value)
    kbnzmtdp: bool  # is_select
    @property
    def _4ponyk7v(self) -> float:
        """elapsed_ms (`4ponyk7v` on the wire)."""
        return self.fields["4ponyk7v"]

    @_4ponyk7v.setter
    def _4ponyk7v(self, value: float) -> None:
        self.edit("4ponyk7v", value=value)
    iaudq64k: str  # truncated
    qzyl3fzm: str  # summary
    fywguxl3: str  # order
    e5veu4zz: bool  # descending


class Session(Resource):
    """Session — `tl.so.tilo.db-client.Session`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    elhp2adh: str  # open
    @property
    def _73sz5sht(self) -> str:
        """name (`73sz5sht` on the wire)."""
        return self.fields["73sz5sht"]

    @_73sz5sht.setter
    def _73sz5sht(self, value: str) -> None:
        self.edit("73sz5sht", value=value)
    m2eenfte: bool  # connected
    l6ey6bmu: int  # handle
    utbjkevx: Tab  # tab
    @property
    def _62jlzlao(self) -> list[str]:
        """schemas (`62jlzlao` on the wire)."""
        return self.fields["62jlzlao"]

    @_62jlzlao.setter
    def _62jlzlao(self, value: list[str]) -> None:
        self.edit("62jlzlao", value=value)
    c5axoen6: list[Entry[TableInfo]]  # tables
    tzehhxal: list[Entry[ColumnInfo]]  # catalog_columns
    p5c7ywkh: list[Entry[KeyInfo]]  # keys
    zb4ttvcb: list[Entry[IndexInfo]]  # indexes
    y5w3id3d: str  # selected_schema
    efbptxv4: str  # selected_table
    @property
    def _4e7a56ci(self) -> int:
        """offset (`4e7a56ci` on the wire)."""
        return self.fields["4e7a56ci"]

    @_4e7a56ci.setter
    def _4e7a56ci(self, value: int) -> None:
        self.edit("4e7a56ci", value=value)
    ikep24c2: int  # page
    ymfu3vqr: str  # status
    z4bjp5xk: str  # notice
    p4wji46q: str  # busy
    d2dlobjx: str  # job_kind
    rxkpfssd: str  # pending
    dvgcj2vb: bool  # confirming
    pmf3mqgy: list[Entry[Outcome]]  # queries
    d6drmo4f: list[Entry[Outcome]]  # pages
    qfvcjkgl: list[Entry[Row]]  # result_rows
    iimyuzxn: list[Entry[Row]]  # page_rows
    g6cz4ye2: list[Entry[Run]]  # history
    yxlfv3op: bool  # transaction
    ergnzsjf: str  # started_sql
    qcislb6v: list[Entry[Secret]]  # secrets
    xrk5rswl: str  # working

    def forget_password(self, *, c: Connection) -> None:
        """Drops what this device kept for `c`."""
        return self.action("forget_password", c=c)

    def forget_password_many(self, items: list[ForgetPasswordArgs]) -> list[dict[str, Any]]:
        """Runs `forget_password` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_password", items)

    def clear_tables(self) -> None:
        """Empties the tables a window holds, so a catalog read again never stands beside the one before."""
        return self.action("clear_tables")

    def clear_tables_many(self, items: list[ClearTablesArgs]) -> list[dict[str, Any]]:
        """Runs `clear_tables` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_tables", items)

    def clear_structure(self) -> None:
        """Empties the keys and indexes of the relation the window last chose."""
        return self.action("clear_structure")

    def clear_structure_many(self, items: list[ClearStructureArgs]) -> list[dict[str, Any]]:
        """Runs `clear_structure` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_structure", items)

    def clear_columns(self) -> None:
        """Empties the columns a window holds, so a catalog read again never stands beside the one before."""
        return self.action("clear_columns")

    def clear_columns_many(self, items: list[ClearColumnsArgs]) -> list[dict[str, Any]]:
        """Runs `clear_columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_columns", items)

    def clear_rows(self, *, result: bool) -> None:
        """Empties a window's rows, so a fresh result never stands beside the one before it."""
        return self.action("clear_rows", result=result)

    def clear_rows_many(self, items: list[ClearRowsArgs]) -> list[dict[str, Any]]:
        """Runs `clear_rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_rows", items)

    def failed_job(self, *, why: str) -> None:
        """The window's job could not be asked about at all: it is over, and the window says why."""
        return self.action("failed_job", why=why)

    def failed_job_many(self, items: list[FailedJobArgs]) -> list[dict[str, Any]]:
        """Runs `failed_job` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("failed_job", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"forget_password", "forget_password_many", "clear_tables", "clear_tables_many", "clear_structure", "clear_structure_many", "clear_columns", "clear_columns_many", "clear_rows", "clear_rows_many", "failed_job", "failed_job_many"})


class OpenAsk(Resource):
    """OpenAsk — `tl.so.tilo.db-client.OpenAsk`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    wr4rh4km: str  # engine
    @property
    def _7ub5mnfo(self) -> str:
        """host (`7ub5mnfo` on the wire)."""
        return self.fields["7ub5mnfo"]

    @_7ub5mnfo.setter
    def _7ub5mnfo(self, value: str) -> None:
        self.edit("7ub5mnfo", value=value)
    rr35b5xd: int  # port
    dmjf4ady: str  # database
    g5wwnnue: str  # user
    y5ginenz: str  # password
    eak32p2q: str  # file_path


class QueryAsk(Resource):
    """QueryAsk — `tl.so.tilo.db-client.QueryAsk`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    @property
    def _64plkkzv(self) -> int:
        """connection (`64plkkzv` on the wire)."""
        return self.fields["64plkkzv"]

    @_64plkkzv.setter
    def _64plkkzv(self, value: int) -> None:
        self.edit("64plkkzv", value=value)
    eeqjqurk: str  # sql


class BrowseAsk(Resource):
    """BrowseAsk — `tl.so.tilo.db-client.BrowseAsk`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    ktmsx6gt: int  # connection
    urg4wtul: str  # schema
    @property
    def _4j7eh7ld(self) -> str:
        """table (`4j7eh7ld` on the wire)."""
        return self.fields["4j7eh7ld"]

    @_4j7eh7ld.setter
    def _4j7eh7ld(self, value: str) -> None:
        self.edit("4j7eh7ld", value=value)
    qexpbqva: int  # limit
    ooei2svb: int  # offset


class TableAsk(Resource):
    """TableAsk — `tl.so.tilo.db-client.TableAsk`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    fmbqqhio: int  # connection
    @property
    def _2maemw4x(self) -> str:
        """schema (`2maemw4x` on the wire)."""
        return self.fields["2maemw4x"]

    @_2maemw4x.setter
    def _2maemw4x(self, value: str) -> None:
        self.edit("2maemw4x", value=value)
    wwrvwb2w: str  # table


class Handle(Resource):
    """Handle — `tl.so.tilo.db-client.Handle`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"
    qjoudlls: int  # connection


class DbClient(Resource):
    """Database Client — `tl.so.tilo.db-client`."""
    __surface__ = "so.tilo.db-client@bd20e39fa4ba3423"

