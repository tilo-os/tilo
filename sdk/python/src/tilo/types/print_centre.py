"""Types for Print Centre (`so.tilo.print-centre`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `print-centre/<resource>`, read with
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
    "Notice",
    "Defaults",
    "ScannedPage",
    "ScanSession",
    "PrintDocument",
    "Choice",
    "PrinterOptions",
    "PrintCentre",
    "OptionsAsk",
    "ScanDetail",
    "OcrAsk",
    "OcrAnswer",
    "PdfPage",
    "PagesAsk",
    "UrlAnswer",
    "PrintAsk",
    "ScanAsk",
    "TiloPrintCentre",
    "TurnPageArgs",
    "RecognizeArgs",
    "ScanEndedArgs",
    "PrintEndedArgs",
    "AskOptionsArgs",
    "AbsorbDocumentArgs",
    "PrintFileArgs",
    "ClearShowingArgs",
    "SendToPrinterArgs",
    "ScanPageArgs",
    "CropPageArgs",
    "RemovePageArgs",
    "NewScannedDocumentArgs",
    "MovePageArgs",
    "ChoosePrinterArgs",
    "SetTwoSidedArgs",
    "SetPrintOptionArgs",
    "SetResolutionArgs",
    "SetScanColourArgs",
    "SetScanSourceArgs",
    "SetOcrLanguageArgs",
    "SetScannerArgs",
]

TurnPageArgs = TypedDict("TurnPageArgs", {
})
"""The arguments of `turn_page`."""

RecognizeArgs = TypedDict("RecognizeArgs", {
})
"""The arguments of `recognize`."""

ScanEndedArgs = TypedDict("ScanEndedArgs", {
    "j": "Any",
})
"""The arguments of `scan_ended`."""

PrintEndedArgs = TypedDict("PrintEndedArgs", {
    "j": "Any",
})
"""The arguments of `print_ended`."""

AskOptionsArgs = TypedDict("AskOptionsArgs", {
})
"""The arguments of `ask_options`."""

AbsorbDocumentArgs = TypedDict("AbsorbDocumentArgs", {
    "file": "Any",
})
"""The arguments of `absorb_document`."""

PrintFileArgs = TypedDict("PrintFileArgs", {
    "file": "Any",
})
"""The arguments of `print_file`."""

ClearShowingArgs = TypedDict("ClearShowingArgs", {
})
"""The arguments of `clear_showing`."""

SendToPrinterArgs = TypedDict("SendToPrinterArgs", {
})
"""The arguments of `send_to_printer`."""

ScanPageArgs = TypedDict("ScanPageArgs", {
})
"""The arguments of `scan_page`."""

CropPageArgs = TypedDict("CropPageArgs", {
    "page": "ScannedPage",
    "left": "float",
    "top": "float",
    "right": "float",
    "bottom": "float",
})
"""The arguments of `crop_page`."""

RemovePageArgs = TypedDict("RemovePageArgs", {
    "page": "ScannedPage",
})
"""The arguments of `remove_page`."""

NewScannedDocumentArgs = TypedDict("NewScannedDocumentArgs", {
})
"""The arguments of `new_scanned_document`."""

MovePageArgs = TypedDict("MovePageArgs", {
    "page": "ScannedPage",
    "order": "int",
    "swapped": NotRequired["int | None"],
})
"""The arguments of `move_page`."""

ChoosePrinterArgs = TypedDict("ChoosePrinterArgs", {
    "queue": "str",
})
"""The arguments of `choose_printer`."""

SetTwoSidedArgs = TypedDict("SetTwoSidedArgs", {
    "two_sided": "bool",
})
"""The arguments of `set_two_sided`."""

SetPrintOptionArgs = TypedDict("SetPrintOptionArgs", {
    "option": "str",
    "value": "str",
})
"""The arguments of `set_print_option`."""

SetResolutionArgs = TypedDict("SetResolutionArgs", {
    "dpi": "int",
})
"""The arguments of `set_resolution`."""

SetScanColourArgs = TypedDict("SetScanColourArgs", {
    "colour": "str",
})
"""The arguments of `set_scan_colour`."""

SetScanSourceArgs = TypedDict("SetScanSourceArgs", {
    "source": "str",
})
"""The arguments of `set_scan_source`."""

SetOcrLanguageArgs = TypedDict("SetOcrLanguageArgs", {
    "language": "str",
})
"""The arguments of `set_ocr_language`."""

SetScannerArgs = TypedDict("SetScannerArgs", {
    "device_name": "str",
})
"""The arguments of `set_scanner`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
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


