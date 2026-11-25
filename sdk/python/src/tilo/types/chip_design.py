"""Types for Chip Design (`so.tilo.chip-design`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `chip-design/<resource>`, read with
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
    "OperatorKind",
    "CacheKind",
    "CheckResult",
    "Comparison",
    "ObservationKind",
    "Outcome",
    "Orientation",
    "RunResult",
    "DependencyKind",
    "StageKind",
    "Notice",
    "Port",
    "Operator",
    "Connection",
    "Assumptions",
    "Report",
    "Check",
    "Tool",
    "Artifact",
    "Digest",
    "CellCount",
    "Observation",
    "Memory",
    "Variant",
    "StageLink",
    "Stage",
    "Macro",
    "Dependency",
    "Subflow",
    "Receipt",
    "Project",
    "ImportAsk",
    "NodeAsk",
    "EdgeAsk",
    "GraphAsk",
    "OutcomeOf",
    "ChipDesign",
    "ImportFileArgs",
    "ImportReportArgs",
    "ImportVerificationArgs",
    "TakeInArgs",
    "KeepReportArgs",
    "KeepReceiptArgs",
    "RemoveReportArgs",
    "RemoveVerificationArgs",
    "ShowGraphArgs",
    "ShowAllArgs",
    "InspectReportArgs",
    "InspectReceiptArgs",
    "InspectStageArgs",
    "InspectMacroArgs",
    "OpenSubflowArgs",
    "DismissArgs",
    "ZoomGraphArgs",
    "SelectOutputArgs",
    "ClearOutputArgs",
    "InspectOperatorArgs",
    "CloseOperatorArgs",
    "AddOperatorArgs",
    "AddOperatorAtArgs",
    "SetShapeArgs",
    "ApplyShapeArgs",
    "ApplyWidthArgs",
    "TapMacroArgs",
    "SetOutputWidthArgs",
    "MoveOperatorArgs",
    "RemoveOperatorArgs",
    "ConnectArgs",
    "DisconnectArgs",
    "ResolveGraphArgs",
    "ApplyGraphArgs",
    "InspectSeenMacroArgs",
    "TapGraphArgs",
]

OperatorKind = Literal["input", "gate_input", "norm_weight_input", "rms_norm", "apply_norm_weight", "silu", "multiply", "projection_input", "matrix_weight_input", "linear_projection", "add"]
"""The structural operator kinds a design is built from."""

CacheKind = Literal["bound", "unsupported"]
"""Whether a storage report bounds its attention cache, or cannot say."""

CheckResult = Literal["passed", "failed"]
"""How a reported check ended."""

Comparison = Literal["exact", "characterization"]
"""What a numerical comparison asserts."""

ObservationKind = Literal["none", "cpu_reference", "rtl_simulation", "generic_synthesis", "library_physical", "rms_norm", "silu", "gated_delta", "softmax"]
"""What a reported evidence stage observed."""

Outcome = Literal["completed", "expected_refusal"]
"""How a reported RTL simulation ended."""

Orientation = Literal["r0", "r90", "r180", "r270", "mx", "my", "mxr90", "myr90"]
"""The reported macro placements' orientation."""

RunResult = Literal["passed", "failed", "cancelled"]
"""How a reported run ended."""

DependencyKind = Literal["data", "build", "verification"]
"""What a reported evidence dependency carries."""

StageKind = Literal["prepare", "numerical", "fault", "synthesis", "aggregate"]
"""Which stage of an architecture comparison a receipt reports."""

ImportFileArgs = TypedDict("ImportFileArgs", {
    "file": "Any",
})
"""The arguments of `import_file`."""

ImportReportArgs = TypedDict("ImportReportArgs", {
    "document": "str",
})
"""The arguments of `import_report`."""

ImportVerificationArgs = TypedDict("ImportVerificationArgs", {
    "document": "str",
})
"""The arguments of `import_verification`."""

TakeInArgs = TypedDict("TakeInArgs", {
    "document": "str",
})
"""The arguments of `take_in`."""

KeepReportArgs = TypedDict("KeepReportArgs", {
    "digest": "str",
    "source": "str",
    "facts": "Any",
})
"""The arguments of `keep_report`."""

KeepReceiptArgs = TypedDict("KeepReceiptArgs", {
    "digest": "str",
    "source": "str",
    "facts": "Any",
})
"""The arguments of `keep_receipt`."""

RemoveReportArgs = TypedDict("RemoveReportArgs", {
    "r": "Report",
})
"""The arguments of `remove_report`."""

RemoveVerificationArgs = TypedDict("RemoveVerificationArgs", {
    "r": "Receipt",
})
"""The arguments of `remove_verification`."""

ShowGraphArgs = TypedDict("ShowGraphArgs", {
})
"""The arguments of `show_graph`."""

ShowAllArgs = TypedDict("ShowAllArgs", {
})
"""The arguments of `show_all`."""

InspectReportArgs = TypedDict("InspectReportArgs", {
    "r": "Report",
})
"""The arguments of `inspect_report`."""

InspectReceiptArgs = TypedDict("InspectReceiptArgs", {
    "r": "Receipt",
})
"""The arguments of `inspect_receipt`."""

InspectStageArgs = TypedDict("InspectStageArgs", {
    "r": "Receipt",
    "s": "Stage",
})
"""The arguments of `inspect_stage`."""

InspectMacroArgs = TypedDict("InspectMacroArgs", {
    "r": "Receipt",
    "m": "Macro",
})
"""The arguments of `inspect_macro`."""

OpenSubflowArgs = TypedDict("OpenSubflowArgs", {
    "r": "Receipt",
    "s": "Subflow",
})
"""The arguments of `open_subflow`."""

DismissArgs = TypedDict("DismissArgs", {
})
"""The arguments of `dismiss`."""

ZoomGraphArgs = TypedDict("ZoomGraphArgs", {
    "step": "str",
})
"""The arguments of `zoom_graph`."""

SelectOutputArgs = TypedDict("SelectOutputArgs", {
    "op": "Operator",
    "port": "str",
})
"""The arguments of `select_output`."""

ClearOutputArgs = TypedDict("ClearOutputArgs", {
})
"""The arguments of `clear_output`."""

InspectOperatorArgs = TypedDict("InspectOperatorArgs", {
    "op": "Operator",
})
"""The arguments of `inspect_operator`."""

