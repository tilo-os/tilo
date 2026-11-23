"""A libsecret client, as GNOME applications are: gi.repository.Secret with
its default (Diffie-Hellman) session, collection browsing through `Items`,
and the plain session through raw D-Bus. Prints `ok <name>` per check and
exits non-zero on the first failure."""

import sys

import gi

gi.require_version("Secret", "1")
from gi.repository import Gio, GLib, Secret  # noqa: E402

SCHEMA = Secret.Schema.new(
    "so.tilo.test.Schema",
    Secret.SchemaFlags.NONE,
    {"account": Secret.SchemaAttributeType.STRING},
)


def ok(name):
    print(f"ok {name}", flush=True)


def check(condition, name):
    if not condition:
        print(f"FAILED {name}", file=sys.stderr, flush=True)
        sys.exit(1)
    ok(name)


# libsecret negotiates dh-ietf1024-sha256-aes128-cbc-pkcs7 for every one of
# these calls.
service = Secret.Service.get_sync(
    Secret.ServiceFlags.OPEN_SESSION | Secret.ServiceFlags.LOAD_COLLECTIONS, None
)
check(service.get_session_algorithms() == "dh-ietf1024-sha256-aes128-cbc-pkcs7", "dh session negotiated")

check(
    Secret.password_store_sync(
        SCHEMA, {"account": "alice"}, Secret.COLLECTION_DEFAULT, "Alice's mail", "s3cret", None
    ),
    "store",
)
check(Secret.password_lookup_sync(SCHEMA, {"account": "alice"}, None) == "s3cret", "lookup")
check(Secret.password_lookup_sync(SCHEMA, {"account": "bob"}, None) is None, "lookup of a missing item")
check(
    Secret.password_store_sync(
        SCHEMA, {"account": "alice"}, Secret.COLLECTION_DEFAULT, "Alice's mail", "s3cret2", None
    )
    and Secret.password_lookup_sync(SCHEMA, {"account": "alice"}, None) == "s3cret2",
    "store replaces",
)

# Browse the way Seahorse and many apps do: the collection's Items.
collection = Secret.Collection.for_alias_sync(
    service, Secret.COLLECTION_DEFAULT, Secret.CollectionFlags.LOAD_ITEMS, None
)
check(collection is not None and not collection.get_locked(), "default collection is unlocked")
items = collection.get_items()
check(len(items) == 1, "Items lists the caller's one item")
item = items[0]
check(item.get_label() == "Alice's mail", "item Label")
check(item.get_attributes().get("account") == "alice", "item Attributes")
check(item.get_created() > 0 and item.get_modified() > 0, "item Created and Modified")
item.load_secret_sync(None)
check(item.get_secret().get_text() == "s3cret2", "item secret over the dh session")

# What another application stored is not visible here.
check(
    Secret.password_lookup_sync(SCHEMA, {"account": "from-secret-tool"}, None) is None,
    "another application's item is invisible",
)

check(Secret.password_clear_sync(SCHEMA, {"account": "alice"}, None), "clear")
check(Secret.password_lookup_sync(SCHEMA, {"account": "alice"}, None) is None, "cleared item is gone")

# The plain session, raw: OpenSession("plain"), CreateItem, GetSecret.
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
NAME = "org.freedesktop.secrets"


def call(path, interface, method, args, signature):
    reply = bus.call_sync(
        NAME, path, interface, method, GLib.Variant(signature, args), None,
        Gio.DBusCallFlags.NONE, 5000, None,
    )
    return reply.unpack()


output, session = call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "OpenSession", ("plain", GLib.Variant("s", "")), "(sv)")
check(output == "" and session.startswith("/org/freedesktop/secrets/session/"), "plain session opens")
props = {
    "org.freedesktop.Secret.Item.Label": GLib.Variant("s", "plain item"),
    "org.freedesktop.Secret.Item.Attributes": GLib.Variant("a{ss}", {"account": "plain"}),
}
created, prompt = call(
    "/org/freedesktop/secrets/aliases/default", "org.freedesktop.Secret.Collection", "CreateItem",
    (props, (session, b"", b"visible", "text/plain"), True),
    "(a{sv}(oayays)b)",
)
check(prompt == "/", "storing needs no prompt")
secret = call(created, "org.freedesktop.Secret.Item", "GetSecret", (session,), "(o)")[0]
check(bytes(secret[2]) == b"visible", "plain session returns the secret")
found, locked = call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "SearchItems", ({"account": "plain"},), "(a{ss})")
check(found == [created] and locked == [], "SearchItems")
listed = bus.call_sync(
    NAME, "/org/freedesktop/secrets/aliases/default", "org.freedesktop.DBus.Properties", "Get",
    GLib.Variant("(ss)", ("org.freedesktop.Secret.Collection", "Items")), None, Gio.DBusCallFlags.NONE, 5000, None,
).unpack()[0]
check(created in listed, "Items includes the plain-session item")
call(created, "org.freedesktop.Secret.Item", "Delete", (), "()")
check(call("/org/freedesktop/secrets", "org.freedesktop.Secret.Service", "SearchItems", ({"account": "plain"},), "(a{ss})")[0] == [], "Delete")
print("done", flush=True)
