#!/usr/bin/env python3
"""Compare production RTL with an independent integer oracle and synthesize it.

Only synthetic coefficients are used. Missing tools are failures, not skipped
checks. A successful result proves this arithmetic/protocol contract only.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import signal
import subprocess

from integer_reference import MAX_GROUP_SIZE, MAX_LANES, group_dot

ROOT = Path(__file__).resolve().parent
SEED = 20261006


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixtures(lanes, size):
    rng = random.Random(SEED)
    weights = [[rng.randrange(-8, 8) for _ in range(size)] for _ in range(lanes)]
    weights[0] = [-8] * size
    if lanes > 1:
        weights[1] = [7] * size
    inputs = [[-128] * size, [127] * size, [0] * size,
              [-128 if index % 2 == 0 else 127 for index in range(size)]]
    inputs.extend([[rng.randrange(-128, 128) for _ in range(size)] for _ in range(8)])
    expected = [group_dot(vector, weights) for vector in inputs]
    return weights, inputs, expected


def wrapper(weights):
    lanes, size = len(weights), len(weights[0])
    packed = 0
    for index, weight in enumerate(value for row in weights for value in row):
        packed |= (weight & 15) << (index * 4)
    bits = lanes * size * 4
    return f"""// Generated SYNTHETIC coefficient fixture, seed {SEED}.