CloseOperatorArgs = TypedDict("CloseOperatorArgs", {
})
"""The arguments of `close_operator`."""

AddOperatorArgs = TypedDict("AddOperatorArgs", {
    "name": "str",
})
"""The arguments of `add_operator`."""

AddOperatorAtArgs = TypedDict("AddOperatorAtArgs", {
    "node": "str",
    "kind": "OperatorKind",
    "shape": "list[int]",
    "output_width": "int",
    "x": "int",
    "y": "int",
})
"""The arguments of `add_operator_at`."""

SetShapeArgs = TypedDict("SetShapeArgs", {
    "op": "Operator",
    "shape": "list[int]",
})
"""The arguments of `set_shape`."""

ApplyShapeArgs = TypedDict("ApplyShapeArgs", {
    "op": "Operator",
})
"""The arguments of `apply_shape`."""

ApplyWidthArgs = TypedDict("ApplyWidthArgs", {
    "op": "Operator",
})
"""The arguments of `apply_width`."""

TapMacroArgs = TypedDict("TapMacroArgs", {
    "s": "Stage",
    "index": "int",
})
"""The arguments of `tap_macro`."""

SetOutputWidthArgs = TypedDict("SetOutputWidthArgs", {
    "op": "Operator",
    "width": "int",
})
"""The arguments of `set_output_width`."""

MoveOperatorArgs = TypedDict("MoveOperatorArgs", {
    "op": "Operator",
    "x": "int",
    "y": "int",
})
"""The arguments of `move_operator`."""

RemoveOperatorArgs = TypedDict("RemoveOperatorArgs", {
    "op": "Operator",
})
"""The arguments of `remove_operator`."""

ConnectArgs = TypedDict("ConnectArgs", {
    "from": "Operator",
    "from_port": "str",
    "to": "Operator",
    "to_port": "str",
})
"""The arguments of `connect`."""

DisconnectArgs = TypedDict("DisconnectArgs", {
    "op": "Operator",
    "port": "str",
})
"""The arguments of `disconnect`."""

ResolveGraphArgs = TypedDict("ResolveGraphArgs", {
})
"""The arguments of `resolve_graph`."""

ApplyGraphArgs = TypedDict("ApplyGraphArgs", {
    "answer": "str",
})
"""The arguments of `apply_graph`."""

InspectSeenMacroArgs = TypedDict("InspectSeenMacroArgs", {
    "r": NotRequired["Receipt | None"],
    "s": "Stage",
    "m": "Macro",
})
"""The arguments of `inspect_seen_macro`."""

TapGraphArgs = TypedDict("TapGraphArgs", {
    "index": "int",
})
"""The arguments of `tap_graph`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
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


class Port(Resource):
    """Port — `tl.so.tilo.chip-design.Port`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    esvx2n72: str  # key
    vjm4mdax: str  # data_type


class Operator(Resource):
    """Operator — `tl.so.tilo.chip-design.Operator`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    wxkbwcn5: str  # node
    e6aj4t7l: OperatorKind  # kind
    nbs52ikt: list[int]  # shape
    w6rehfmk: int  # normalization_width
    gonrdrls: int  # output_width
    u3ws6vjg: bool  # has_output_width
    nreyvv4r: str  # shape_text
    sjbeclxu: str  # width_text
    s4l6n47t: int  # x
    pgjl7vwe: int  # y
    neb6c6b5: list[Entry[Port]]  # inputs
    eb6j7t6x: list[Entry[Port]]  # outputs


class Connection(Resource):
    """Connection — `tl.so.tilo.chip-design.Connection`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    sqqjxtyw: str  # from_node
    hk3nk2x5: str  # from_port
    xgc3rbc2: str  # to_node
    hxqnvwsp: str  # to_port
    @property
    def _3tc244az(self) -> str:
        """digest (`3tc244az` on the wire)."""
        return self.fields["3tc244az"]

    @_3tc244az.setter
    def _3tc244az(self, value: str) -> None:
        self.edit("3tc244az", value=value)
    o2gagtnb: bool  # active
    qkwwqrxp: str  # reason


class Assumptions(Resource):
    """Assumptions — `tl.so.tilo.chip-design.Assumptions`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    run6wd4l: int  # logical_parameters
    smx2rwim: int  # weight_bits
    kclrpitf: int  # kv_bits
    @property
    def _6r63anpq(self) -> int:
        """context_tokens (`6r63anpq` on the wire)."""
        return self.fields["6r63anpq"]

    @_6r63anpq.setter
    def _6r63anpq(self, value: int) -> None:
        self.edit("6r63anpq", value=value)


class Report(Resource):
    """Report — `tl.so.tilo.chip-design.Report`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    twijrbwn: str  # digest
    @property
    def _7rep5j6b(self) -> str:
        """source (`7rep5j6b` on the wire)."""
        return self.fields["7rep5j6b"]

    @_7rep5j6b.setter
    def _7rep5j6b(self, value: str) -> None:
        self.edit("7rep5j6b", value=value)
    hd2g3vdr: str  # model
    fxx5zcjz: int  # schema_version
    svry2b7t: str  # revision
    xyjuenxt: str  # config_sha256
    vyv4me7w: str  # evidence
    b3g6lqvm: int  # batch_size
    @property
    def _5qj7upc2(self) -> Assumptions:
        """assumptions (`5qj7upc2` on the wire)."""
        return self.fields["5qj7upc2"]

    @_5qj7upc2.setter
    def _5qj7upc2(self, value: Assumptions) -> None:
        self.edit("5qj7upc2", value=value)
    @property
    def _42kdgwvs(self) -> int:
        """packed_weight_bytes (`42kdgwvs` on the wire)."""
        return self.fields["42kdgwvs"]

    @_42kdgwvs.setter
    def _42kdgwvs(self, value: int) -> None:
        self.edit("42kdgwvs", value=value)
    @property
    def _7jvkir3f(self) -> CacheKind:
        """cache (`7jvkir3f` on the wire)."""
        return self.fields["7jvkir3f"]

    @_7jvkir3f.setter
    def _7jvkir3f(self, value: CacheKind) -> None:
        self.edit("7jvkir3f", value=value)
    qw2v536a: str  # cache_reason
    ogp5wc76: int  # attention_layers
    he3itm4b: int  # kv_elements
    biifjz5l: int  # kv_bytes
    n3tcruqh: list[str]  # cache_excludes
    mrp773gq: list[str]  # not_estimated
    oypgaywq: list[str]  # limitations