class Defaults(Resource):
    """Defaults — `tl.so.tilo.print-centre.Defaults`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    coq3zrug: bool  # two_sided
    @property
    def _26zbe323(self) -> int:
        """scan_resolution (`26zbe323` on the wire)."""
        return self.fields["26zbe323"]

    @_26zbe323.setter
    def _26zbe323(self, value: int) -> None:
        self.edit("26zbe323", value=value)
    wsba2l5p: str  # scan_colour
    vzgtuxuj: str  # scan_source
    warqkuxs: str  # ocr_language


class ScannedPage(Resource):
    """ScannedPage — `tl.so.tilo.print-centre.ScannedPage`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    zphe26i4: Any  # file
    @property
    def _67hjm6gx(self) -> str:
        """text (`67hjm6gx` on the wire)."""
        return self.fields["67hjm6gx"]

    @_67hjm6gx.setter
    def _67hjm6gx(self, value: str) -> None:
        self.edit("67hjm6gx", value=value)
    t2im62jd: int  # order

    def turn_page(self) -> None:
        """Turns a scanned page a quarter clockwise."""
        return self.action("turn_page")

    def turn_page_many(self, items: list[TurnPageArgs]) -> list[dict[str, Any]]:
        """Runs `turn_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("turn_page", items)

    def recognize(self) -> None:
        """Reads the text off a scanned page, in the person's language."""
        return self.action("recognize")

    def recognize_many(self, items: list[RecognizeArgs]) -> list[dict[str, Any]]:
        """Runs `recognize` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("recognize", items)

    def copy_text(self) -> None:
        """Copies a scanned page's text."""
        return self.action("copy_text")

    def page_to_save(self) -> Any:
        """The scanned page as a file to save."""
        return self.action("page_to_save")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"turn_page", "turn_page_many", "recognize", "recognize_many", "copy_text", "page_to_save"})


class ScanSession(Resource):
    """ScanSession — `tl.so.tilo.print-centre.ScanSession`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    @property
    def _3kybq73d(self) -> str:
        """scanner (`3kybq73d` on the wire)."""
        return self.fields["3kybq73d"]

    @_3kybq73d.setter
    def _3kybq73d(self, value: str) -> None:
        self.edit("3kybq73d", value=value)
    pva6zctd: list[Entry[ScannedPage]]  # pages
    wv7xzi7n: str  # job
    t6j77lzd: str  # failed

    def scan_ended(self, *, j: Any) -> None:
        """The scan ended; a page that arrived is added to the document."""
        return self.action("scan_ended", j=j)

    def scan_ended_many(self, items: list[ScanEndedArgs]) -> list[dict[str, Any]]:
        """Runs `scan_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("scan_ended", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"scan_ended", "scan_ended_many"})


class PrintDocument(Resource):
    """PrintDocument — `tl.so.tilo.print-centre.PrintDocument`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    lurulxdl: Any | None  # file
    nfdtbdvt: Any | None  # preview
    hkdu56wa: str  # name
    @property
    def _7ec7kpow(self) -> str:
        """queue (`7ec7kpow` on the wire)."""
        return self.fields["7ec7kpow"]

    @_7ec7kpow.setter
    def _7ec7kpow(self, value: str) -> None:
        self.edit("7ec7kpow", value=value)
    @property
    def _2ts7f4kz(self) -> float:
        """copies (`2ts7f4kz` on the wire)."""
        return self.fields["2ts7f4kz"]

    @_2ts7f4kz.setter
    def _2ts7f4kz(self, value: float) -> None:
        self.edit("2ts7f4kz", value=value)
    uiqz5mt6: str  # pages
    @property
    def _27dapuxo(self) -> bool:
        """two_sided (`27dapuxo` on the wire)."""
        return self.fields["27dapuxo"]

    @_27dapuxo.setter
    def _27dapuxo(self, value: bool) -> None:
        self.edit("27dapuxo", value=value)
    baptgoau: str  # page_size
    ngceod5n: str  # colour
    bgty3reb: str  # duplex
    zqg7u6bz: str  # orientation
    dzhipxmg: str  # job
    so7pxc5y: str  # failed
    @property
    def _4ngifmut(self) -> str:
        """trouble (`4ngifmut` on the wire)."""
        return self.fields["4ngifmut"]

    @_4ngifmut.setter
    def _4ngifmut(self, value: str) -> None:
        self.edit("4ngifmut", value=value)

    def print_ended(self, *, j: Any) -> None:
        """The print ended, printed or failed; it says which."""
        return self.action("print_ended", j=j)

    def print_ended_many(self, items: list[PrintEndedArgs]) -> list[dict[str, Any]]:
        """Runs `print_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("print_ended", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"print_ended", "print_ended_many"})


