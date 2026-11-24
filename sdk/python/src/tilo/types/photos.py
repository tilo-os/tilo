"""Types for Photos (`so.tilo.photos`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Photo` is the app's `tl.so.tilo.photos.Photo` at `photos/<resource>`, read with
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
    "Photo",
    "Album",
    "Person",
    "Library",
    "Photos",
    "ToggleFavoriteArgs",
    "RotateRightArgs",
    "RotateLeftArgs",
    "ResetEditsArgs",
    "RecognizeTextArgs",
    "CropPhotoArgs",
    "TagArgs",
    "UntagArgs",
    "SetPlaceArgs",
    "ClearPlaceArgs",
    "AddSelectedToAlbumArgs",
    "RenameAlbumArgs",
    "AlbumContainsToggleArgs",
    "ImportDroppedArgs",
    "DismissImportArgs",
    "FavoriteSelectedArgs",
    "RemoveSelectedArgs",
    "RemoveAlbumArgs",
    "AddPersonArgs",
    "RemovePersonArgs",
    "RenamePersonArgs",
    "MergePeopleArgs",
    "SplitPhotoArgs",
    "RemovePersonResourceArgs",
]

ToggleFavoriteArgs = TypedDict("ToggleFavoriteArgs", {
    "favorite": NotRequired["bool | None"],
})
"""The arguments of `toggle_favorite`."""

RotateRightArgs = TypedDict("RotateRightArgs", {
})
"""The arguments of `rotate_right`."""

RotateLeftArgs = TypedDict("RotateLeftArgs", {
})
"""The arguments of `rotate_left`."""

ResetEditsArgs = TypedDict("ResetEditsArgs", {
})
"""The arguments of `reset_edits`."""

RecognizeTextArgs = TypedDict("RecognizeTextArgs", {
})
"""The arguments of `recognize_text`."""

CropPhotoArgs = TypedDict("CropPhotoArgs", {
    "x": "float",
    "y": "float",
    "width": "float",
    "height": "float",
})
"""The arguments of `crop_photo`."""

TagArgs = TypedDict("TagArgs", {
    "t": "str",
})
"""The arguments of `tag`."""

UntagArgs = TypedDict("UntagArgs", {
    "t": "str",
})
"""The arguments of `untag`."""

SetPlaceArgs = TypedDict("SetPlaceArgs", {
    "at": "list[float]",
})
"""The arguments of `set_place`."""

ClearPlaceArgs = TypedDict("ClearPlaceArgs", {
})
"""The arguments of `clear_place`."""

AddSelectedToAlbumArgs = TypedDict("AddSelectedToAlbumArgs", {
    "ids": "list[str]",
})
"""The arguments of `add_selected_to_album`."""

RenameAlbumArgs = TypedDict("RenameAlbumArgs", {
    "to": "str",
})
"""The arguments of `rename_album`."""

AlbumContainsToggleArgs = TypedDict("AlbumContainsToggleArgs", {
    "p": "Photo",
})
"""The arguments of `album_contains_toggle`."""

ImportDroppedArgs = TypedDict("ImportDroppedArgs", {
    "files": "list[Any]",
})
"""The arguments of `import_dropped`."""

DismissImportArgs = TypedDict("DismissImportArgs", {
})
"""The arguments of `dismiss_import`."""

FavoriteSelectedArgs = TypedDict("FavoriteSelectedArgs", {
    "ids": "list[str]",
})
"""The arguments of `favorite_selected`."""

RemoveSelectedArgs = TypedDict("RemoveSelectedArgs", {
    "ids": "list[str]",
})
"""The arguments of `remove_selected`."""

RemoveAlbumArgs = TypedDict("RemoveAlbumArgs", {
    "a": "Album",
})
"""The arguments of `remove_album`."""

AddPersonArgs = TypedDict("AddPersonArgs", {
    "p": "Photo",
    "person": "str",
})
"""The arguments of `add_person`."""

RemovePersonArgs = TypedDict("RemovePersonArgs", {
    "p": "Photo",
    "person": "str",
})
"""The arguments of `remove_person`."""

RenamePersonArgs = TypedDict("RenamePersonArgs", {
    "person": "Person",
    "to": "str",
})
"""The arguments of `rename_person`."""

MergePeopleArgs = TypedDict("MergePeopleArgs", {
    "from": "Person",
    "into": "Person",
})
"""The arguments of `merge_people`."""

SplitPhotoArgs = TypedDict("SplitPhotoArgs", {
    "person": "Person",
    "photo": "str",
})
"""The arguments of `split_photo`."""

RemovePersonResourceArgs = TypedDict("RemovePersonResourceArgs", {
    "person": "Person",
})
"""The arguments of `remove_person_resource`."""

class Photo(Resource):
    """Photo — `tl.so.tilo.photos.Photo`."""
    __surface__ = "so.tilo.photos@c38aec041dfd1a75"
    rv4vs7su: str  # name
    r55t3rg3: str  # picture
    vwrtdas3: str  # thumb
    @property
    def _2i7i3as5(self) -> str:
        """edited (`2i7i3as5` on the wire)."""
        return self.fields["2i7i3as5"]

    @_2i7i3as5.setter
    def _2i7i3as5(self, value: str) -> None:
        self.edit("2i7i3as5", value=value)
    ktv6ci4q: int  # rotation
    yyfm6s7t: bool  # favorite
    wm3tr75p: str  # captured_at
    zz3fpxbt: str  # folder
    @property
    def _5bhxlhcf(self) -> list[str]:
        """tags (`5bhxlhcf` on the wire)."""
        return self.fields["5bhxlhcf"]

    @_5bhxlhcf.setter
    def _5bhxlhcf(self, value: list[str]) -> None:
        self.edit("5bhxlhcf", value=value)
    vlem53gy: list[str]  # people
    bgo4zgpq: str  # location
    kko7mkgg: float  # latitude
    @property
    def _4xox6lum(self) -> float:
        """longitude (`4xox6lum` on the wire)."""
        return self.fields["4xox6lum"]

    @_4xox6lum.setter
    def _4xox6lum(self, value: float) -> None:
        self.edit("4xox6lum", value=value)
    brtcqh34: str  # description
    p4pebna2: str  # recognized
    @property
    def _6esqywn4(self) -> list[str]:
        """objects (`6esqywn4` on the wire)."""
        return self.fields["6esqywn4"]

    @_6esqywn4.setter
    def _6esqywn4(self, value: list[str]) -> None:
        self.edit("6esqywn4", value=value)
    @property
    def _7rzyj24a(self) -> list[str]:
        """pets (`7rzyj24a` on the wire)."""
        return self.fields["7rzyj24a"]

    @_7rzyj24a.setter
    def _7rzyj24a(self, value: list[str]) -> None:
        self.edit("7rzyj24a", value=value)

    def toggle_favorite(self,
        *,
        favorite: bool | None = None,
    ) -> None:
        """Marks a photo as a favorite, or takes that back."""
        args: dict[str, Any] = {}
        if favorite is not None:
            args["favorite"] = favorite
        return self.action("toggle_favorite", **args)

    def toggle_favorite_many(self, items: list[ToggleFavoriteArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_favorite", items)

    def rotate_right(self) -> None:
        """Turns the photograph a quarter clockwise, as its person sees it: the window draws the original turned, and a save writes what it draws."""
        return self.action("rotate_right")

    def rotate_right_many(self, items: list[RotateRightArgs]) -> list[dict[str, Any]]:
        """Runs `rotate_right` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rotate_right", items)

    def rotate_left(self) -> None:
        """Turns the photograph a quarter anticlockwise."""
        return self.action("rotate_left")

    def rotate_left_many(self, items: list[RotateLeftArgs]) -> list[dict[str, Any]]:
        """Runs `rotate_left` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rotate_left", items)

    def reset_edits(self) -> None:
        """Puts the photograph back as it came, taking the person's own turns off it."""
        return self.action("reset_edits")

    def reset_edits_many(self, items: list[ResetEditsArgs]) -> list[dict[str, Any]]:
        """Runs `reset_edits` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset_edits", items)

    def describe_photo(self) -> None:
        """What a picture shows, in a sentence, as the device's image model reads it: the sentence the library's details show and search finds. The work is the device's own, asked of this session on the app's behalf, so the app never runs a model itself."""
        return self.action("describe_photo")

    def recognize_text(self) -> None:
        """The words in the photograph, read on this device and kept with the photo, so the library finds it by what it says. Needs no model: a device without one still reads its text."""
        return self.action("recognize_text")

    def recognize_text_many(self, items: list[RecognizeTextArgs]) -> list[dict[str, Any]]:
        """Runs `recognize_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("recognize_text", items)

    def copy_photo(self) -> None:
        """Copies the photograph to the person's clipboard as a picture, so it pastes into another app."""
        return self.action("copy_photo")

    def tag_photo(self) -> None:
        """The objects and pets the device's image model finds in a picture, added to what the photo already lists."""
        return self.action("tag_photo")

    def crop_photo(self,
        *,
        x: float,
        y: float,
        width: float,
        height: float,
    ) -> None:
        """Keeps the part of the photograph its person chose: `x`, `y`, `width` and `height` are fractions of the picture as the window shows it, so every member and device reads the crop."""
        return self.action("crop_photo", x=x, y=y, width=width, height=height)

    def crop_photo_many(self, items: list[CropPhotoArgs]) -> list[dict[str, Any]]:
        """Runs `crop_photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("crop_photo", items)

    def tag(self, *, t: str) -> None:
        """Gives a photo a tag."""
        return self.action("tag", t=t)

    def tag_many(self, items: list[TagArgs]) -> list[dict[str, Any]]:
        """Runs `tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("tag", items)

    def untag(self, *, t: str) -> None:
        """Takes a tag off a photo."""
        return self.action("untag", t=t)

    def untag_many(self, items: list[UntagArgs]) -> list[dict[str, Any]]:
        """Runs `untag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("untag", items)

    def share(self) -> str:
        """The link that reaches a photo's picture: the address of its bytes, which every device of the workspace reads from the content store."""
        return self.action("share")

    def set_place(self, *, at: list[float]) -> None:
        """Sets the place a photo was taken from where the person points on the map: the picker reports the point as its latitude then its longitude."""
        return self.action("set_place", at=at)

    def set_place_many(self, items: list[SetPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `set_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_place", items)

    def clear_place(self) -> None:
        """Takes the place off a photo."""
        return self.action("clear_place")

    def clear_place_many(self, items: list[ClearPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `clear_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_place", items)

    def export_photo(self) -> Any:
        """The original picture as a file, to save where the person says."""
        return self.action("export_photo")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"toggle_favorite", "toggle_favorite_many", "rotate_right", "rotate_right_many", "rotate_left", "rotate_left_many", "reset_edits", "reset_edits_many", "describe_photo", "recognize_text", "recognize_text_many", "copy_photo", "tag_photo", "crop_photo", "crop_photo_many", "tag", "tag_many", "untag", "untag_many", "share", "set_place", "set_place_many", "clear_place", "clear_place_many", "export_photo"})


class Album(Resource):
    """Album — `tl.so.tilo.photos.Album`."""
    __surface__ = "so.tilo.photos@c38aec041dfd1a75"
    unujaudz: str  # name
    k7fl2y27: list[str]  # photos

    def add_selected_to_album(self, *, ids: list[str]) -> None:
        """Puts each named photo in an album."""
        return self.action("add_selected_to_album", ids=ids)

    def add_selected_to_album_many(self, items: list[AddSelectedToAlbumArgs]) -> list[dict[str, Any]]:
        """Runs `add_selected_to_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_selected_to_album", items)

    def rename_album(self, *, to: str) -> None:
        """Renames an album."""
        return self.action("rename_album", to=to)

    def rename_album_many(self, items: list[RenameAlbumArgs]) -> list[dict[str, Any]]:
        """Runs `rename_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_album", items)

    def album_contains_toggle(self, *, p: Photo) -> None:
        """Puts a photo in an album, or takes it out of one when it is already in it."""
        return self.action("album_contains_toggle", p=p)

    def album_contains_toggle_many(self, items: list[AlbumContainsToggleArgs]) -> list[dict[str, Any]]:
        """Runs `album_contains_toggle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("album_contains_toggle", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_selected_to_album", "add_selected_to_album_many", "rename_album", "rename_album_many", "album_contains_toggle", "album_contains_toggle_many"})


class Person(Resource):
    """Person — `tl.so.tilo.photos.Person`."""
    __surface__ = "so.tilo.photos@c38aec041dfd1a75"
    segahwtp: str  # name
    vbzsnqg5: list[str]  # photos


class Library(Resource):
    """Library — `tl.so.tilo.photos.Library`."""
    __surface__ = "so.tilo.photos@c38aec041dfd1a75"
    moeuycny: list[Entry[Photo]]  # photos
    tqa4lyow: list[Entry[Album]]  # albums
    xr5mtp2m: list[Entry[Person]]  # people
    tp7e7pzd: int  # import_found
    @property
    def _6jdgyzo3(self) -> int:
        """import_done (`6jdgyzo3` on the wire)."""
        return self.fields["6jdgyzo3"]

    @_6jdgyzo3.setter
    def _6jdgyzo3(self, value: int) -> None:
        self.edit("6jdgyzo3", value=value)
    ybraeglq: list[str]  # import_issues

    def import_photo(self, *, file: Any) -> Photo:
        """Imports a photograph the person picks: the library keeps the address of its bytes, the name it came in with, what its own container says of it (when and where it was taken) and a small preview the grid draws. A picture whose bytes the window already holds reads at once."""
        return self.action("import_photo", file=file)

    def absorb_picture(self, *, file: Any) -> Photo:
        """Adds the picture a file holds to the library: the photo the same picture already made, brought up to what the file says now, or a new one. What the person set on a photo — its place, its tags — is theirs, so reading the file again never overwrites it."""
        return self.action("absorb_picture", file=file)

    def import_photo_meta(self,
        *,
        file: Any,
        captured_at: str | None = None,
        location: str | None = None,
    ) -> Photo:
        """Imports a photograph a caller already knows the capture time and place of (the camera's handoff). It opens no file kind of its own: the `opens` action above is what Files and the person's own file system hand a picture to."""
        args: dict[str, Any] = {}
        args["file"] = file
        if captured_at is not None:
            args["captured_at"] = captured_at
        if location is not None:
            args["location"] = location
        return self.action("import_photo_meta", **args)

    def import_dropped(self, *, files: list[Any]) -> None:
        """Imports the pictures a person drops on the window or picks a folder of: each picture file becomes a photo, with the folder it came in, and a file that is no picture is left alone and said so in the report. Picking the same folder again brings what the files say to the photos already there, rather than making a second copy of each."""
        return self.action("import_dropped", files=files)

    def import_dropped_many(self, items: list[ImportDroppedArgs]) -> list[dict[str, Any]]:
        """Runs `import_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_dropped", items)

    def dismiss_import(self) -> None:
        """Takes the last import's report off the window."""
        return self.action("dismiss_import")

    def dismiss_import_many(self, items: list[DismissImportArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_import", items)

    def favorite_selected(self, *, ids: list[str]) -> None:
        """Marks each named photo a favorite."""
        return self.action("favorite_selected", ids=ids)

    def favorite_selected_many(self, items: list[FavoriteSelectedArgs]) -> list[dict[str, Any]]:
        """Runs `favorite_selected` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("favorite_selected", items)

    def remove_selected(self, *, ids: list[str]) -> None:
        """Takes each named photo out of the library."""
        return self.action("remove_selected", ids=ids)

    def remove_selected_many(self, items: list[RemoveSelectedArgs]) -> list[dict[str, Any]]:
        """Runs `remove_selected` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_selected", items)

    # the app's `remove` action is the SDK's own: run it with `action("remove")`.
    def new_album(self,
        *,
        p: Photo | None = None,
    ) -> Album:
        """Starts an album, and puts the photo it is started from in it."""
        args: dict[str, Any] = {}
        if p is not None:
            args["p"] = p
        return self.action("new_album", **args)

    def remove_album(self, *, a: Album) -> None:
        """Deletes an album. Its photos stay in the library."""
        return self.action("remove_album", a=a)

    def remove_album_many(self, items: list[RemoveAlbumArgs]) -> list[dict[str, Any]]:
        """Runs `remove_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_album", items)

    def add_person(self,
        *,
        p: Photo,
        person: str,
    ) -> None:
        """Names a person on a photo, and puts the photo on that person when the library knows them."""
        return self.action("add_person", p=p, person=person)

    def add_person_many(self, items: list[AddPersonArgs]) -> list[dict[str, Any]]:
        """Runs `add_person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_person", items)

    def remove_person(self,
        *,
        p: Photo,
        person: str,
    ) -> None:
        """Takes a person off a photo, and the photo off that person."""
        return self.action("remove_person", p=p, person=person)

    def remove_person_many(self, items: list[RemovePersonArgs]) -> list[dict[str, Any]]:
        """Runs `remove_person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_person", items)

    def rename_person(self,
        *,
        person: Person,
        to: str,
    ) -> None:
        """Names a person: the photos they are on say the new name too."""
        return self.action("rename_person", person=person, to=to)

    def rename_person_many(self, items: list[RenamePersonArgs]) -> list[dict[str, Any]]:
        """Runs `rename_person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_person", items)

    def new_person(self,
        *,
        name: str,
        photos: list[str],
    ) -> Person:
        """Starts a person the library knows, with the photos it already has of them."""
        return self.action("new_person", name=name, photos=photos)

    def merge_people(self,
        *,
        from_: Person,
        into: Person,
    ) -> None:
        """Merges one person into another: their photos come together under the name kept, and the name merged away leaves every photo it was on."""
        args: dict[str, Any] = {}
        args["from"] = from_
        args["into"] = into
        return self.action("merge_people", **args)

    def merge_people_many(self, items: list[MergePeopleArgs]) -> list[dict[str, Any]]:
        """Runs `merge_people` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("merge_people", items)

    def split_photo(self,
        *,
        person: Person,
        photo: str,
    ) -> None:
        """Takes one photo off a person, and the person off that photo."""
        return self.action("split_photo", person=person, photo=photo)

    def split_photo_many(self, items: list[SplitPhotoArgs]) -> list[dict[str, Any]]:
        """Runs `split_photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("split_photo", items)

    def remove_person_resource(self, *, person: Person) -> None:
        """Forgets a person: their name leaves every photo they were on."""
        return self.action("remove_person_resource", person=person)

    def remove_person_resource_many(self, items: list[RemovePersonResourceArgs]) -> list[dict[str, Any]]:
        """Runs `remove_person_resource` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_person_resource", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"import_photo", "absorb_picture", "import_photo_meta", "import_dropped", "import_dropped_many", "dismiss_import", "dismiss_import_many", "favorite_selected", "favorite_selected_many", "remove_selected", "remove_selected_many", "new_album", "remove_album", "remove_album_many", "add_person", "add_person_many", "remove_person", "remove_person_many", "rename_person", "rename_person_many", "new_person", "merge_people", "merge_people_many", "split_photo", "split_photo_many", "remove_person_resource", "remove_person_resource_many"})


class Photos(Resource):
    """Photos — `tl.so.tilo.photos`."""
    __surface__ = "so.tilo.photos@c38aec041dfd1a75"