class Check(Resource):
    """Check — `tl.so.tilo.chip-design.Check`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    i2clfcou: str  # name
    arunp5tg: CheckResult  # result


class Tool(Resource):
    """Tool — `tl.so.tilo.chip-design.Tool`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    @property
    def _6l36tmym(self) -> str:
        """name (`6l36tmym` on the wire)."""
        return self.fields["6l36tmym"]

    @_6l36tmym.setter
    def _6l36tmym(self, value: str) -> None:
        self.edit("6l36tmym", value=value)
    ptuif53v: str  # version
    @property
    def _6ffjwxk2(self) -> str:
        """executable_sha256 (`6ffjwxk2` on the wire)."""
        return self.fields["6ffjwxk2"]

    @_6ffjwxk2.setter
    def _6ffjwxk2(self, value: str) -> None:
        self.edit("6ffjwxk2", value=value)


class Artifact(Resource):
    """Artifact — `tl.so.tilo.chip-design.Artifact`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    @property
    def _4d7avciq(self) -> str:
        """name (`4d7avciq` on the wire)."""
        return self.fields["4d7avciq"]

    @_4d7avciq.setter
    def _4d7avciq(self, value: str) -> None:
        self.edit("4d7avciq", value=value)
    nwa3vepp: str  # sha256


class Digest(Resource):
    """Digest — `tl.so.tilo.chip-design.Digest`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    r4lv3eu7: str  # name
    pn5tgrsn: str  # sha256


class CellCount(Resource):
    """CellCount — `tl.so.tilo.chip-design.CellCount`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    r4l4iqx2: str  # name
    vxrpbdeb: int  # count


class Observation(Resource):
    """Observation — `tl.so.tilo.chip-design.Observation`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    l3my4c7u: str  # label
    boatafil: ObservationKind  # kind
    jbgmt4ie: Comparison  # comparison
    @property
    def _2m7xr7oi(self) -> int:
        """compared_words (`2m7xr7oi` on the wire)."""
        return self.fields["2m7xr7oi"]

    @_2m7xr7oi.setter
    def _2m7xr7oi(self, value: int) -> None:
        self.edit("2m7xr7oi", value=value)
    @property
    def _765n5dvn(self) -> int:
        """differing_words (`765n5dvn` on the wire)."""
        return self.fields["765n5dvn"]

    @_765n5dvn.setter
    def _765n5dvn(self, value: int) -> None:
        self.edit("765n5dvn", value=value)
    r2sgywxz: str  # output_sha256
    @property
    def _2mvyzxqy(self) -> Outcome:
        """outcome (`2mvyzxqy` on the wire)."""
        return self.fields["2mvyzxqy"]

    @_2mvyzxqy.setter
    def _2mvyzxqy(self, value: Outcome) -> None:
        self.edit("2mvyzxqy", value=value)
    jqiqarxm: int  # refused_inputs
    @property
    def _7g5ljuf2(self) -> int:
        """cycles (`7g5ljuf2` on the wire)."""
        return self.fields["7g5ljuf2"]

    @_7g5ljuf2.setter
    def _7g5ljuf2(self, value: int) -> None:
        self.edit("7g5ljuf2", value=value)
    tkyyux4t: int  # accepted_inputs
    byvm4brz: int  # accepted_outputs
    ysqpkcxy: int  # logic_cells
    fmeekkru: int  # logical_memory_bits
    fswivmpb: str  # library
    e5eeqpa7: str  # clock_constraint_ns
    orrpic3k: str  # setup_slack_ns
    pbdqj47x: str  # hold_slack_ns
    j7qjxpgt: int  # routed_signal_nets
    js7iwiwx: int  # router_violations
    bsogreds: str  # library_area_um2
    @property
    def _62qim76q(self) -> str:
        """profile (`62qim76q` on the wire)."""
        return self.fields["62qim76q"]

    @_62qim76q.setter
    def _62qim76q(self, value: str) -> None:
        self.edit("62qim76q", value=value)
    vdf52bkl: int  # fp32_comparisons
    qvzkneov: int  # total_inputs
    sofq7z7n: int  # exp_differences
    rgsgq4i3: int  # max_exp_ulp
    fwsjrjk3: int  # heads
    shn33gs4: int  # tokens
    fn6veddp: int  # width
    d3y6qjsu: int  # attention_words
    rfak7s5n: int  # state_words
    @property
    def _3mbqqhmz(self) -> str:
        """name (`3mbqqhmz` on the wire)."""
        return self.fields["3mbqqhmz"]

    @_3mbqqhmz.setter
    def _3mbqqhmz(self, value: str) -> None:
        self.edit("3mbqqhmz", value=value)
    hif4kqoj: int  # masked
    @property
    def _3ijpmjgv(self) -> int:
        """max_ulp (`3ijpmjgv` on the wire)."""
        return self.fields["3ijpmjgv"]

    @_3ijpmjgv.setter
    def _3ijpmjgv(self, value: int) -> None:
        self.edit("3ijpmjgv", value=value)


class Memory(Resource):
    """Memory — `tl.so.tilo.chip-design.Memory`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    imhcxbpl: str  # name
    eifw7p4n: int  # width_bits
    @property
    def _474at3uz(self) -> int:
        """depth_words (`474at3uz` on the wire)."""
        return self.fields["474at3uz"]

    @_474at3uz.setter
    def _474at3uz(self, value: int) -> None:
        self.edit("474at3uz", value=value)
    o76j7m2f: int  # bits
    @property
    def _2aqawzb3(self) -> int:
        """read_ports (`2aqawzb3` on the wire)."""
        return self.fields["2aqawzb3"]

    @_2aqawzb3.setter
    def _2aqawzb3(self, value: int) -> None:
        self.edit("2aqawzb3", value=value)
    he5exqe2: int  # clocked_read_ports
    @property
    def _6nk642cp(self) -> int:
        """write_ports (`6nk642cp` on the wire)."""
        return self.fields["6nk642cp"]

    @_6nk642cp.setter
    def _6nk642cp(self, value: int) -> None:
        self.edit("6nk642cp", value=value)
    hovn72p4: bool  # fully_initialized


class Variant(Resource):
    """Variant — `tl.so.tilo.chip-design.Variant`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    zl5mvubo: str  # identity
    @property
    def _4y2sweqe(self) -> bool:
        """numerical (`4y2sweqe` on the wire)."""
        return self.fields["4y2sweqe"]

    @_4y2sweqe.setter
    def _4y2sweqe(self, value: bool) -> None:
        self.edit("4y2sweqe", value=value)
    qe2ijn52: int  # cold_program_cycles
    lvk3uaeb: int  # vector_load_cycles
    pm72baqp: int  # warm_cycles
    bctwv4iq: int  # accepted_weight_bytes
    sbhcuyji: int  # accepted_descriptor_bytes
    lrpqhjtv: int  # cache_reads
    chkz2y4j: int  # quantizer_packets
    f7om2r4s: int  # comparison_words
    @property
    def _5p4i7krn(self) -> str:
        """output_sha256 (`5p4i7krn` on the wire)."""
        return self.fields["5p4i7krn"]

    @_5p4i7krn.setter
    def _5p4i7krn(self, value: str) -> None:
        self.edit("5p4i7krn", value=value)
    vte7iosv: bool  # synthesis
    z3aiymm6: int  # generic_logic_cells
    wsvyc46d: int  # activation_cache_bits
    ouotgooh: int  # programmed_weight_bits
    htrm4etw: int  # fixed_initialized_bits
    s3urh6zt: list[Entry[Memory]]  # memories


