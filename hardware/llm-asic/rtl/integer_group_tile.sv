// Signed W4A8 fixed-coefficient group projection, before group rescaling.
// WEIGHTS stores lane-major signed INT4 nibbles, input zero at each row's LSB.
// Configuration is an experimental arithmetic contract, not validated Qwen
// quantization. All reset behavior is synchronous to the rising clock edge.
module integer_group_tile #(
    parameter integer LANES = 4,
    parameter integer GROUP_SIZE = 128,
    parameter logic [LANES*GROUP_SIZE*4-1:0] WEIGHTS = '0
) (
    input  logic                      clk,
    input  logic                      reset_n,
    input  logic                      in_valid,
    output logic                      in_ready,
    input  logic signed [7:0]         activation,
    output logic                      out_valid,
    input  logic                      out_ready,
    output logic [LANES*32-1:0]       out_sums
);
    localparam integer INDEX_BITS = GROUP_SIZE > 1 ? $clog2(GROUP_SIZE) : 1;
    logic [INDEX_BITS-1:0] input_index;
    logic signed [31:0] accumulator [LANES];
    wire signed [11:0] product [LANES];

    // A stalled result prevents consuming activations. Consuming a result
    // can coincide with accepting the first activation of the following group.
    assign in_ready = reset_n && (!out_valid || out_ready);

    for (genvar lane = 0; lane < LANES; lane++) begin : coefficients
        wire signed [3:0] weight;
        assign weight = WEIGHTS[(lane*GROUP_SIZE + int'(input_index))*4 +: 4];
        assign product[lane] = activation * weight;
    end

    always_ff @(posedge clk) begin
        if (!reset_n) begin
            input_index <= '0;
            out_valid <= 1'b0;
            out_sums <= '0;
            for (integer lane = 0; lane < LANES; lane++)
                accumulator[lane] <= '0;
        end else begin
            if (out_valid && out_ready)
                out_valid <= 1'b0;
            if (in_valid && in_ready) begin
                for (integer lane = 0; lane < LANES; lane++) begin
                    if (input_index == INDEX_BITS'(GROUP_SIZE-1)) begin
                        out_sums[lane*32 +: 32] <= accumulator[lane]
                            + {{20{product[lane][11]}}, product[lane]};
                        accumulator[lane] <= '0;
                    end else begin
                        accumulator[lane] <= accumulator[lane]
                            + {{20{product[lane][11]}}, product[lane]};
                    end
                end
                if (input_index == INDEX_BITS'(GROUP_SIZE-1)) begin
                    input_index <= '0;
                    out_valid <= 1'b1;
                end else begin
                    input_index <= input_index + 1'b1;
                end
            end
        end
    end

`ifndef SYNTHESIS
    initial begin
        if (LANES < 1 || LANES > 64 || GROUP_SIZE < 1 || GROUP_SIZE > 5120)
            $fatal(1, "unsupported integer group dimensions");
    end
`endif
endmodule
