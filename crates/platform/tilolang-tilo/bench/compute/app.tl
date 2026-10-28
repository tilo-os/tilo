app so.tilo.lang.bench.compute "Compute" (icon: .hash, category: .utility)

/// Where the workloads leave their results, so that each engine's can be compared.
type Lab {
    result: Int
}

root Lab

default view lab for Lab

/// Three rounds of a 64-bit linear congruential step, folded to a small number: the integer
/// multiplies, adds and divisions of a hash or a pixel kernel.
fn mix(x: Int) -> Int {
    let a = x * 6364136223846793005 + 1442695040888963407
    let b = a * 6364136223846793005 + 1442695040888963407
    let c = b * 6364136223846793005 + 1442695040888963407
    (c / 65536) % 1000003
}

/// The sum of `mix` over every index in [lo, hi), halving the range down to single indices:
/// guest calls and arithmetic only, no host call.
fn sweep(lo: Int, hi: Int) -> Int {
    if hi - lo == 1 {
        mix(lo)
    } else {
        let mid = (lo + hi) / 2
        sweep(lo, mid) + sweep(mid, hi)
    }
}

/// `sweep` over `n` indices.
action sweep_range(lab: Lab, n: Int) {
    lab.result = sweep(0, n)
}

/// The sum of `mix` over 32^4 = 1048576 indices, as four nested loops over a list: each step
/// reads an element of the list through the host.
action nested_loops(lab: Lab) {
    let k = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31]
    let total = 0
    for a in k {
        for b in k {
            for c in k {
                for d in k {
                    total = total + mix(((a * 32 + b) * 32 + c) * 32 + d)
                }
            }
        }
    }
    lab.result = total
}

view lab(l: Lab) {
    page {
        text("{l.result}")
    }
}
