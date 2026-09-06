import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

function one(log, pattern, optional = false) {
  const matches = [...log.matchAll(pattern)];
  if (optional && !matches.length) return null;
  assert.equal(matches.length, 1, 'expected one match: ' + pattern);
  return matches[0];
}
export function parse(job, meta, bytes) {
  const log = bytes.toString();
  const issues = [];
  const row = { name: job.name, revision: job.revision, workload: job.workload, configuration: job.configuration, round: job.round, exit_code: meta.exit_code, complete: false, issues };
  try {
    assert.equal(meta.output_truncated, false);
    assert.equal(meta.signal, null);
    assert.equal(meta.spawn_error, null);
    assert.equal(bytes.length, meta.log_bytes);
    assert.equal(createHash('sha256').update(bytes).digest('hex'), meta.log_sha256);
    assert.ok(!/Warning: truncated output|tokens truncated/.test(log));
    const adapter = one(log, /^(?:main benchmark adapter|benchmark adapter|DRM bridge adapter|UI renderer adapter): (.+)$/gm)[1];
    assert.match(adapter, /llvmpipe \(LLVM 22\.1\.8, 128 bits\)/);
    assert.match(adapter, /backend=Vulkan device_type=Cpu/);
    row.adapter = adapter;
    row.peak_rss_kib = Number(one(log, /Maximum resident set size \(kbytes\):\s*(\d+)/g)[1]);
    const wall = one(log, /Elapsed \(wall clock\) time \(h:mm:ss or m:ss\):\s*([\d:.]+)/g)[1].split(':').map(Number);
    row.process_wall_s = wall.reduce((total, part) => total * 60 + part, 0);
    assert.ok(Number.isFinite(row.process_wall_s));
    if (job.workload === 'shell') {
      const timing = one(log, /(?:main )?DRM render\/readback (?:at )?3840x2160: samples=(\d+) (?:total_us=(\d+) )?mean=(\d+)us p99=(\d+)us min=(\d+)us max=(\d+)us/g);
      row.samples = Number(timing[1]);
      row.reported_mean_us = Number(timing[3]);
      row.p99_us = Number(timing[4]); row.min_us = Number(timing[5]); row.max_us = Number(timing[6]);
      if (job.revision === 'main') {
        assert.ok(timing[2]);
        row.total_us = Number(timing[2]);
        assert.equal(meta.exit_code, 0);
      } else {
        const total = one(log, /DRM timing total: total_us=(\d+) samples=(\d+)/g);
        row.total_us = Number(total[1]);
        assert.equal(Number(total[2]), row.samples);
      }
      assert.equal(row.samples, 1000);
      row.mean_us = row.total_us / row.samples;
      assert.equal(Math.floor(row.mean_us), row.reported_mean_us);
      assert.ok([0, 1].includes(meta.exit_code));
      assert.ok(row.min_us > 0 && row.max_us >= row.p99_us);
      row.numeric_gate_pass = row.mean_us <= 25000 && row.p99_us <= 50000;
      row.overflow_verified = job.revision !== 'main';
      if (job.revision === 'main') {
        assert.match(log, /main bridge limitation: no bump settlement/);
        assert.match(log, /main XRGB destination/);
      } else {
        const attempts = one(log, /Vello render attempts: total=(\d+) sparse=(\d+) full=(\d+) recovery=(\d+)/g);
        row.attempts = Number(attempts[1]); row.sparse = Number(attempts[2]); row.full = Number(attempts[3]); row.recovery = Number(attempts[4]);
        assert.equal(row.attempts, row.sparse + row.full);
        assert.equal(row.attempts, 1000 + row.recovery);
        const writes = one(log, /CPU XRGB writes: full=(\d+) partial=(\d+) total=(\d+) bytes=(\d+)/g);
        row.cpu_xrgb_write_bytes = Number(writes[4]);
        assert.equal(Number(writes[3]), 1000);
      }
    } else {
      const header = one(log, /=== Dashboard bench \((\d+) events, canvas 3840×2160\) ===/g);
      row.events = Number(header[1]);
      row.redraws = Number(one(log, /^redrawn events:\s+(\d+)/gm)[1]);
      row.skipped = Number(one(log, /^skipped events:\s+(\d+)/gm)[1]);
      assert.equal(row.events, row.redraws + row.skipped);
      assert.equal(row.events, job.workload === 'ui-frozen' ? 1000000 : 1000);
      const total = one(log, /^TOTAL\/event\s+(\d+)\s+(\d+)\s+(\d+)\s*$/gm);
      row.reported_mean_us = Number(total[1]); row.p99_us = Number(total[3]);
      row.mean_us = row.reported_mean_us;
      assert.ok([0, 2].includes(meta.exit_code));
      if (job.workload === 'ui-frozen') {
        assert.equal(row.redraws, 0); assert.equal(row.skipped, 1000000);
        row.numeric_gate_pass = null;
      } else {
        assert.equal(row.redraws, 1000); assert.equal(row.skipped, 0);
        row.numeric_gate_pass = row.p99_us <= 30000;
        if (job.revision !== 'main') {
          const gate = one(log, /frame-budget: (PASS|FAIL) budget=30000us mean=(\d+)us p99=(\d+)us max=(\d+)us samples=(\d+)/g);
          assert.equal(Number(gate[5]), 1000); assert.equal(Number(gate[2]), row.reported_mean_us); assert.equal(Number(gate[3]), row.p99_us);
          row.max_us = Number(gate[4]);
        }
      }
      if (job.revision === 'main') {
        const exact = one(log, /benchmark timing: samples=(\d+) redraws=(\d+) total_us=(\d+) p99_rounded=(\d+)us max=(\d+)us cpu_target_readback_bytes=0 overflowed_submissions=(\d+)/g);
        assert.equal(Number(exact[1]), row.events);
        assert.equal(Number(exact[2]), row.redraws);
        row.total_us = Number(exact[3]);
        row.mean_us = row.total_us / row.events;
        assert.equal(Math.floor(row.mean_us), row.reported_mean_us);
        assert.equal(Number(exact[4]), row.p99_us);
        row.max_us = Number(exact[5]);
        row.overflowed_submissions = Number(exact[6]);
        assert.equal(row.overflowed_submissions, 0, 'main UI reported incomplete overflowed frames');
        assert.match(log, /benchmark validity: complete/);
      } else {
        assert.match(log, /CPU (?:target )?readback bytes: 0/);
      }
    }
    row.complete = true;
  } catch (error) { issues.push(error.message); }
  return row;
}

if (process.argv[1] && import.meta.url === new URL('file://' + process.argv[1]).href) {
  const plan = JSON.parse(readFileSync(process.argv[2], 'utf8'));
  const rows = plan.jobs.filter(job => job.kind === 'benchmark').map(job => {
    try { return parse(job, JSON.parse(readFileSync(job.log + '.result.json', 'utf8')), readFileSync(job.log)); }
    catch (error) { return { name: job.name, revision: job.revision, workload: job.workload, configuration: job.configuration, round: job.round, complete: false, issues: [error.message] }; }
  });
  console.log(JSON.stringify(rows, null, 2));
}
