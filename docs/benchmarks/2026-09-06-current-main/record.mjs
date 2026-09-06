import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { closeSync, openSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const [planFile, name] = process.argv.slice(2);
if (!planFile || !name) throw new Error('usage: node record.mjs PLAN_JSON JOB_NAME');
const plan = JSON.parse(readFileSync(planFile, 'utf8'));
const job = plan.jobs.find(item => item.name === name);
if (!job) throw new Error('unknown job: ' + name);
if (!Array.isArray(job.args) || !job.command || !job.cwd || !job.log) {
  throw new Error('job requires command, args, cwd, log');
}
const log = resolve(job.log);
const resultFile = log + '.result.json';
const startFile = log + '.started.json';
const env = { ...process.env };
for (const key of plan.clear_env ?? []) delete env[key];
for (const [key, value] of Object.entries(job.env ?? {})) env[key] = String(value);
const envPattern = /^(RUSTFLAGS$|RUSTC|CARGO_ENCODED_RUSTFLAGS$|CARGO_BUILD_|CARGO_TARGET_DIR$|CARGO_PROFILE_RELEASE_|VELLO_|QUICK_UI_|QOS_REDRAW_BENCH_|QOS_DRM_XRGB_DAMAGE$|QOS_SHOTS_THEME$|MESA_|GALLIUM_|LP_NUM_THREADS$|VK_ICD_FILENAMES$|VK_DRIVER_FILES$|WGPU_BACKEND$)/;
const started = new Date().toISOString();
const start = {
  ...job, plan_file: resolve(planFile),
  plan_sha256: createHash('sha256').update(readFileSync(planFile)).digest('hex'),
  started, cleared_env: plan.clear_env ?? [],
  effective_relevant_env: Object.fromEntries(Object.entries(env).filter(([key]) => envPattern.test(key))),
};
writeFileSync(startFile, JSON.stringify(start, null, 2) + '\n', { flag: 'wx' });
const fd = openSync(log, 'wx');
const launchedAt = performance.now();
const child = spawn(job.command, job.args, { cwd: job.cwd, env, stdio: ['ignore', fd, fd] });
console.log(JSON.stringify({ name, started, child_pid: child.pid, log }));
let spawnError = null;
child.on('error', error => { spawnError = String(error); });
child.on('close', (exitCode, signal) => {
  closeSync(fd);
  const bytes = readFileSync(log);
  const result = {
    ...start, finished: new Date().toISOString(), elapsed_ms: performance.now() - launchedAt,
    exit_code: exitCode, signal, spawn_error: spawnError,
    log_bytes: bytes.length, log_sha256: createHash('sha256').update(bytes).digest('hex'),
    output_truncated: false,
  };
  writeFileSync(resultFile, JSON.stringify(result, null, 2) + '\n', { flag: 'wx' });
  console.log(JSON.stringify({ name, exit_code: exitCode, signal, spawn_error: spawnError, elapsed_ms: result.elapsed_ms, log_bytes: bytes.length, result_file: resultFile }));
  process.exitCode = spawnError || signal || exitCode !== 0 ? 1 : 0;
});
