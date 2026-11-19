"""Types for Photos (`so.tilo.photos`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `photos.library` at `photos/<resource>`, read with
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
    "Album",
    "AnalysisProvenance",
    "Edits",
    "ImageAnalysis",
    "LibraryPhoto",
    "LocationProvenance",
    "OpenImage",
    "PhotoLocation",
    "Rect01",
    "Library",
    "ImportPhotoArgs",
    "RefreshImportedMetadataArgs",
    "RemovePhotoArgs",
    "RotateRightArgs",
    "RotateLeftArgs",
    "CropArgs",
    "MergeImageAnalysisArgs",
    "SetPersonMembershipArgs",
    "RenamePersonArgs",
    "RemovePersonArgs",
    "RemoveAlbumArgs",
]

class Album(TypedDict):
    id: str
    name: str
    photo_ids: list[str]

class AnalysisProvenance(TypedDict):
    source: str
    engine: str
    model_id: str
    model_revision: str
    language: str
    content: str
    scope: str

class Edits(TypedDict):
    crop: Any
    rotate_quarters: int

class ImageAnalysis(TypedDict):
    ocr_text: str
    description: str
    objects: list[str]
    pets: list[str]
    provenance: AnalysisProvenance
    ocr_provenance: AnalysisProvenance | None
    vision_provenance: AnalysisProvenance | None

class LibraryPhoto(TypedDict):
    id: str
    name: str
    image: OpenImage
    favorite: bool
    folder: str | None
    people: list[str]
    tags: list[str]
    captured_at: str | None
    location: Any | None
    ocr: Any | None
    vision: Any | None
    edits: Edits

class LocationProvenance(TypedDict):
    source: str
    scope: str

class OpenImage(TypedDict):
    name: str
    width: int
    height: int
    content: Any

class PhotoLocation(TypedDict):
    latitude: float
    longitude: float
    label: str | None
    provenance: LocationProvenance

class Rect01(TypedDict):
    x: float
    y: float
    w: float
    h: float

ImportPhotoArgs = TypedDict("ImportPhotoArgs", {
    "image": "OpenImage",
    "captured_at": NotRequired["str | None"],
    "location": NotRequired["PhotoLocation | None"],
})
"""The arguments of `import-photo`."""

RefreshImportedMetadataArgs = TypedDict("RefreshImportedMetadataArgs", {
    "photo": "str",
    "captured_at": NotRequired["str | None"],
    "location": NotRequired["PhotoLocation | None"],
})
"""The arguments of `refresh-imported-metadata`."""

RemovePhotoArgs = TypedDict("RemovePhotoArgs", {
    "photo": "str",
})
"""The arguments of `remove-photo`."""

RotateRightArgs = TypedDict("RotateRightArgs", {
    "photo": "str",
})
"""The arguments of `rotate-right`."""

RotateLeftArgs = TypedDict("RotateLeftArgs", {
    "photo": "str",
})
"""The arguments of `rotate-left`."""

CropArgs = TypedDict("CropArgs", {
    "photo": "str",
    "region": "Rect01",
})
"""The arguments of `crop`."""

MergeImageAnalysisArgs = TypedDict("MergeImageAnalysisArgs", {
    "photo": "str",
    "analysis": "ImageAnalysis",
})
"""The arguments of `merge-image-analysis`."""

SetPersonMembershipArgs = TypedDict("SetPersonMembershipArgs", {
    "person": "str",
    "photos": "list[str]",
})
"""The arguments of `set-person-membership`."""

RenamePersonArgs = TypedDict("RenamePersonArgs", {
    "person": "str",
    "to": "str",
})
"""The arguments of `rename-person`."""

RemovePersonArgs = TypedDict("RemovePersonArgs", {
    "person": "str",
})
"""The arguments of `remove-person`."""

RemoveAlbumArgs = TypedDict("RemoveAlbumArgs", {
    "album": "str",
})
"""The arguments of `remove-album`."""

class Library(Resource):
    """Library — `photos.library`."""
    __surface__ = "so.tilo.photos@733ec0557fdbf49c"
    photos: list[Entry[LibraryPhoto]]
    albums: list[Entry[Album]]

    def import_photo(self,
        *,
        image: OpenImage,
        captured_at: str | None = None,
        location: PhotoLocation | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["image"] = image
        if captured_at is not None:
            args["captured_at"] = captured_at
        if location is not None:
            args["location"] = location
        return self.action("import-photo", **args)

    def import_photo_many(self, items: list[ImportPhotoArgs]) -> list[dict[str, Any]]:
        """Runs `import-photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-photo", items)

    def refresh_imported_metadata(self,
        *,
        photo: str,
        captured_at: str | None = None,
        location: PhotoLocation | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["photo"] = photo
        if captured_at is not None:
            args["captured_at"] = captured_at
        if location is not None:
            args["location"] = location
        return self.action("refresh-imported-metadata", **args)

    def refresh_imported_metadata_many(self, items: list[RefreshImportedMetadataArgs]) -> list[dict[str, Any]]:
        """Runs `refresh-imported-metadata` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("refresh-imported-metadata", items)

    def remove_photo(self, *, photo: str) -> None:
        """Runs the app's action."""
        return self.action("remove-photo", photo=photo)

    def remove_photo_many(self, items: list[RemovePhotoArgs]) -> list[dict[str, Any]]:
        """Runs `remove-photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-photo", items)

    def rotate_right(self, *, photo: str) -> None:
        """Runs the app's action."""
        return self.action("rotate-right", photo=photo)

    def rotate_right_many(self, items: list[RotateRightArgs]) -> list[dict[str, Any]]:
        """Runs `rotate-right` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rotate-right", items)

    def rotate_left(self, *, photo: str) -> None:
        """Runs the app's action."""
        return self.action("rotate-left", photo=photo)

    def rotate_left_many(self, items: list[RotateLeftArgs]) -> list[dict[str, Any]]:
        """Runs `rotate-left` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rotate-left", items)

    def crop(self,
        *,
        photo: str,
        region: Rect01,
    ) -> None:
        """Runs the app's action."""
        return self.action("crop", photo=photo, region=region)

    def crop_many(self, items: list[CropArgs]) -> list[dict[str, Any]]:
        """Runs `crop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("crop", items)

    def merge_image_analysis(self,
        *,
        photo: str,
        analysis: ImageAnalysis,
    ) -> None:
        """Runs the app's action."""
        return self.action("merge-image-analysis", photo=photo, analysis=analysis)

    def merge_image_analysis_many(self, items: list[MergeImageAnalysisArgs]) -> list[dict[str, Any]]:
        """Runs `merge-image-analysis` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("merge-image-analysis", items)

    def set_person_membership(self,
        *,
        person: str,
        photos: list[str],
    ) -> None:
        """Runs the app's action."""
        return self.action("set-person-membership", person=person, photos=photos)

    def set_person_membership_many(self, items: list[SetPersonMembershipArgs]) -> list[dict[str, Any]]:
        """Runs `set-person-membership` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-person-membership", items)

    def rename_person(self,
        *,
        person: str,
        to: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename-person", person=person, to=to)

    def rename_person_many(self, items: list[RenamePersonArgs]) -> list[dict[str, Any]]:
        """Runs `rename-person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-person", items)

    def remove_person(self, *, person: str) -> None:
        """Runs the app's action."""
        return self.action("remove-person", person=person)

    def remove_person_many(self, items: list[RemovePersonArgs]) -> list[dict[str, Any]]:
        """Runs `remove-person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-person", items)

    def remove_album(self, *, album: str) -> None:
        """Runs the app's action."""
        return self.action("remove-album", album=album)

    def remove_album_many(self, items: list[RemoveAlbumArgs]) -> list[dict[str, Any]]:
        """Runs `remove-album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-album", items)

    def make_thumbnails(self, *, photos: list[Any]) -> None:
        """Runs the app's action."""
        return self.action("make-thumbnails", photos=photos)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"import_photo", "import_photo_many", "refresh_imported_metadata", "refresh_imported_metadata_many", "remove_photo", "remove_photo_many", "rotate_right", "rotate_right_many", "rotate_left", "rotate_left_many", "crop", "crop_many", "merge_image_analysis", "merge_image_analysis_many", "set_person_membership", "set_person_membership_many", "rename_person", "rename_person_many", "remove_person", "remove_person_many", "remove_album", "remove_album_many", "make_thumbnails"})