class StageLink(Resource):
    """StageLink — `tl.so.tilo.chip-design.StageLink`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    @property
    def _2bpp44k7(self) -> str:
        """name (`2bpp44k7` on the wire)."""
        return self.fields["2bpp44k7"]

    @_2bpp44k7.setter
    def _2bpp44k7(self, value: str) -> None:
        self.edit("2bpp44k7", value=value)
    cphzthhu: str  # receipt_sha256
    vnv3rjs3: str  # producer_sha256
    u5cz3l65: str  # input_identity_sha256


class Stage(Resource):
    """Stage — `tl.so.tilo.chip-design.Stage`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    @property
    def _4qfzbkq5(self) -> str:
        """key (`4qfzbkq5` on the wire)."""
        return self.fields["4qfzbkq5"]

    @_4qfzbkq5.setter
    def _4qfzbkq5(self, value: str) -> None:
        self.edit("4qfzbkq5", value=value)
    htbbatar: str  # title
    wyge75zm: RunResult  # status
    uglh3san: str  # original_receipt_artifact
    @property
    def _46xvj5o3(self) -> str:
        """producer_sha256 (`46xvj5o3` on the wire)."""
        return self.fields["46xvj5o3"]

    @_46xvj5o3.setter
    def _46xvj5o3(self, value: str) -> None:
        self.edit("46xvj5o3", value=value)
    @property
    def _775i2e3d(self) -> str:
        """input_identity_sha256 (`775i2e3d` on the wire)."""
        return self.fields["775i2e3d"]

    @_775i2e3d.setter
    def _775i2e3d(self, value: str) -> None:
        self.edit("775i2e3d", value=value)
    vxt23uic: list[str]  # limitations
    zistzyfl: ObservationKind  # observed
    ff56mklf: Observation  # facts
    @property
    def _4nxr5eoq(self) -> str:
        """observation (`4nxr5eoq` on the wire)."""
        return self.fields["4nxr5eoq"]

    @_4nxr5eoq.setter
    def _4nxr5eoq(self, value: str) -> None:
        self.edit("4nxr5eoq", value=value)
    vqhz7zzv: list[Entry[Check]]  # checks
    znevuybg: bool  # placed
    @property
    def _6mwfx57p(self) -> str:
        """database_artifact (`6mwfx57p` on the wire)."""
        return self.fields["6mwfx57p"]

    @_6mwfx57p.setter
    def _6mwfx57p(self, value: str) -> None:
        self.edit("6mwfx57p", value=value)
    t4u6picr: str  # observation_artifact
    jquleaxm: int  # dbu_per_micron
    abpu3jku: int  # die_left
    zgxyfzp2: int  # die_bottom
    cfo4rbhf: int  # die_right
    ozebffaf: int  # die_top
    lk5ymnxh: int  # total_instances
    linro3fe: int  # omitted_instances
    l7433uoi: list[Entry[Macro]]  # macros


class Macro(Resource):
    """Macro — `tl.so.tilo.chip-design.Macro`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    l2pfdak5: str  # slot
    shgem24v: str  # name
    oo4l5j2f: str  # master
    tpbo6p6i: Orientation  # orientation
    mvnygfkc: str  # placement
    sb4fvyic: int  # left
    uwbrid6d: int  # bottom
    bdrtkvnd: int  # right
    @property
    def _2f6kftyj(self) -> int:
        """top (`2f6kftyj` on the wire)."""
        return self.fields["2f6kftyj"]

    @_2f6kftyj.setter
    def _2f6kftyj(self, value: int) -> None:
        self.edit("2f6kftyj", value=value)


class Dependency(Resource):
    """Dependency — `tl.so.tilo.chip-design.Dependency`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    lifw33iv: str  # from
    lau4lbgx: str  # to
    vuoz6p3k: DependencyKind  # kind
    flo3szj2: str  # artifact


