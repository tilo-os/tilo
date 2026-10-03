//! WPE WebKit platform probes and latency reproductions.

#[path = "registry.rs"]
mod registry;

registry::scenario_modules! {
    Wpe;
    wpe_dmabuf_probe, "wpe_dmabuf_probe.rs", "Vulkan DMA-BUF import capability";
    wpe_latency_bench, "wpe_latency_bench.rs", "WebView throughput and input latency";
    wpe_scroll_repro, "wpe_scroll_repro.rs", "embedded page scrolling reproduction";
}

fn main() {
    registry::dispatch(registry::Surface::Wpe, SCENARIOS, None);
}
