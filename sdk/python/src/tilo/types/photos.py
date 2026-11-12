"""Types for Photos (`so.tilo.photos`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Library` is the app's `photos.library` at `photos/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict
from tilo import Resource, Run

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
    content: str

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

class Library(Resource):
    """Photos — `photos.library`."""
    photos: list[LibraryPhoto]
    albums: list[Album]

    def import_photo(self,
        *,
        image: OpenImage,
        captured_at: str | None = None,
        location: PhotoLocation | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["image"] = image
        if captured_at is not None:
            args["captured_at"] = captured_at
        if location is not None:
            args["location"] = location
        return self.action("import-photo", **args)

    def refresh_imported_metadata(self,
        *,
        photo: str,
        captured_at: str | None = None,
        location: PhotoLocation | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["photo"] = photo
        if captured_at is not None:
            args["captured_at"] = captured_at
        if location is not None:
            args["location"] = location
        return self.action("refresh-imported-metadata", **args)

    def remove_photo(self, *, photo: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-photo", photo=photo)

    def rotate_right(self, *, photo: str) -> Run | None:
        """Runs the app's action."""
        return self.action("rotate-right", photo=photo)

    def rotate_left(self, *, photo: str) -> Run | None:
        """Runs the app's action."""
        return self.action("rotate-left", photo=photo)

    def crop(self,
        *,
        photo: str,
        region: Rect01,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("crop", photo=photo, region=region)

    def merge_image_analysis(self,
        *,
        photo: str,
        analysis: ImageAnalysis,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("merge-image-analysis", photo=photo, analysis=analysis)

    def set_person_membership(self,
        *,
        person: str,
        photos: list[str],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-person-membership", person=person, photos=photos)

    def rename_person(self,
        *,
        person: str,
        to: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename-person", person=person, to=to)

    def remove_person(self, *, person: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-person", person=person)

    def remove_album(self, *, album: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-album", album=album)

    def make_thumbnails(self, *, photos: list[str]) -> None:
        """Runs the app's action."""
        return self.action("make-thumbnails", photos=photos)


