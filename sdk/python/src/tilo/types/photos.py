"""Types for Photos (`so.tilo.photos`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `photos/<resource>`, read with
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
    "Change",
    "Notice",
    "Photo",
    "Album",
    "Person",
    "Suggestion",
    "Library",
    "AlbumDraft",
    "Unread",
    "ChosenPicture",
    "ImportAsk",
    "ReadPicture",
    "ImportRecord",
    "JobStarted",
    "FaceScan",
    "Photos",
    "CheckPictureArgs",
    "ToggleFavoriteArgs",
    "RotateRightArgs",
    "RotateLeftArgs",
    "ResetEditsArgs",
    "DevelopArgs",
    "RecognizeTextArgs",
    "CropPhotoArgs",
    "TagArgs",
    "UntagArgs",
    "SetPlaceArgs",
    "ClearPlaceArgs",
    "AddSelectedToAlbumArgs",
    "RenameAlbumArgs",
    "AlbumContainsToggleArgs",
    "ImportFolderArgs",
    "StartImportArgs",
    "ImportedArgs",
    "RetryImportArgs",
    "ReindexArgs",
    "DismissImportArgs",
    "FavoriteSelectedArgs",
    "RemoveSelectedArgs",
    "EditSelectedArgs",
    "RemoveAlbumArgs",
    "StartAlbumDraftArgs",
    "AlbumDraftToggleArgs",
    "CancelAlbumDraftArgs",
    "SaveAlbumArgs",
    "DeleteAlbumDraftArgs",
    "DismissNoticeArgs",
    "AddPersonArgs",
    "RemovePersonArgs",
    "RenamePersonArgs",
    "SetRecognitionArgs",
    "PauseScanArgs",
    "DeleteFaceDataArgs",
    "ApproveSuggestionArgs",
    "MergeSuggestionArgs",
    "DismissSuggestionArgs",
    "MergePeopleArgs",
    "SplitPhotoArgs",
    "RemovePersonResourceArgs",
]

Change = Literal["keep", "clear", "set", "add", "remove"]
"""What an edit does to one field of every selected photo: keeps what each photo has, takes it off all of them, gives them all one value, or adds or takes off names (tags, people)."""

CheckPictureArgs = TypedDict("CheckPictureArgs", {
})
"""The arguments of `check_picture`."""

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

DevelopArgs = TypedDict("DevelopArgs", {
    "rotation": "int",
    "crop": "list[float]",
})
"""The arguments of `develop`."""

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

ImportFolderArgs = TypedDict("ImportFolderArgs", {
    "files": "list[Any]",
})
"""The arguments of `import_folder`."""

StartImportArgs = TypedDict("StartImportArgs", {
    "ask": "str",
    "found": "int",
})
"""The arguments of `start_import`."""

ImportedArgs = TypedDict("ImportedArgs", {
    "outcome": "str",
})
"""The arguments of `imported`."""

RetryImportArgs = TypedDict("RetryImportArgs", {
})
"""The arguments of `retry_import`."""

ReindexArgs = TypedDict("ReindexArgs", {
    "folder": NotRequired["str | None"],
})
"""The arguments of `reindex`."""

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

EditSelectedArgs = TypedDict("EditSelectedArgs", {
    "ids": "list[str]",
    "tags": NotRequired["Change | None"],
    "tag_names": NotRequired["list[str] | None"],
    "tags_off": NotRequired["list[str] | None"],
    "people": NotRequired["Change | None"],
    "person_names": NotRequired["list[str] | None"],
    "people_off": NotRequired["list[str] | None"],
    "folder": NotRequired["Change | None"],
    "folder_text": NotRequired["str | None"],
    "captured_at": NotRequired["Change | None"],
    "captured_text": NotRequired["str | None"],
    "description": NotRequired["Change | None"],
    "description_text": NotRequired["str | None"],
    "place": NotRequired["Change | None"],
    "place_name": NotRequired["str | None"],
    "latitude": NotRequired["float | None"],
    "longitude": NotRequired["float | None"],
})
"""The arguments of `edit_selected`."""

RemoveAlbumArgs = TypedDict("RemoveAlbumArgs", {
    "a": "Album",
})
"""The arguments of `remove_album`."""

StartAlbumDraftArgs = TypedDict("StartAlbumDraftArgs", {
    "id": "str",
})
"""The arguments of `start_album_draft`."""

AlbumDraftToggleArgs = TypedDict("AlbumDraftToggleArgs", {
    "p": "Photo",
})
"""The arguments of `album_draft_toggle`."""

CancelAlbumDraftArgs = TypedDict("CancelAlbumDraftArgs", {
})
"""The arguments of `cancel_album_draft`."""

SaveAlbumArgs = TypedDict("SaveAlbumArgs", {
})
"""The arguments of `save_album`."""

DeleteAlbumDraftArgs = TypedDict("DeleteAlbumDraftArgs", {
})
"""The arguments of `delete_album_draft`."""

DismissNoticeArgs = TypedDict("DismissNoticeArgs", {
})
"""The arguments of `dismiss_notice`."""

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

SetRecognitionArgs = TypedDict("SetRecognitionArgs", {
    "on": "bool",
})
"""The arguments of `set_recognition`."""

PauseScanArgs = TypedDict("PauseScanArgs", {
})
"""The arguments of `pause_scan`."""

DeleteFaceDataArgs = TypedDict("DeleteFaceDataArgs", {
})
"""The arguments of `delete_face_data`."""

ApproveSuggestionArgs = TypedDict("ApproveSuggestionArgs", {
    "s": "Suggestion",
    "name": NotRequired["str | None"],
})
"""The arguments of `approve_suggestion`."""

MergeSuggestionArgs = TypedDict("MergeSuggestionArgs", {
    "s": "Suggestion",
    "person": "Person",
})
"""The arguments of `merge_suggestion`."""

DismissSuggestionArgs = TypedDict("DismissSuggestionArgs", {
    "s": "Suggestion",
})
"""The arguments of `dismiss_suggestion`."""

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

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
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


class Photo(Resource):
    """Photo — `tl.so.tilo.photos.Photo`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
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
    dpfjny5e: list[float]  # crop
    nyatlpd5: str  # picture_state
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

    def check_picture(self) -> None:
        """Looks at where this device stands on a photo's picture: whether it holds the bytes yet, and whether what it holds is a picture it opens. A record reaches this device from another one before its picture's bytes do, so a photo says the picture is still arriving instead of drawing nothing; the window looks again while it waits."""
        return self.action("check_picture")

    def check_picture_many(self, items: list[CheckPictureArgs]) -> list[dict[str, Any]]:
        """Runs `check_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("check_picture", items)

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
        """Turns the photograph a quarter clockwise, as its person sees it."""
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
        """Puts the photograph back as it came, taking every edit off it."""
        return self.action("reset_edits")

    def reset_edits_many(self, items: list[ResetEditsArgs]) -> list[dict[str, Any]]:
        """Runs `reset_edits` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset_edits", items)

    def develop(self,
        *,
        rotation: int,
        crop: list[float],
    ) -> None:
        """Records an edit on the photograph and draws it: the device keeps the part the person chose, then turns it, and keeps the result in the content store. Nothing is written over the photograph, so every edit can be taken back and the person can see the picture as it came beside the one they made."""
        return self.action("develop", rotation=rotation, crop=crop)

    def develop_many(self, items: list[DevelopArgs]) -> list[dict[str, Any]]:
        """Runs `develop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("develop", items)

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
        """What the device's image model finds in a picture, added to what the photo already lists: the objects, and the pets apart from them."""
        return self.action("tag_photo")

    def analyze_photo(self) -> None:
        """What the photograph shows, as the device's model reads it in one go: the sentence search finds it by, the words in it, and the objects and pets it holds. It replaces what the photo said before, so analyzing again after the picture changed says what it shows now."""
        return self.action("analyze_photo")

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
    __actions__ = frozenset({"check_picture", "check_picture_many", "toggle_favorite", "toggle_favorite_many", "rotate_right", "rotate_right_many", "rotate_left", "rotate_left_many", "reset_edits", "reset_edits_many", "develop", "develop_many", "describe_photo", "recognize_text", "recognize_text_many", "copy_photo", "tag_photo", "analyze_photo", "crop_photo", "crop_photo_many", "tag", "tag_many", "untag", "untag_many", "share", "set_place", "set_place_many", "clear_place", "clear_place_many", "export_photo"})