class Choice(Resource):
    """Choice — `tl.so.tilo.print-centre.Choice`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    ybptuzdv: str  # key
    n2rjd6ws: str  # label
    q5qiqhve: bool  # default


class PrinterOptions(Resource):
    """PrinterOptions — `tl.so.tilo.print-centre.PrinterOptions`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    nov3upcs: list[Choice]  # page_sizes
    pvufqhii: list[Choice]  # colour_modes
    boxhaco3: list[Choice]  # duplex_modes
    f6npg6br: list[Choice]  # orientations


class PrintCentre(Resource):
    """PrintCentre — `tl.so.tilo.print-centre.PrintCentre`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    kqm47afv: Defaults  # defaults
    lz7ue3si: list[Entry[PrintDocument]]  # documents
    iysceybl: list[Entry[ScanSession]]  # scans
    qxaoly4s: PrinterOptions  # options
    juk2pzrk: str  # showing

    def document_of(self) -> PrintDocument:
        """The person's document being printed, made the first time it is wanted."""
        return self.action("document_of")

    def scan_of(self) -> ScanSession:
        """The person's scanned document, made the first time it is wanted."""
        return self.action("scan_of")

    def ask_options(self) -> None:
        """Asks the engine what the chosen printer reports, so the form offers only what it can do."""
        return self.action("ask_options")

    def ask_options_many(self, items: list[AskOptionsArgs]) -> list[dict[str, Any]]:
        """Runs `ask_options` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ask_options", items)

    def absorb_document(self, *, file: Any) -> None:
        """Takes the file the person chose as what will be printed."""
        return self.action("absorb_document", file=file)

    def absorb_document_many(self, items: list[AbsorbDocumentArgs]) -> list[dict[str, Any]]:
        """Runs `absorb_document` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("absorb_document", items)

    def print_file(self, *, file: Any) -> None:
        """Takes a document another app hands over as what this window prints, and shows the print form so the person chooses the printer and how it prints: the one action an app calls to print a file it made. The file's bytes are the workspace's content, so the app that hands it over never needs this window's own files."""
        return self.action("print_file", file=file)

    def print_file_many(self, items: list[PrintFileArgs]) -> list[dict[str, Any]]:
        """Runs `print_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("print_file", items)

    def clear_showing(self) -> None:
        """Leaves the place another app asked this window to show, so the person's own choice stands."""
        return self.action("clear_showing")

    def clear_showing_many(self, items: list[ClearShowingArgs]) -> list[dict[str, Any]]:
        """Runs `clear_showing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_showing", items)

    def send_to_printer(self) -> None:
        """Prints the chosen document on the chosen printer, with the form's settings."""
        return self.action("send_to_printer")

    def send_to_printer_many(self, items: list[SendToPrinterArgs]) -> list[dict[str, Any]]:
        """Runs `send_to_printer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("send_to_printer", items)

    def scan_page(self) -> None:
        """Scans one page with the chosen scanner and the defaults."""
        return self.action("scan_page")

    def scan_page_many(self, items: list[ScanPageArgs]) -> list[dict[str, Any]]:
        """Runs `scan_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("scan_page", items)

    def crop_page(self,
        *,
        page: ScannedPage,
        left: float,
        top: float,
        right: float,
        bottom: float,
    ) -> None:
        """Crops a scanned page to the part the person kept, in fractions of the picture a viewer shows. The text read off the whole page no longer describes it, so it is cleared."""
        return self.action("crop_page", page=page, left=left, top=top, right=right, bottom=bottom)

    def crop_page_many(self, items: list[CropPageArgs]) -> list[dict[str, Any]]:
        """Runs `crop_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("crop_page", items)

    def remove_page(self, *, page: ScannedPage) -> None:
        """Throws a scanned page away."""
        return self.action("remove_page", page=page)

    def remove_page_many(self, items: list[RemovePageArgs]) -> list[dict[str, Any]]:
        """Runs `remove_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_page", items)

    def new_scanned_document(self) -> None:
        """Starts a new scanned document: the pages it held go. The change is the person's own and is taken back from history; it is refused while a page is still being scanned, so a scan in flight is never thrown away."""
        return self.action("new_scanned_document")

    def new_scanned_document_many(self, items: list[NewScannedDocumentArgs]) -> list[dict[str, Any]]:
        """Runs `new_scanned_document` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new_scanned_document", items)

    def move_page(self,
        *,
        page: ScannedPage,
        order: int,
        swapped: int | None = None,
    ) -> None:
        """Moves a scanned page to `order`, and the page that stood there to where it was."""
        args: dict[str, Any] = {}
        args["page"] = page
        args["order"] = order
        if swapped is not None:
            args["swapped"] = swapped
        return self.action("move_page", **args)

    def move_page_many(self, items: list[MovePageArgs]) -> list[dict[str, Any]]:
        """Runs `move_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_page", items)

    def document_to_save(self) -> Any:
        """The scanned document as one searchable PDF, to save."""
        return self.action("document_to_save")

    def choose_printer(self, *, queue: str) -> None:
        """Chooses a printer, and offers what it reports."""
        return self.action("choose_printer", queue=queue)

    def choose_printer_many(self, items: list[ChoosePrinterArgs]) -> list[dict[str, Any]]:
        """Runs `choose_printer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose_printer", items)

    def set_two_sided(self, *, two_sided: bool) -> None:
        """Prints on both sides of the paper, or one."""
        return self.action("set_two_sided", two_sided=two_sided)

    def set_two_sided_many(self, items: list[SetTwoSidedArgs]) -> list[dict[str, Any]]:
        """Runs `set_two_sided` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_two_sided", items)

    def set_print_option(self,
        *,
        option: str,
        value: str,
    ) -> None:
        """One of the printer's own options."""
        return self.action("set_print_option", option=option, value=value)

    def set_print_option_many(self, items: list[SetPrintOptionArgs]) -> list[dict[str, Any]]:
        """Runs `set_print_option` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_print_option", items)

    def set_resolution(self, *, dpi: int) -> None:
        """How finely to scan."""
        return self.action("set_resolution", dpi=dpi)

    def set_resolution_many(self, items: list[SetResolutionArgs]) -> list[dict[str, Any]]:
        """Runs `set_resolution` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_resolution", items)

    def set_scan_colour(self, *, colour: str) -> None:
        """How to scan: in colour, grayscale or line art."""
        return self.action("set_scan_colour", colour=colour)

    def set_scan_colour_many(self, items: list[SetScanColourArgs]) -> list[dict[str, Any]]:
        """Runs `set_scan_colour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scan_colour", items)

    def set_scan_source(self, *, source: str) -> None:
        """Where to scan from."""
        return self.action("set_scan_source", source=source)

    def set_scan_source_many(self, items: list[SetScanSourceArgs]) -> list[dict[str, Any]]:
        """Runs `set_scan_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scan_source", items)

    def set_ocr_language(self, *, language: str) -> None:
        """The language text is recognized in."""
        return self.action("set_ocr_language", language=language)

    def set_ocr_language_many(self, items: list[SetOcrLanguageArgs]) -> list[dict[str, Any]]:
        """Runs `set_ocr_language` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_ocr_language", items)

    def set_scanner(self, *, device_name: str) -> None:
        """Runs the app's action."""
        return self.action("set_scanner", device_name=device_name)

    def set_scanner_many(self, items: list[SetScannerArgs]) -> list[dict[str, Any]]:
        """Runs `set_scanner` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scanner", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"document_of", "scan_of", "ask_options", "ask_options_many", "absorb_document", "absorb_document_many", "print_file", "print_file_many", "clear_showing", "clear_showing_many", "send_to_printer", "send_to_printer_many", "scan_page", "scan_page_many", "crop_page", "crop_page_many", "remove_page", "remove_page_many", "new_scanned_document", "new_scanned_document_many", "move_page", "move_page_many", "document_to_save", "choose_printer", "choose_printer_many", "set_two_sided", "set_two_sided_many", "set_print_option", "set_print_option_many", "set_resolution", "set_resolution_many", "set_scan_colour", "set_scan_colour_many", "set_scan_source", "set_scan_source_many", "set_ocr_language", "set_ocr_language_many", "set_scanner", "set_scanner_many"})


class OptionsAsk(Resource):
    """OptionsAsk — `tl.so.tilo.print-centre.OptionsAsk`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    aun3edzr: str  # queue


class ScanDetail(Resource):
    """ScanDetail — `tl.so.tilo.print-centre.ScanDetail`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    jlmf7wws: str  # url


class OcrAsk(Resource):
    """OcrAsk — `tl.so.tilo.print-centre.OcrAsk`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    vuwh5pv6: str  # url
    @property
    def _56cimtrv(self) -> str:
        """language (`56cimtrv` on the wire)."""
        return self.fields["56cimtrv"]

    @_56cimtrv.setter
    def _56cimtrv(self, value: str) -> None:
        self.edit("56cimtrv", value=value)


class OcrAnswer(Resource):
    """OcrAnswer — `tl.so.tilo.print-centre.OcrAnswer`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    qegnvsxr: str  # text


class PdfPage(Resource):
    """PdfPage — `tl.so.tilo.print-centre.PdfPage`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    qxzi756t: str  # url
    mytppaxs: str  # text


class PagesAsk(Resource):
    """PagesAsk — `tl.so.tilo.print-centre.PagesAsk`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    omod5rbh: list[PdfPage]  # pages
    u566k7ni: str  # name


class UrlAnswer(Resource):
    """UrlAnswer — `tl.so.tilo.print-centre.UrlAnswer`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    y2ejci4l: str  # url


class PrintAsk(Resource):
    """PrintAsk — `tl.so.tilo.print-centre.PrintAsk`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    fk3jkxxv: str  # queue
    tddc4c3j: str  # url
    dkvqbxny: str  # name
    @property
    def _4ryo577f(self) -> int:
        """copies (`4ryo577f` on the wire)."""
        return self.fields["4ryo577f"]

    @_4ryo577f.setter
    def _4ryo577f(self, value: int) -> None:
        self.edit("4ryo577f", value=value)
    g6s27sdx: str  # pages
    @property
    def _7tzzarzm(self) -> bool:
        """two_sided (`7tzzarzm` on the wire)."""
        return self.fields["7tzzarzm"]

    @_7tzzarzm.setter
    def _7tzzarzm(self, value: bool) -> None:
        self.edit("7tzzarzm", value=value)
    akngsdrz: str  # page_size
    jq74dcfy: str  # colour
    oef7pq2y: str  # duplex
    frurca4k: str  # orientation


class ScanAsk(Resource):
    """ScanAsk — `tl.so.tilo.print-centre.ScanAsk`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    vilpbf2g: str  # scanner
    onyv6rml: int  # resolution
    dvoh6l75: str  # colour
    @property
    def _5mr6kzrm(self) -> str:
        """source (`5mr6kzrm` on the wire)."""
        return self.fields["5mr6kzrm"]

    @_5mr6kzrm.setter
    def _5mr6kzrm(self, value: str) -> None:
        self.edit("5mr6kzrm", value=value)
    @property
    def _72c3cb6z(self) -> str:
        """name (`72c3cb6z` on the wire)."""
        return self.fields["72c3cb6z"]

    @_72c3cb6z.setter
    def _72c3cb6z(self, value: str) -> None:
        self.edit("72c3cb6z", value=value)


class TiloPrintCentre(Resource):
    """Print Centre — `tl.so.tilo.print-centre`."""
    __surface__ = "so.tilo.print-centre@c0a891ca54f0df90"
    def preview_of_file(self, *, file: Any) -> Any | None:
        """The first page's picture, for the print form: a PDF's first page, or the picture itself."""
        return self.action("preview_of_file", file=file)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"preview_of_file"})