class Subflow(Resource):
    """Subflow — `tl.so.tilo.chip-design.Subflow`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    utp25rwu: str  # key
    @property
    def _2hn4pjda(self) -> str:
        """title (`2hn4pjda` on the wire)."""
        return self.fields["2hn4pjda"]

    @_2hn4pjda.setter
    def _2hn4pjda(self, value: str) -> None:
        self.edit("2hn4pjda", value=value)
    qhfawhwq: str  # summary_artifact


class Receipt(Resource):
    """Receipt — `tl.so.tilo.chip-design.Receipt`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    mhsaondd: str  # digest
    nw7f6brj: str  # source
    x37f53ms: str  # evidence
    nj3b4qpo: RunResult  # status
    jhyibxbz: str  # imported_bytes_sha256
    @property
    def _3zyb62tt(self) -> str:
        """failure (`3zyb62tt` on the wire)."""
        return self.fields["3zyb62tt"]

    @_3zyb62tt.setter
    def _3zyb62tt(self, value: str) -> None:
        self.edit("3zyb62tt", value=value)
    zon6lior: str  # failure_stage
    bp2hd5ou: CheckResult  # input_validation
    sp7jdjll: bool  # has_input_validation
    gcb6foq7: int  # lanes
    ufj5tgp7: bool  # has_lanes
    ntsw3ejx: int  # group_size
    t5yt4cgs: bool  # has_group_size
    ghx5dlez: str  # declared_toolchain_image
    sofs7yem: str  # sample_manifest_sha256
    tu4t57su: str  # verifier_sha256
    qvghxafg: str  # artifact_validator_sha256
    snhxqhnq: str  # mapper_sha256
    @property
    def _7ifmtq3n(self) -> str:
        """library_revision (`7ifmtq3n` on the wire)."""
        return self.fields["7ifmtq3n"]

    @_7ifmtq3n.setter
    def _7ifmtq3n(self, value: str) -> None:
        self.edit("7ifmtq3n", value=value)
    pyl5qiwx: int  # mapped_cell_count
    psq5vxbm: bool  # has_mapped_cell_count
    @property
    def _325si2ag(self) -> str:
        """reported_library_area (`325si2ag` on the wire)."""
        return self.fields["325si2ag"]

    @_325si2ag.setter
    def _325si2ag(self, value: str) -> None:
        self.edit("325si2ag", value=value)
    @property
    def _4ldwfggs(self) -> int:
        """abc_delay_target_ps (`4ldwfggs` on the wire)."""
        return self.fields["4ldwfggs"]

    @_4ldwfggs.setter
    def _4ldwfggs(self, value: int) -> None:
        self.edit("4ldwfggs", value=value)
    sdqy2njd: bool  # has_abc_target
    jxgfvcs7: bool  # manufacturable
    d6eyywsn: bool  # has_manufacturable
    hu5xloo5: bool  # synthetic_coefficients
    rmrnw3gy: bool  # has_synthetic_coefficients
    gvdrc4h2: bool  # synthetic_activations
    @property
    def _3uovqvrr(self) -> bool:
        """has_synthetic_activations (`3uovqvrr` on the wire)."""
        return self.fields["3uovqvrr"]

    @_3uovqvrr.setter
    def _3uovqvrr(self, value: bool) -> None:
        self.edit("3uovqvrr", value=value)
    @property
    def _6fqc7ll4(self) -> str:
        """sample_sha256 (`6fqc7ll4` on the wire)."""
        return self.fields["6fqc7ll4"]

    @_6fqc7ll4.setter
    def _6fqc7ll4(self, value: str) -> None:
        self.edit("6fqc7ll4", value=value)
    hsospyt4: str  # sample_receipt_sha256
    zenayb65: str  # rtl_source_sha256
    mbf2wgz7: str  # real_protocol
    @property
    def _4ddzxjoe(self) -> str:
        """c_instrumentation (`4ddzxjoe` on the wire)."""
        return self.fields["4ddzxjoe"]

    @_4ddzxjoe.setter
    def _4ddzxjoe(self, value: str) -> None:
        self.edit("4ddzxjoe", value=value)
    cbi2ze7u: int  # external_scalar_comparisons
    @property
    def _277q5ghg(self) -> bool:
        """has_external_scalar_comparisons (`277q5ghg` on the wire)."""
        return self.fields["277q5ghg"]

    @_277q5ghg.setter
    def _277q5ghg(self, value: bool) -> None:
        self.edit("277q5ghg", value=value)
    bdd52yao: int  # generic_cell_count
    bsobd2xf: bool  # has_generic_cell_count
    @property
    def _7bk6ofeo(self) -> int:
        """excluded_metadata_cells (`7bk6ofeo` on the wire)."""
        return self.fields["7bk6ofeo"]

    @_7bk6ofeo.setter
    def _7bk6ofeo(self, value: int) -> None:
        self.edit("7bk6ofeo", value=value)
    @property
    def _3sohcddq(self) -> bool:
        """has_excluded_metadata_cells (`3sohcddq` on the wire)."""
        return self.fields["3sohcddq"]

    @_3sohcddq.setter
    def _3sohcddq(self, value: bool) -> None:
        self.edit("3sohcddq", value=value)
    @property
    def _5rnyzfkw(self) -> list[str]:
        """compiler_flags (`5rnyzfkw` on the wire)."""
        return self.fields["5rnyzfkw"]

    @_5rnyzfkw.setter
    def _5rnyzfkw(self, value: list[str]) -> None:
        self.edit("5rnyzfkw", value=value)
    u44pzow2: list[Entry[Check]]  # checks
    zg5xxom6: list[Entry[Tool]]  # tools
    zxqh5fqc: list[Entry[Artifact]]  # artifacts
    @property
    def _4nv3clxq(self) -> list[str]:
        """excludes (`4nv3clxq` on the wire)."""
        return self.fields["4nv3clxq"]

    @_4nv3clxq.setter
    def _4nv3clxq(self, value: list[str]) -> None:
        self.edit("4nv3clxq", value=value)
    upp3twxb: list[Entry[Digest]]  # input_digests
    eryiyu4m: list[Entry[Digest]]  # primary_sources
    ljs2gw3t: list[Entry[Digest]]  # sources
    ev5xwwya: list[Entry[CellCount]]  # generic_cells
    yd5juegt: bool  # operator
    ytpaenff: str  # op_original_receipt_sha256
    @property
    def _67ramk2w(self) -> str:
        """op_primary_revision (`67ramk2w` on the wire)."""
        return self.fields["67ramk2w"]

    @_67ramk2w.setter
    def _67ramk2w(self, value: str) -> None:
        self.edit("67ramk2w", value=value)
    l2hy4vng: str  # op_compiler_version
    @property
    def _7mcamyri(self) -> str:
        """op_compiler_executable_sha256 (`7mcamyri` on the wire)."""
        return self.fields["7mcamyri"]

    @_7mcamyri.setter
    def _7mcamyri(self, value: str) -> None:
        self.edit("7mcamyri", value=value)
    gbjecqi4: list[str]  # op_compiler_flags
    s52ihrs2: list[Entry[Digest]]  # op_primary_sources
    el3wnlvr: list[Entry[Digest]]  # op_local_sources
    vayzuyd3: list[Entry[Digest]]  # op_libraries
    ph5z4g57: list[Entry[Observation]]  # observations
    @property
    def _3tunzd7f(self) -> bool:
        """study (`3tunzd7f` on the wire)."""
        return self.fields["3tunzd7f"]

    @_3tunzd7f.setter
    def _3tunzd7f(self, value: bool) -> None:
        self.edit("3tunzd7f", value=value)
    yu7n73kw: StageKind  # study_stage
    o34vwu7q: str  # study_original_receipt_sha256
    czwcogej: str  # study_producer_sha256
    iehv2mu5: str  # study_input_identity_sha256
    idk7oh4r: int  # study_expected_comparison_words
    tietricm: list[Entry[StageLink]]  # links
    p4nm3blp: list[Entry[Variant]]  # variants
    @property
    def _4zxln2bs(self) -> bool:
        """flow (`4zxln2bs` on the wire)."""
        return self.fields["4zxln2bs"]

    @_4zxln2bs.setter
    def _4zxln2bs(self, value: bool) -> None:
        self.edit("4zxln2bs", value=value)
    x4ibyx74: str  # flow_title
    eti75aww: str  # flow_input_identity_sha256
    dj67bcy5: list[Entry[Stage]]  # stages
    @property
    def _6cz7yifm(self) -> list[Entry[Dependency]]:
        """dependencies (`6cz7yifm` on the wire)."""
        return self.fields["6cz7yifm"]

    @_6cz7yifm.setter
    def _6cz7yifm(self, value: list[Entry[Dependency]]) -> None:
        self.edit("6cz7yifm", value=value)
    ol3j2h7j: list[Entry[Subflow]]  # subflows