class Album(Resource):
    """Album — `tl.so.tilo.photos.Album`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
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
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    segahwtp: str  # name
    vbzsnqg5: list[str]  # photos


class Suggestion(Resource):
    """Suggestion — `tl.so.tilo.photos.Suggestion`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    qracadw6: str  # name
    ehc6rrhm: list[str]  # photos
    @property
    def _2adnxzro(self) -> bool:
        """sent (`2adnxzro` on the wire)."""
        return self.fields["2adnxzro"]

    @_2adnxzro.setter
    def _2adnxzro(self, value: bool) -> None:
        self.edit("2adnxzro", value=value)


class Library(Resource):
    """Library — `tl.so.tilo.photos.Library`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    moeuycny: list[Entry[Photo]]  # photos
    tqa4lyow: list[Entry[Album]]  # albums
    xr5mtp2m: list[Entry[Person]]  # people
    r7k2giak: list[Entry[Suggestion]]  # suggestions
    tp7e7pzd: int  # import_found
    @property
    def _6jdgyzo3(self) -> int:
        """import_done (`6jdgyzo3` on the wire)."""
        return self.fields["6jdgyzo3"]

    @_6jdgyzo3.setter
    def _6jdgyzo3(self, value: int) -> None:
        self.edit("6jdgyzo3", value=value)
    ybraeglq: list[str]  # import_issues
    @property
    def _4gwd7453(self) -> str:
        """import_job (`4gwd7453` on the wire)."""
        return self.fields["4gwd7453"]

    @_4gwd7453.setter
    def _4gwd7453(self, value: str) -> None:
        self.edit("4gwd7453", value=value)
    ii2qmjt5: bool  # import_stopped
    xkkw7d65: list[Entry[Unread]]  # unread
    fypuuz3p: list[Entry[FaceScan]]  # face_scans
    jcilt5zf: str  # notice
    nyozkafm: list[Entry[AlbumDraft]]  # album_drafts

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

    def import_folder(self, *, files: list[Any]) -> None:
        """Imports the pictures a person drops on the window or picks a folder of, as a job on this device: it reads each picture, when and where it was taken and its preview, saying how far it has got as it goes, and can be stopped. A file that is no picture is left alone and said so. Picking the same folder again brings what the files say to the photos already there, rather than making a second copy of each."""
        return self.action("import_folder", files=files)

    def import_folder_many(self, items: list[ImportFolderArgs]) -> list[dict[str, Any]]:
        """Runs `import_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_folder", items)

    def start_import(self,
        *,
        ask: str,
        found: int,
    ) -> None:
        """Starts the device reading the pictures `ask` names, stopping an import still under way first. When the job ends, `imported` brings what it read into the library, as the one who started it."""
        return self.action("start_import", ask=ask, found=found)

    def start_import_many(self, items: list[StartImportArgs]) -> list[dict[str, Any]]:
        """Runs `start_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_import", items)

    def imported(self, *, outcome: str) -> None:
        """An import's end: each picture it read becomes a photo, or brings what it says to the photo the same picture already made, and what it could not read is kept to try again. What the person set on a photo (its time, its place) is theirs, so reading it again never overwrites it."""
        return self.action("imported", outcome=outcome)

    def imported_many(self, items: list[ImportedArgs]) -> list[dict[str, Any]]:
        """Runs `imported` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("imported", items)

    def stop_import(self) -> None:
        """Stops the import under way: it ends after the picture it is reading, and what it read so far joins the library."""
        return self.action("stop_import")

    def retry_import(self) -> None:
        """Tries again the pictures the last import could not read where trying again could help."""
        return self.action("retry_import")

    def retry_import_many(self, items: list[RetryImportArgs]) -> list[dict[str, Any]]:
        """Runs `retry_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retry_import", items)

    def reindex(self,
        *,
        folder: str | None = None,
    ) -> None:
        """Reads the photos of a folder again, or of the whole library when no folder is named: their previews, and when and where they were taken where nobody set it."""
        args: dict[str, Any] = {}
        if folder is not None:
            args["folder"] = folder
        return self.action("reindex", **args)

    def reindex_many(self, items: list[ReindexArgs]) -> list[dict[str, Any]]:
        """Runs `reindex` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reindex", items)

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

    def edit_selected(self,
        *,
        ids: list[str],
        tags: Change | None = None,
        tag_names: list[str] | None = None,
        tags_off: list[str] | None = None,
        people: Change | None = None,
        person_names: list[str] | None = None,
        people_off: list[str] | None = None,
        folder: Change | None = None,
        folder_text: str | None = None,
        captured_at: Change | None = None,
        captured_text: str | None = None,
        description: Change | None = None,
        description_text: str | None = None,
        place: Change | None = None,
        place_name: str | None = None,
        latitude: float | None = None,
        longitude: float | None = None,
    ) -> None:
        """Changes the details of every named photo as one change: a field left `keep` keeps what each photo has, `clear` takes it off all of them, `set` gives them all one value, and tags and people are added or taken off across the selection, so every photo keeps the rest of its own. The selection is names in a list, so a person's selection, an agent's and a program's are the same change, undone at once."""
        args: dict[str, Any] = {}
        args["ids"] = ids
        if tags is not None:
            args["tags"] = tags
        if tag_names is not None:
            args["tag_names"] = tag_names
        if tags_off is not None:
            args["tags_off"] = tags_off
        if people is not None:
            args["people"] = people
        if person_names is not None:
            args["person_names"] = person_names
        if people_off is not None:
            args["people_off"] = people_off
        if folder is not None:
            args["folder"] = folder
        if folder_text is not None:
            args["folder_text"] = folder_text
        if captured_at is not None:
            args["captured_at"] = captured_at
        if captured_text is not None:
            args["captured_text"] = captured_text
        if description is not None:
            args["description"] = description
        if description_text is not None:
            args["description_text"] = description_text
        if place is not None:
            args["place"] = place
        if place_name is not None:
            args["place_name"] = place_name
        if latitude is not None:
            args["latitude"] = latitude
        if longitude is not None:
            args["longitude"] = longitude
        return self.action("edit_selected", **args)

    def edit_selected_many(self, items: list[EditSelectedArgs]) -> list[dict[str, Any]]:
        """Runs `edit_selected` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_selected", items)

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

    def start_album_draft(self, *, id: str) -> None:
        """Opens an album draft: the album `id` names as it stands now, or an empty one to start. The person chooses its photos in the library and nothing about the album changes until they save the draft, so a change another member makes meanwhile is reviewed, not overwritten unseen."""
        return self.action("start_album_draft", id=id)

    def start_album_draft_many(self, items: list[StartAlbumDraftArgs]) -> list[dict[str, Any]]:
        """Runs `start_album_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_album_draft", items)

    def album_draft_toggle(self, *, p: Photo) -> None:
        """Chooses a photo for the album draft, or takes it out of the draft when it is already in it."""
        return self.action("album_draft_toggle", p=p)

    def album_draft_toggle_many(self, items: list[AlbumDraftToggleArgs]) -> list[dict[str, Any]]:
        """Runs `album_draft_toggle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("album_draft_toggle", items)

    def cancel_album_draft(self) -> None:
        """Closes the album draft, saving nothing."""
        return self.action("cancel_album_draft")

    def cancel_album_draft_many(self, items: list[CancelAlbumDraftArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_album_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_album_draft", items)

    def save_album(self) -> None:
        """Saves the album draft. A photo the draft names that the library no longer holds, or an album another member deleted while it was open, is a change made first by someone else: the draft stays as it is and says so, so they review it and save again. What the save does add and take away merges with a change another member made meanwhile."""
        return self.action("save_album")

    def save_album_many(self, items: list[SaveAlbumArgs]) -> list[dict[str, Any]]:
        """Runs `save_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save_album", items)

    def delete_album_draft(self) -> None:
        """Deletes the album the draft is of. Its photos stay in the library."""
        return self.action("delete_album_draft")

    def delete_album_draft_many(self, items: list[DeleteAlbumDraftArgs]) -> list[dict[str, Any]]:
        """Runs `delete_album_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_album_draft", items)

    def dismiss_notice(self) -> None:
        """Takes down what the library last had to say."""
        return self.action("dismiss_notice")

    def dismiss_notice_many(self, items: list[DismissNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_notice", items)

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

    def face_scan(self) -> FaceScan:
        """This person's face scan, started where they have none."""
        return self.action("face_scan")

    def set_recognition(self, *, on: bool) -> None:
        """Lets this device look for faces in the photos, or stops it: turning it off ends a scan under way, and keeps every name already given."""
        return self.action("set_recognition", on=on)

    def set_recognition_many(self, items: list[SetRecognitionArgs]) -> list[dict[str, Any]]:
        """Runs `set_recognition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_recognition", items)

    def scan_faces(self) -> None:
        """Starts scanning the photos on this device for faces, or goes on with a paused scan: the model reads twelve at a time, and the window takes the next twelve as each batch ends, until every photo nobody is named on was read. Each face it finds is a suggestion of the scanner's own."""
        return self.action("scan_faces")

    def scan_step(self) -> None:
        """One batch of a running scan: the next twelve photos it has not read."""
        return self.action("scan_step")

    def read_batch(self, *, scan: FaceScan) -> bool:
        """Reads the next twelve photos `scan` has not read and nobody is named on yet. Answers whether there were any to read."""
        return self.action("read_batch", scan=scan)

    def pause_scan(self) -> None:
        """Pauses the scan under way: scanning again goes on with the next photos."""
        return self.action("pause_scan")

    def pause_scan_many(self, items: list[PauseScanArgs]) -> list[dict[str, Any]]:
        """Runs `pause_scan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pause_scan", items)

    def delete_face_data(self) -> None:
        """Deletes the face data this person's scans made on this device: the faces nobody named yet and what the scan had read. The people already named are kept."""
        return self.action("delete_face_data")

    def delete_face_data_many(self, items: list[DeleteFaceDataArgs]) -> list[dict[str, Any]]:
        """Runs `delete_face_data` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_face_data", items)

    def approve_suggestion(self,
        *,
        s: Suggestion,
        name: str | None = None,
    ) -> None:
        """Names a face: its photos join the person with that name, and the suggestion is sent, so the workspace keeps it."""
        args: dict[str, Any] = {}
        args["s"] = s
        if name is not None:
            args["name"] = name
        return self.action("approve_suggestion", **args)

    def approve_suggestion_many(self, items: list[ApproveSuggestionArgs]) -> list[dict[str, Any]]:
        """Runs `approve_suggestion` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("approve_suggestion", items)

    def merge_suggestion(self,
        *,
        s: Suggestion,
        person: Person,
    ) -> None:
        """Puts a face's photos on a person the library already knows, and sends the suggestion."""
        return self.action("merge_suggestion", s=s, person=person)

    def merge_suggestion_many(self, items: list[MergeSuggestionArgs]) -> list[dict[str, Any]]:
        """Runs `merge_suggestion` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("merge_suggestion", items)

    def dismiss_suggestion(self, *, s: Suggestion) -> None:
        """Takes a suggestion away: the face is not offered again, since the photos are not rescanned into one while they are unnamed only by choice."""
        return self.action("dismiss_suggestion", s=s)

    def dismiss_suggestion_many(self, items: list[DismissSuggestionArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_suggestion` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_suggestion", items)

    def person_named(self, *, name: str) -> Person:
        """The person the library knows by `name`, started where it knows none."""
        return self.action("person_named", name=name)

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
    __actions__ = frozenset({"import_photo", "absorb_picture", "import_photo_meta", "import_folder", "import_folder_many", "start_import", "start_import_many", "imported", "imported_many", "stop_import", "retry_import", "retry_import_many", "reindex", "reindex_many", "dismiss_import", "dismiss_import_many", "favorite_selected", "favorite_selected_many", "remove_selected", "remove_selected_many", "edit_selected", "edit_selected_many", "new_album", "remove_album", "remove_album_many", "start_album_draft", "start_album_draft_many", "album_draft_toggle", "album_draft_toggle_many", "cancel_album_draft", "cancel_album_draft_many", "save_album", "save_album_many", "delete_album_draft", "delete_album_draft_many", "dismiss_notice", "dismiss_notice_many", "add_person", "add_person_many", "remove_person", "remove_person_many", "rename_person", "rename_person_many", "face_scan", "set_recognition", "set_recognition_many", "scan_faces", "scan_step", "read_batch", "pause_scan", "pause_scan_many", "delete_face_data", "delete_face_data_many", "approve_suggestion", "approve_suggestion_many", "merge_suggestion", "merge_suggestion_many", "dismiss_suggestion", "dismiss_suggestion_many", "person_named", "new_person", "merge_people", "merge_people_many", "split_photo", "split_photo_many", "remove_person_resource", "remove_person_resource_many"})


class AlbumDraft(Resource):
    """AlbumDraft — `tl.so.tilo.photos.AlbumDraft`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    @property
    def _53utp7nd(self) -> str:
        """album (`53utp7nd` on the wire)."""
        return self.fields["53utp7nd"]

    @_53utp7nd.setter
    def _53utp7nd(self, value: str) -> None:
        self.edit("53utp7nd", value=value)
    jaabesu7: str  # name
    l4infwly: list[str]  # photos


class Unread(Resource):
    """Unread — `tl.so.tilo.photos.Unread`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    @property
    def _6iu5enh3(self) -> str:
        """name (`6iu5enh3` on the wire)."""
        return self.fields["6iu5enh3"]

    @_6iu5enh3.setter
    def _6iu5enh3(self, value: str) -> None:
        self.edit("6iu5enh3", value=value)
    cpplykgh: str  # picture
    @property
    def _3iejvl3x(self) -> str:
        """path (`3iejvl3x` on the wire)."""
        return self.fields["3iejvl3x"]

    @_3iejvl3x.setter
    def _3iejvl3x(self, value: str) -> None:
        self.edit("3iejvl3x", value=value)
    vzrzob3d: str  # why
    dof27753: bool  # retry


class ChosenPicture(Resource):
    """ChosenPicture — `tl.so.tilo.photos.ChosenPicture`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    wjvddrc5: str  # name
    @property
    def _6agrr5bt(self) -> str:
        """url (`6agrr5bt` on the wire)."""
        return self.fields["6agrr5bt"]

    @_6agrr5bt.setter
    def _6agrr5bt(self, value: str) -> None:
        self.edit("6agrr5bt", value=value)
    e67vj6hs: str  # path


class ImportAsk(Resource):
    """ImportAsk — `tl.so.tilo.photos.ImportAsk`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    pgrmuslm: list[ChosenPicture]  # files


class ReadPicture(Resource):
    """ReadPicture — `tl.so.tilo.photos.ReadPicture`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    z7dxm5rp: str  # name
    bedul3kq: str  # picture
    v72wrl73: str  # thumb
    f56g7pyb: str  # folder
    sc6xvdzp: str  # captured_at
    ohsouzrl: float  # latitude
    ug6uhwwb: float  # longitude


class ImportRecord(Resource):
    """ImportRecord — `tl.so.tilo.photos.ImportRecord`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    k33gw4xu: str  # job
    hbuvlsle: str  # state
    l5ism53v: int  # found
    nbqjpo64: int  # done
    jrp3muui: list[ReadPicture]  # photos
    affxqykm: list[Unread]  # issues


class JobStarted(Resource):
    """JobStarted — `tl.so.tilo.photos.JobStarted`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    cgbfeocd: str  # job


class FaceScan(Resource):
    """FaceScan — `tl.so.tilo.photos.FaceScan`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"
    dejwr76b: bool  # on
    dcarwzkc: str  # state
    quetfngb: list[str]  # scanned


class Photos(Resource):
    """Photos — `tl.so.tilo.photos`."""
    __surface__ = "so.tilo.photos@6619e9e267c54f4a"