module fixed_linear_fixture (
    input logic clk, reset_n, in_valid,
    output logic in_ready,
    input logic signed [7:0] activation,
    output logic out_valid,
    input logic out_ready,
    output logic [{lanes * 32 - 1}:0] out_sums
);
    integer_group_tile #(.LANES({lanes}), .GROUP_SIZE({size}),
        .WEIGHTS({bits}'h{packed:x})) tile (.*);
endmodule
"""


def bench(inputs, expected):
    size, groups, lanes = len(inputs[0]), len(inputs), len(expected[0])
    values = ",".join(f"8'h{value & 255:02x}" for vector in inputs for value in vector)
    answers = ",".join("'{" + ",".join(map(str, row)) + "}" for row in expected)
    return f"""// Independent oracle answers and SYNTHETIC stimulus.
module tb;
    localparam integer LANES={lanes}, SIZE={size}, GROUPS={groups};
    localparam integer INPUTS=SIZE*GROUPS;
    logic clk=0, reset_n=0, in_valid=0, out_ready=0;
    logic in_ready, out_valid;
    logic signed [7:0] activation=0;
    logic [LANES*32-1:0] out_sums;
    logic signed [7:0] vectors [INPUTS] = '{{{values}}};
    integer answers [GROUPS][LANES] = '{{{answers}}};
    integer sent=0, received=0, stalls=0, overlap=0, cycle=0;
    logic previous_stall=0;
    logic [LANES*32-1:0] previous_payload;
    fixed_linear_fixture dut (.*);
    always #5 clk = !clk;

    task automatic reset_dut;
        @(negedge clk);
        reset_n=0; in_valid=0; out_ready=0;
        @(posedge clk); #1;
        if (out_valid !== 1'b0) $fatal(1, "reset retained a result");
        @(negedge clk); reset_n=1;
    endtask

    initial begin
        reset_dut();
        // Reset a completed but unconsumed result.
        repeat (SIZE) begin
            @(negedge clk); in_valid=1; activation=8'h80;
            @(posedge clk);
            if (!in_ready) $fatal(1, "unexpected input stall before first result");
        end
        @(negedge clk); in_valid=0;
        if (!out_valid) $fatal(1, "completed group produced no result");
        reset_dut();
        // Reset halfway through a group; no partial state may leak afterward.
        repeat (SIZE/2) begin
            @(negedge clk); in_valid=1; activation=8'h7f;
            @(posedge clk);
            if (!in_ready) $fatal(1, "unexpected partial-group stall");
        end
        reset_dut();

        while (received < GROUPS && cycle < INPUTS*12+100) begin
            @(negedge clk);
            in_valid = sent < INPUTS && (out_valid || cycle % 7 != 0);
            if (sent < INPUTS) activation = vectors[sent];
            // Deliberately hold the first output for three complete cycles.
            out_ready = !(out_valid && received == 0 && stalls < 3)
                && cycle % 5 != 0;
            @(posedge clk);
            if (previous_stall && (!out_valid || out_sums !== previous_payload))
                $fatal(1, "result changed while stalled");
            if (out_valid && out_ready) begin
                if (received >= GROUPS) $fatal(1, "unexpected extra result");
                for (integer lane=0; lane<LANES; lane++)
                    if ($signed(out_sums[lane*32 +: 32]) !== answers[received][lane])
                        $fatal(1, "oracle mismatch group=%0d lane=%0d", received, lane);
                received++;
                if (in_valid && in_ready) overlap++;
            end
            if (out_valid && !out_ready) stalls++;
            previous_stall = out_valid && !out_ready;
            previous_payload = out_sums;
            if (in_valid && in_ready) sent++;
            cycle++;
        end
        if (received != GROUPS || sent != INPUTS || stalls < 3 || overlap == 0)
            $fatal(1, "incomplete protocol coverage sent=%0d received=%0d", sent, received);
        @(negedge clk); in_valid=0; out_ready=1;
        repeat (3) begin
            @(posedge clk);
            if (out_valid) $fatal(1, "extra result after final output");
        end
        $display("RTL_PASS groups=%0d inputs=%0d stalls=%0d overlap=%0d", GROUPS, INPUTS, stalls, overlap);
        $finish;
    end
endmodule
"""


def run(command, log, timeout=120):
    with log.open("w") as output:
        with subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=output,
                              stderr=subprocess.STDOUT, start_new_session=True) as process:
            try:
                status = process.wait(timeout=timeout)
            except (subprocess.TimeoutExpired, KeyboardInterrupt):
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                raise
    if status:
        raise RuntimeError(f"command failed ({status}); see {log}")
    with log.open() as output:
        text = output.read(16_000_001)
    if len(text) > 16_000_000:
        raise RuntimeError(f"tool output exceeds the 16 MB read limit; see {log}")
    return text


def verify(args):
    if not 1 <= args.lanes <= MAX_LANES or not 1 <= args.group_size <= MAX_GROUP_SIZE:
        raise ValueError("unsupported tile dimensions")
    args.out.mkdir(parents=True, exist_ok=False)
    weights, inputs, expected = fixtures(args.lanes, args.group_size)
    fixture = args.out / "fixed_linear_fixture.sv"
    tb = args.out / "tb.sv"
    fixture.write_text(wrapper(weights))
    tb.write_text(bench(inputs, expected))
    vectors = args.out / "vectors.json"
    vectors.write_text(json.dumps({"seed": SEED, "synthetic": True,
                                  "weights": weights, "inputs": inputs, "expected": expected}, indent=2))
    rtl = ROOT / "rtl/integer_group_tile.sv"
    record = {"schema_version": 1, "status": "running", "evidence": "rtl_contract_check",
              "synthetic_coefficients": True, "lanes": args.lanes, "group_size": args.group_size,
              "input_sha256": {"rtl/integer_group_tile.sv": digest(rtl),
                               "integer_reference.py": digest(ROOT / "integer_reference.py"),
                               "verify_tile.py": digest(Path(__file__)),
                               **{f"generated/{path.name}": digest(path) for path in [fixture, tb, vectors]}},
              "declared_toolchain_image": args.toolchain_image,
              "tools": {}, "checks": {},
              "excludes": ["model_quality", "checkpoint_weight_validation", "group_rescaling", "full_model_execution",
                           "process_mapping", "physical_area", "power", "timing_closure"]}
    try:
        verilator = shutil.which(args.verilator)
        if verilator is None:
            raise RuntimeError("Verilator unavailable; production RTL was not observed")
        record["tools"]["verilator"] = {"version": run([verilator, "--version"], args.out / "verilator-version.log").strip(),
                                         "executable_sha256": digest(Path(verilator).resolve())}
        obj = args.out / "obj"
        command = [verilator, "--binary", "--timing", "--assert", "--timescale-override", "1ns/1ps",
                   "--top-module", "tb", "--Mdir", str(obj), "-o", "tile-sim", str(rtl), str(fixture), str(tb)]
        run(command, args.out / "compile.log", 180)
        output = run([str(obj / "tile-sim")], args.out / "simulation.log")
        if "RTL_PASS " not in output:
            raise RuntimeError("simulator exited without proving the expected protocol observations")
        record["checks"]["rtl_vs_integer_oracle"] = "passed"
        yosys = shutil.which(args.yosys)
        if yosys is None:
            raise RuntimeError("Yosys unavailable; synthesis was not observed")
        record["tools"]["yosys"] = {"version": run([yosys, "-V"], args.out / "yosys-version.log").strip(),
                                     "executable_sha256": digest(Path(yosys).resolve())}
        netlist = args.out / "synthesized.v"
        if any('"' in str(path) or '\n' in str(path) for path in [rtl, fixture, netlist]):
            raise ValueError("EDA source paths cannot contain quotes or newlines")
        script = f'read_verilog -sv -DSYNTHESIS "{rtl}" "{fixture}"; synth -top fixed_linear_fixture; check -assert; stat -json; write_verilog -noattr "{netlist}"'
        run([yosys, "-p", script], args.out / "synthesis.log")
        record["checks"]["generic_synthesis"] = "passed"
        gate_obj = args.out / "obj-netlist"
        command = [verilator, "--binary", "--timing", "--assert", "--timescale-override", "1ns/1ps",
                   "--top-module", "tb", "--Mdir", str(gate_obj), "-o", "netlist-sim", str(netlist), str(tb)]
        run(command, args.out / "netlist-compile.log", 180)
        output = run([str(gate_obj / "netlist-sim")], args.out / "netlist-simulation.log")
        if "RTL_PASS " not in output:
            raise RuntimeError("synthesized netlist did not prove the expected observations")
        record["checks"]["synthesized_netlist_vs_integer_oracle"] = "passed"
        record["status"] = "passed"
    except (RuntimeError, OSError, subprocess.SubprocessError, ValueError) as error:
        record["status"] = "failed"
        record["failure"] = str(error)
        raise
    finally:
        record["artifact_sha256"] = {path.name: digest(path) for path in sorted(args.out.glob("*.log"))}
        if (args.out / "synthesized.v").exists():
            record["artifact_sha256"]["synthesized.v"] = digest(args.out / "synthesized.v")
        (args.out / "result.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--lanes", type=int, default=4)
    parser.add_argument("--group-size", type=int, default=128)
    parser.add_argument("--verilator", default="verilator")
    parser.add_argument("--yosys", default="yosys")
    parser.add_argument("--toolchain-image", help="Caller-declared immutable container image identity")
    args = parser.parse_args()
    try:
        verify(args)
    except (ValueError, OSError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"tile verification failed: {error}\n")
    print(f"RTL comparison and generic synthesis passed; evidence: {args.out / 'result.json'}")


if __name__ == "__main__":
    main()