class Project(Resource):
    """Project — `tl.so.tilo.chip-design.Project`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    @property
    def _5vvat77h(self) -> list[Entry[Report]]:
        """reports (`5vvat77h` on the wire)."""
        return self.fields["5vvat77h"]

    @_5vvat77h.setter
    def _5vvat77h(self, value: list[Entry[Report]]) -> None:
        self.edit("5vvat77h", value=value)
    lst2bswm: list[Entry[Receipt]]  # verifications
    qajzmqsz: list[Entry[Operator]]  # operators
    ij6pw4z7: list[Entry[Connection]]  # connections
    lmh6n2cu: str  # selected
    iwadzlgx: str  # subject
    u7dhnpij: str  # node
    mppe3c5h: str  # output_node
    ocvjm3oh: str  # output_port
    vrtmdnei: int  # zoom
    @property
    def _3oydn2gz(self) -> str:
        """stage (`3oydn2gz` on the wire)."""
        return self.fields["3oydn2gz"]

    @_3oydn2gz.setter
    def _3oydn2gz(self, value: str) -> None:
        self.edit("3oydn2gz", value=value)
    uuloznl2: str  # macro
    yl422q4s: str  # failure
    @property
    def _2cb665aj(self) -> str:
        """notice (`2cb665aj` on the wire)."""
        return self.fields["2cb665aj"]

    @_2cb665aj.setter
    def _2cb665aj(self, value: str) -> None:
        self.edit("2cb665aj", value=value)

    def import_file(self, *, file: Any) -> None:
        """Imports the engineering evidence a JSON file holds: an analytical storage report or a hardware verification receipt. This is what a person's files and the file chooser hand the app, and importing the same bytes twice keeps one record."""
        return self.action("import_file", file=file)

    def import_file_many(self, items: list[ImportFileArgs]) -> list[dict[str, Any]]:
        """Runs `import_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_file", items)

    def import_report(self, *, document: str) -> None:
        """Imports an analytical storage report from its JSON."""
        return self.action("import_report", document=document)

    def import_report_many(self, items: list[ImportReportArgs]) -> list[dict[str, Any]]:
        """Runs `import_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_report", items)

    def import_verification(self, *, document: str) -> None:
        """Imports a hardware verification receipt from its JSON."""
        return self.action("import_verification", document=document)

    def import_verification_many(self, items: list[ImportVerificationArgs]) -> list[dict[str, Any]]:
        """Runs `import_verification` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_verification", items)

    def take_in(self, *, document: str) -> None:
        """Takes in one imported document, whichever it is: the device checks it first, and what it holds reaches the project only once its own contract accepts it."""
        return self.action("take_in", document=document)

    def take_in_many(self, items: list[TakeInArgs]) -> list[dict[str, Any]]:
        """Runs `take_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take_in", items)

    def keep_report(self,
        *,
        digest: str,
        source: str,
        facts: Any,
    ) -> None:
        """Keeps one analytical storage report and the assumptions and scope it states."""
        return self.action("keep_report", digest=digest, source=source, facts=facts)

    def keep_report_many(self, items: list[KeepReportArgs]) -> list[dict[str, Any]]:
        """Runs `keep_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_report", items)

    def keep_receipt(self,
        *,
        digest: str,
        source: str,
        facts: Any,
    ) -> None:
        """Keeps one hardware verification receipt: its scope, its checks, the identities it reports, and whichever of the three reported protocols it speaks."""
        return self.action("keep_receipt", digest=digest, source=source, facts=facts)

    def keep_receipt_many(self, items: list[KeepReceiptArgs]) -> list[dict[str, Any]]:
        """Runs `keep_receipt` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_receipt", items)

    def remove_report(self, *, r: Report) -> None:
        """Removes a report and everything it stated."""
        return self.action("remove_report", r=r)

    def remove_report_many(self, items: list[RemoveReportArgs]) -> list[dict[str, Any]]:
        """Runs `remove_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_report", items)

    def remove_verification(self, *, r: Receipt) -> None:
        """Removes a verification receipt and everything it stated."""
        return self.action("remove_verification", r=r)

    def remove_verification_many(self, items: list[RemoveVerificationArgs]) -> list[dict[str, Any]]:
        """Runs `remove_verification` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_verification", items)

    def show_graph(self) -> None:
        """Opens the operator graph."""
        return self.action("show_graph")

    def show_graph_many(self, items: list[ShowGraphArgs]) -> list[dict[str, Any]]:
        """Runs `show_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_graph", items)

    def show_all(self) -> None:
        """Lists every report and receipt the project holds."""
        return self.action("show_all")

    def show_all_many(self, items: list[ShowAllArgs]) -> list[dict[str, Any]]:
        """Runs `show_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_all", items)

    def inspect_report(self, *, r: Report) -> None:
        """Shows one report."""
        return self.action("inspect_report", r=r)

    def inspect_report_many(self, items: list[InspectReportArgs]) -> list[dict[str, Any]]:
        """Runs `inspect_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("inspect_report", items)

    def inspect_receipt(self, *, r: Receipt) -> None:
        """Shows one verification receipt."""
        return self.action("inspect_receipt", r=r)

    def inspect_receipt_many(self, items: list[InspectReceiptArgs]) -> list[dict[str, Any]]:
        """Runs `inspect_receipt` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("inspect_receipt", items)

    def inspect_stage(self,
        *,
        r: Receipt,
        s: Stage,
    ) -> None:
        """Shows one stage of the receipt the window holds open."""
        return self.action("inspect_stage", r=r, s=s)

    def inspect_stage_many(self, items: list[InspectStageArgs]) -> list[dict[str, Any]]:
        """Runs `inspect_stage` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("inspect_stage", items)

    def inspect_macro(self,
        *,
        r: Receipt,
        m: Macro,
    ) -> None:
        """Shows one macro of the placement the window holds open."""
        return self.action("inspect_macro", r=r, m=m)

    def inspect_macro_many(self, items: list[InspectMacroArgs]) -> list[dict[str, Any]]:
        """Runs `inspect_macro` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("inspect_macro", items)

    def open_subflow(self,
        *,
        r: Receipt,
        s: Subflow,
    ) -> None:
        """Opens the receipt an evidence flow's subflow reference names, when its summary is imported."""
        return self.action("open_subflow", r=r, s=s)

    def open_subflow_many(self, items: list[OpenSubflowArgs]) -> list[dict[str, Any]]:
        """Runs `open_subflow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_subflow", items)

    def dismiss(self) -> None:
        """Shrugs off the line about the last refusal or success."""
        return self.action("dismiss")

    def dismiss_many(self, items: list[DismissArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss", items)

    def zoom_graph(self, *, step: str) -> None:
        """Zooms the operator graph in, out, or back to its whole extent."""
        return self.action("zoom_graph", step=step)

    def zoom_graph_many(self, items: list[ZoomGraphArgs]) -> list[dict[str, Any]]:
        """Runs `zoom_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("zoom_graph", items)

    def select_output(self,
        *,
        op: Operator,
        port: str,
    ) -> None:
        """Chooses the output a person connects from, naming the inputs it can feed."""
        return self.action("select_output", op=op, port=port)

    def select_output_many(self, items: list[SelectOutputArgs]) -> list[dict[str, Any]]:
        """Runs `select_output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("select_output", items)

    def clear_output(self) -> None:
        """Leaves the graph without a chosen output."""
        return self.action("clear_output")

    def clear_output_many(self, items: list[ClearOutputArgs]) -> list[dict[str, Any]]:
        """Runs `clear_output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_output", items)

    def inspect_operator(self, *, op: Operator) -> None:
        """Shows one operator's own facts, or closes them."""
        return self.action("inspect_operator", op=op)

    def inspect_operator_many(self, items: list[InspectOperatorArgs]) -> list[dict[str, Any]]:
        """Runs `inspect_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("inspect_operator", items)

    def close_operator(self) -> None:
        """Runs the app's action."""
        return self.action("close_operator")

    def close_operator_many(self, items: list[CloseOperatorArgs]) -> list[dict[str, Any]]:
        """Runs `close_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_operator", items)

    def add_operator(self, *, name: str) -> None:
        """Adds an operator of `kind` after the ones already in the graph, on a four-wide grid."""
        return self.action("add_operator", name=name)

    def add_operator_many(self, items: list[AddOperatorArgs]) -> list[dict[str, Any]]:
        """Runs `add_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_operator", items)

    def add_operator_at(self,
        *,
        node: str,
        kind: OperatorKind,
        shape: list[int],
        output_width: int,
        x: int,
        y: int,
    ) -> None:
        """Adds one operator with exactly the shape, matrix width and place a caller gives it. The device checks the whole design before any of it is kept."""
        return self.action("add_operator_at", node=node, kind=kind, shape=shape, output_width=output_width, x=x, y=y)

    def add_operator_at_many(self, items: list[AddOperatorAtArgs]) -> list[dict[str, Any]]:
        """Runs `add_operator_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_operator_at", items)

    def set_shape(self,
        *,
        op: Operator,
        shape: list[int],
    ) -> None:
        """Reshapes one operator. Its last axis sets the normalization width, and every connection the new shape cannot hold stops holding."""
        return self.action("set_shape", op=op, shape=shape)

    def set_shape_many(self, items: list[SetShapeArgs]) -> list[dict[str, Any]]:
        """Runs `set_shape` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_shape", items)

    def apply_shape(self, *, op: Operator) -> None:
        """Applies the shape a person typed for one operator, when it names one."""
        return self.action("apply_shape", op=op)

    def apply_shape_many(self, items: list[ApplyShapeArgs]) -> list[dict[str, Any]]:
        """Runs `apply_shape` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply_shape", items)

    def apply_width(self, *, op: Operator) -> None:
        """Applies the matrix output width a person typed for one operator."""
        return self.action("apply_width", op=op)

    def apply_width_many(self, items: list[ApplyWidthArgs]) -> list[dict[str, Any]]:
        """Runs `apply_width` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply_width", items)

    def tap_macro(self,
        *,
        s: Stage,
        index: int,
    ) -> None:
        """Shows one placement macro of the stage the window holds open."""
        return self.action("tap_macro", s=s, index=index)

    def tap_macro_many(self, items: list[TapMacroArgs]) -> list[dict[str, Any]]:
        """Runs `tap_macro` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("tap_macro", items)

    def set_output_width(self,
        *,
        op: Operator,
        width: int,
    ) -> None:
        """Sets the matrix output width of one operator."""
        return self.action("set_output_width", op=op, width=width)

    def set_output_width_many(self, items: list[SetOutputWidthArgs]) -> list[dict[str, Any]]:
        """Runs `set_output_width` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_output_width", items)

    def move_operator(self,
        *,
        op: Operator,
        x: int,
        y: int,
    ) -> None:
        """Moves one operator to a place on the canvas."""
        return self.action("move_operator", op=op, x=x, y=y)

    def move_operator_many(self, items: list[MoveOperatorArgs]) -> list[dict[str, Any]]:
        """Runs `move_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_operator", items)

    def remove_operator(self, *, op: Operator) -> None:
        """Removes one operator, and every connection it held. Every other connection holds as it did: removing an operator takes only its own edges, and no other operator's ports change."""
        return self.action("remove_operator", op=op)

    def remove_operator_many(self, items: list[RemoveOperatorArgs]) -> list[dict[str, Any]]:
        """Runs `remove_operator` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_operator", items)

    def connect(self,
        *,
        from_: Operator,
        from_port: str,
        to: Operator,
        to_port: str,
    ) -> None:
        """Connects one output to one input. The device refuses a connection whose types differ, whose input another connection holds, or that would close a cycle."""
        args: dict[str, Any] = {}
        args["from"] = from_
        args["from_port"] = from_port
        args["to"] = to
        args["to_port"] = to_port
        return self.action("connect", **args)

    def connect_many(self, items: list[ConnectArgs]) -> list[dict[str, Any]]:
        """Runs `connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("connect", items)

    def disconnect(self,
        *,
        op: Operator,
        port: str,
    ) -> None:
        """Disconnects every connection into one input."""
        return self.action("disconnect", op=op, port=port)

    def disconnect_many(self, items: list[DisconnectArgs]) -> list[dict[str, Any]]:
        """Runs `disconnect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("disconnect", items)

    def resolve_graph(self) -> None:
        """Resolves the whole design again, so what the window shows is what the stored design makes."""
        return self.action("resolve_graph")

    def resolve_graph_many(self, items: list[ResolveGraphArgs]) -> list[dict[str, Any]]:
        """Runs `resolve_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resolve_graph", items)

    def apply_graph(self, *, answer: str) -> None:
        """Keeps the ports and connection outcomes the device answered, so the window draws and refuses by the design's own resolution."""
        return self.action("apply_graph", answer=answer)

    def apply_graph_many(self, items: list[ApplyGraphArgs]) -> list[dict[str, Any]]:
        """Runs `apply_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply_graph", items)

    def inspect_seen_macro(self,
        *,
        s: Stage,
        m: Macro,
        r: Receipt | None = None,
    ) -> None:
        """Shows one macro the person picked from the placement's own list."""
        args: dict[str, Any] = {}
        if r is not None:
            args["r"] = r
        args["s"] = s
        args["m"] = m
        return self.action("inspect_seen_macro", **args)

    def inspect_seen_macro_many(self, items: list[InspectSeenMacroArgs]) -> list[dict[str, Any]]:
        """Runs `inspect_seen_macro` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("inspect_seen_macro", items)

    def tap_graph(self, *, index: int) -> None:
        """A press on the canvas: an operator's title opens its facts, an output is what the next input connects from, and an input connects or opens its operator."""
        return self.action("tap_graph", index=index)

    def tap_graph_many(self, items: list[TapGraphArgs]) -> list[dict[str, Any]]:
        """Runs `tap_graph` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("tap_graph", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"import_file", "import_file_many", "import_report", "import_report_many", "import_verification", "import_verification_many", "take_in", "take_in_many", "keep_report", "keep_report_many", "keep_receipt", "keep_receipt_many", "remove_report", "remove_report_many", "remove_verification", "remove_verification_many", "show_graph", "show_graph_many", "show_all", "show_all_many", "inspect_report", "inspect_report_many", "inspect_receipt", "inspect_receipt_many", "inspect_stage", "inspect_stage_many", "inspect_macro", "inspect_macro_many", "open_subflow", "open_subflow_many", "dismiss", "dismiss_many", "zoom_graph", "zoom_graph_many", "select_output", "select_output_many", "clear_output", "clear_output_many", "inspect_operator", "inspect_operator_many", "close_operator", "close_operator_many", "add_operator", "add_operator_many", "add_operator_at", "add_operator_at_many", "set_shape", "set_shape_many", "apply_shape", "apply_shape_many", "apply_width", "apply_width_many", "tap_macro", "tap_macro_many", "set_output_width", "set_output_width_many", "move_operator", "move_operator_many", "remove_operator", "remove_operator_many", "connect", "connect_many", "disconnect", "disconnect_many", "resolve_graph", "resolve_graph_many", "apply_graph", "apply_graph_many", "inspect_seen_macro", "inspect_seen_macro_many", "tap_graph", "tap_graph_many"})


class ImportAsk(Resource):
    """ImportAsk — `tl.so.tilo.chip-design.ImportAsk`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    fmuu7ss2: str  # json


