// The big-folder story: a person copies a large folder (a photo library and a
// few long videos) into their workspace through its mounted folder, browses
// it, joins from a second device, opens a photo and a video there, and
// restarts the first device; every file is still there on both.
//
// It walks the fs daemons' size-ladder pass (tools/ci/fs-workspace-scale.sh
// daemons, crates/system/fs/tests/native_sync_bench.rs): two real fs daemons
// with their FUSE mounts, the second joining the first over the network and
// fetching on demand, as two of a person's computers do. The pass reports
// each moment it measures; each step here is one of them against the step's
// budget (budgets.json), and the copy rate, the store's size against the
// content and the files lost are measures with targets of their own.
//
// usage: node big-folder.mjs SITE_URL OUT_DIR
// SITE_URL is unused (the story has no stack); sizes are in big-folder.json.
// Needs cargo and /dev/fuse (in a user namespace that may mount, as the
// fs-workspace-scale check runs it).
import {spawn} from 'node:child_process';
import {createWriteStream, readFileSync} from 'node:fs';
import {join} from 'node:path';
import {recorder, StoryStopped} from './recorder.mjs';

const [, , , out] = process.argv;
if (!out) {
  console.error('usage: node big-folder.mjs SITE_URL OUT_DIR');
  process.exit(2);
}
const repo = new URL('../../../../', import.meta.url).pathname;
const size = JSON.parse(readFileSync(new URL('./big-folder.json', import.meta.url), 'utf8'));
const log = join(out, 'bench.log');
const story = recorder({id: 'big-folder', out, logs: [log]});

const mib = bytes => bytes / (1024 * 1024);
const photoMib = mib(size.photos.count * size.photos.bytes);
const totalMib = Math.ceil(photoMib + size.videos.count * size.videos.mib);

/** The ladder pass at the story's size: every BENCH line it printed, by metric. */
const bench = () => new Promise((resolve, reject) => {
  const metrics = {};
  const sink = createWriteStream(log);
  const child = spawn('bash', ['tools/ci/fs-workspace-scale.sh', 'daemons'], {
    cwd: repo,
    env: {
      ...process.env,
      TILO_FS_LADDER_TOTAL_MIB: String(totalMib),
      TILO_FS_LADDER_SMALL_FILES: String(size.photos.count),
      TILO_FS_LADDER_SMALL_BYTES: String(size.photos.bytes),
      TILO_FS_LADDER_HUGE_FILES: String(size.videos.count),
      TILO_FS_LADDER_DIR_ENTRIES: String(size.folderEntries),
      TILO_FS_LADDER_VERIFY_ALL: '1',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let rest = '';
  // What the pass said when it gave up: the line after its panic.
  let said = '';
  let errors = '';
  const hear = chunk => {
    sink.write(chunk);
    errors = (errors + chunk).slice(-20_000);
    const panic = /panicked at [^\n]*\n([^\n]+)/.exec(errors);
    if (panic && !said) said = panic[1].replace(/; writer's log:.*/, '').slice(0, 400);
    // A build that failed never ran the pass: its first error says why.
    const built = /^error(?:\[E\d+\])?: (.+)$/m.exec(errors);
    if (built && !said && !/test failed/.test(built[1])) said = `it did not build: ${built[1].slice(0, 300)}`;
  };
  const take = chunk => {
    sink.write(chunk);
    const lines = (rest + chunk).split('\n');
    rest = lines.pop();
    for (const line of lines) {
      const bench = /^BENCH \S+ (\S+) (\S+) (\S+)/.exec(line);
      if (bench) {
        metrics[bench[1]] = Number(bench[2]);
        console.log(line);
      }
    }
  };
  child.stdout.on('data', take);
  child.stderr.on('data', hear);
  child.on('error', reject);
  child.on('close', code => { sink.end(); resolve({code, metrics, said}); });
});

/** A metric the pass reported, or the step fails saying which it never reached. */
const metric = (metrics, name) => {
  if (!Number.isFinite(metrics[name])) {
    throw new Error(run.said ? `the daemons' pass stopped: ${run.said}` : `the daemons' pass ended (see bench.log) before it measured ${name}`);
  }
  return metrics[name];
};

let run = {metrics: {}};
let error = null;
try {
  await story.step('Ada copies the folder into their workspace folder', async () => {
    // The pass builds first; only the copy itself is the step's time.
    run = await bench();
    const small = metric(run.metrics, 'put_small_s');
    const huge = metric(run.metrics, 'put_huge_s');
    story.measure('copy-files-per-s', metric(run.metrics, 'put_small_files_s'), `${size.photos.count} photos of ${size.photos.bytes / 1024} KiB`);
    story.measure('copy-video-mib-per-s', metric(run.metrics, 'put_huge_mib_s'), `${size.videos.count} videos of ${size.videos.mib} MiB`);
    return {tookMs: (small + huge) * 1000};
  }, {limit: 3 * 3600_000});

  await story.step('Ada browses the big folder', async () => {
    const entries = metric(run.metrics, 'host_flat_entries');
    if (entries !== size.folderEntries) throw new Error(`the folder lists ${entries} of the ${size.folderEntries} photos copied into it, on the computer that copied them`);
    return {tookMs: metric(run.metrics, 'host_list_flat_s') * 1000};
  });

  await story.step('Ada’s second computer joins and shows the workspace', async () => {
    const fetched = mib(metric(run.metrics, 'peer_join_fetched_bytes'));
    story.measure('join-fetched-mib', fetched, 'what the second computer holds once the tree shows');
    const entries = metric(run.metrics, 'list_flat_entries');
    if (entries !== size.folderEntries) throw new Error(`the second computer lists ${entries} of the folder's ${size.folderEntries} files`);
    return {tookMs: metric(run.metrics, 'peer_join_tree_s') * 1000};
  });

  await story.step('Ada opens a photo the second computer never fetched', async () =>
    ({tookMs: metric(run.metrics, 'peer_open_small_s') * 1000}));

  await story.step('Ada starts a video on the second computer', async () =>
    ({tookMs: metric(run.metrics, 'peer_open_huge_s') * 1000}));

  await story.step('Ada restarts the first computer', async () =>
    ({tookMs: (metric(run.metrics, 'host_restart_s') + metric(run.metrics, 'host_restart_first_list_s')) * 1000}));

  await story.step('Every file is still there, on both computers', async () => {
    story.measure('store-per-content', metric(run.metrics, 'host_disk_after_fill_mib') / mib(metric(run.metrics, 'total_bytes')),
      'the first computer\'s store against what was copied');
    const lost = ['host', 'peer'].map(side =>
      metric(run.metrics, `${side}_missing_after_restart`) + metric(run.metrics, `${side}_wrong_after_restart`));
    story.measure('files-lost', lost[0], 'missing or not as copied, on the first computer after its restart');
    story.measure('files-lost-on-second', lost[1], 'missing or not as copied, on the second computer');
    if (lost[0] || lost[1]) {
      const checked = metric(run.metrics, 'host_files_checked');
      throw new Error(`of ${checked} files copied, ${lost[0]} are missing or wrong on the first computer and ${lost[1]} on the second`);
    }
    if (run.code !== 0) throw new Error(`the daemons' pass failed (exit ${run.code}; see bench.log)`);
  });
} catch (caught) {
  if (!(caught instanceof StoryStopped)) error = caught;
}
const result = await story.finish({error, extra: {logs: [{name: 'the daemons’ log', file: 'bench.log'}]}});
process.exit(result.completed && result.withinBudget ? 0 : 1);
