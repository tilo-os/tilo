import assert from 'node:assert/strict';
import { summarize } from './summarize.mjs';

const row = (workload, revision, values = {}) => ({
  name: `${workload}-${revision}-${values.name ?? 'run'}`,
  workload,
  revision,
  configuration: 'default',
  complete: true,
  mean_us: 100,
  p99_us: 200,
  max_us: 300,
  peak_rss_kib: 400,
  process_wall_s: 5,
  ...values,
});
const group = (summary, key) => summary.groups.find(value => value.key === key);
const comparison = (summary, key) => summary.comparisons.find(value => value.candidate === key);

{
  const summary = summarize([
    row('shell', 'main', { max_us: 0 }),
    row('shell', 'branch', { max_us: 0 }),
  ]);
  assert.equal(group(summary, 'shell/main/default').worst_us, 0);
  assert.equal(group(summary, 'shell/branch/default').worst_us, 0);
}
{
  const summary = summarize([
    row('shell', 'main', { max_us: 12 }),
    row('shell', 'main', { name: 'missing-max', max_us: undefined }),
  ]);
  assert.equal(group(summary, 'shell/main/default').worst_us, null);
}
{
  const summary = summarize([
    ...[0, 1, 2].map(index => row('ui-frozen', 'main', { name: index, mean_us: 10, process_wall_s: 10 })),
    ...[0, 1, 2].map(index => row('ui-frozen', 'branch', { name: index, mean_us: 2, process_wall_s: 5 })),
  ]);
  const result = comparison(summary, 'ui-frozen/branch/default');
  assert.equal(result.mean_time_change_percent, null);
  assert.equal(result.process_wall_time_change_percent, -50);
  assert.match(result.caveat, /integer-microsecond truncated/);
}
{
  const summary = summarize([
    ...[0, 1, 2].map(index => row('shell', 'main', { name: index })),
    ...[0, 1].map(index => row('shell', 'branch', { name: index })),
  ]);
  const result = comparison(summary, 'shell/branch/default');
  assert.equal(result.all_three_completed, false);
  assert.equal(result.mean_time_change_percent, null);
  assert.equal(result.p99_change_percent, null);
}
{
  const summary = summarize([
    ...[0, 1, 2].map(index => row('shell', 'main', { name: index, mean_us: 100, p99_us: 200 })),
    ...[0, 1, 2].map(index => row('shell', 'branch', { name: index, mean_us: 110, p99_us: 180 })),
    ...[0, 1, 2].map(index => row('ui', 'main', { name: index, mean_us: 80, p99_us: 100 })),
    ...[0, 1, 2].map(index => row('ui', 'branch', { name: index, mean_us: 100, p99_us: 120 })),
  ]);
  assert.equal(comparison(summary, 'shell/branch/default').mean_time_change_percent, 10.000000000000009);
  assert.equal(comparison(summary, 'shell/branch/default').p99_change_percent, -9.999999999999998);
  assert.equal(comparison(summary, 'ui/branch/default').mean_time_change_percent, 25);
  assert.equal(comparison(summary, 'ui/branch/default').p99_change_percent, 19.999999999999996);
}
console.log('summarize fixtures: PASS');
