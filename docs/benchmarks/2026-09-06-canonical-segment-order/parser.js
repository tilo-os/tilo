const parseBenchmark = (output, workload) => {
  const p = ((output,workload)=>{
const p=((output,workload)=>{
 const ui=/frame-budget: (PASS|FAIL) budget=(\d+)us mean=(\d+)us p99=(\d+)us max=(\d+)us samples=(\d+)/.exec(output);
 const sh=/DRM render\/readback at 3840x2160: samples=(\d+) mean=(\d+)us p99=(\d+)us min=(\d+)us max=(\d+)us/.exec(output);
 const n=r=>r?Number(r[1]):null;
 const applied=(/sparse path tile bounds actual submissions: total=(\d+) active=(\d+) applied=(\d+) native_bbox_tiles=(\d+)/.exec(output)) || (/sparse submission accounting \(measured workload\): total=(\d+) active=(\d+) path_tile_bounds_applied=(\d+) path_tile_bounds_native_bbox_tiles=(\d+)/.exec(output));
 const quad=(/sparse fine quad skip actual submissions: applied=(\d+) total_invocation_groups=(\d+) active_invocation_groups=(\d+) skipped_invocation_groups=(\d+)/.exec(output)) || (/fine_quad_skip_applied=(\d+) fine_quad_total_invocation_groups=(\d+) fine_quad_active_invocation_groups=(\d+) fine_quad_skipped_invocation_groups=(\d+)/.exec(output));
 const attempts=/Vello render attempts: total=(\d+) sparse=(\d+) full=(\d+) recovery=(\d+)/.exec(output);
 const stats=workload==="ui"&&ui?{count:Number(ui[6]),mean_us:Number(ui[3]),p99_us:Number(ui[4]),max_us:Number(ui[5]),gate:ui[1]}:workload==="shell"&&sh?{count:Number(sh[1]),mean_us:Number(sh[2]),p99_us:Number(sh[3]),min_us:Number(sh[4]),max_us:Number(sh[5]),gate:/gates:.*\(Pass\)/.test(output)?"PASS":"FAIL"}:null;
 return {solid_fuse_selected:n((/sparse solid fuse shader-selected submissions: (\d+)/.exec(output)) || (/solid_fuse_shader_selected=(\d+)/.exec(output))),opaque_prune_selected:n((/sparse opaque prune shader-selected submissions: (\d+)/.exec(output)) || (/opaque_prune_shader_selected=(\d+)/.exec(output))),stats,peak_rss_kib:n(/Maximum resident set size \(kbytes\): (\d+)/.exec(output)),known_gpu_bytes:n(/^known GPU bytes: (\d+)/m.exec(output)),sparse:applied?{total:Number(applied[1]),active:Number(applied[2]),applied:Number(applied[3]),bbox_tiles:Number(applied[4])}:null,quad:quad?{applied:Number(quad[1]),total:Number(quad[2]),active:Number(quad[3]),skipped:Number(quad[4])}:null,attempts:attempts?{total:Number(attempts[1]),sparse:Number(attempts[2]),full:Number(attempts[3]),recovery:Number(attempts[4])}:null};
})(output,workload);
const matches=[...output.matchAll(/^(?:UI renderer adapter|DRM bridge adapter): name="([^"]+)" backend=(\w+) device_type=(\w+) driver="([^"]+)"/gm)];
const m=(/sparse segment row mask shader-selected submissions: (\d+)/.exec(output))||(/segment_row_mask_shader_selected=(\d+)/.exec(output));
const extent=(workload==="ui"?/^output target: (\d+)x(\d+) \(native; 1:1 damage raster, no upscale\)/m:/DRM render\/readback at (\d+)x(\d+):/).exec(output);
const cpu=/CPU readback bytes: (\d+)/.exec(output);
return {...p,native_extent:extent?[Number(extent[1]),Number(extent[2])]:null,cpu_readback_bytes:cpu?Number(cpu[1]):null,segment_row_mask_selected:m?Number(m[1]):null,adapter:matches.length===1?{name:matches[0][1],backend:matches[0][2],device_type:matches[0][3],driver:matches[0][4]}:null,adapter_headers:matches.length};
})(output, workload);
  const headers = [...output.matchAll(/^canonical segment order: env_requested=(true|false) renderer_requested=(true|false) available=(true|false) /gm)];
  const counts = [...output.matchAll(/^canonical segment order shader-selected attempts: full=(\d+) sparse=(\d+) total=(\d+)$/gm)];
  const totals = [...output.matchAll(/^DRM timing total: total_us=(\d+) samples=(\d+)$/gm)];
  const header = headers.length === 1 ? headers[0] : null;
  const count = counts.length === 1 ? counts[0] : null;
  const total = totals.length === 1 ? totals[0] : null;
  const canonical = {
    header_count: headers.length,
    counts_count: counts.length,
    env_requested: header ? header[1] === "true" : null,
    renderer_requested: header ? header[2] === "true" : null,
    available: header ? header[3] === "true" : null,
    full_selected_attempts: count ? Number(count[1]) : null,
    sparse_selected_attempts: count ? Number(count[2]) : null,
    total_selected_attempts: count ? Number(count[3]) : null,
    counts_consistent: !!count && Number(count[1]) + Number(count[2]) === Number(count[3])
  };
  const exact_total_us = total ? Number(total[1]) : null;
  const exact_count = total ? Number(total[2]) : null;
  const exact_mean_us = exact_count > 0 ? exact_total_us / exact_count : null;
  const exact_gate = workload === "shell" && p.stats && exact_count === p.stats.count
    ? (exact_total_us <= 25000 * exact_count && p.stats.p99_us <= 50000 ? "PASS" : "FAIL") : null;
  return {...p, canonical, exact_total_us, exact_count, exact_mean_us, exact_gate};
};