class NodeAsk(Resource):
    """NodeAsk — `tl.so.tilo.chip-design.NodeAsk`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    zcgjq6hw: str  # id
    xj4chp77: OperatorKind  # kind
    qzhhc7sz: list[int]  # shape
    gpmyx3jk: int  # output_width
    lcejfaxh: int  # x
    sjxnosz6: int  # y


class EdgeAsk(Resource):
    """EdgeAsk — `tl.so.tilo.chip-design.EdgeAsk`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    cem5h5og: str  # from_node
    og4slduv: str  # from_port
    esecrerr: str  # to_node
    @property
    def _7ydvdfsk(self) -> str:
        """to_port (`7ydvdfsk` on the wire)."""
        return self.fields["7ydvdfsk"]

    @_7ydvdfsk.setter
    def _7ydvdfsk(self, value: str) -> None:
        self.edit("7ydvdfsk", value=value)


class GraphAsk(Resource):
    """GraphAsk — `tl.so.tilo.chip-design.GraphAsk`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    @property
    def _6ydpdgb2(self) -> list[NodeAsk]:
        """nodes (`6ydpdgb2` on the wire)."""
        return self.fields["6ydpdgb2"]

    @_6ydpdgb2.setter
    def _6ydpdgb2(self, value: list[NodeAsk]) -> None:
        self.edit("6ydpdgb2", value=value)
    @property
    def _5sfjy4uo(self) -> list[EdgeAsk]:
        """connections (`5sfjy4uo` on the wire)."""
        return self.fields["5sfjy4uo"]

    @_5sfjy4uo.setter
    def _5sfjy4uo(self, value: list[EdgeAsk]) -> None:
        self.edit("5sfjy4uo", value=value)


class OutcomeOf(Resource):
    """OutcomeOf — `tl.so.tilo.chip-design.OutcomeOf`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"
    bf2avrry: str  # digest
    wfpvvvxh: bool  # active
    @property
    def _7ezbtipq(self) -> str:
        """reason (`7ezbtipq` on the wire)."""
        return self.fields["7ezbtipq"]

    @_7ezbtipq.setter
    def _7ezbtipq(self, value: str) -> None:
        self.edit("7ezbtipq", value=value)


class ChipDesign(Resource):
    """Chip Design — `tl.so.tilo.chip-design`."""
    __surface__ = "so.tilo.chip-design@dee902aae2b72415"

