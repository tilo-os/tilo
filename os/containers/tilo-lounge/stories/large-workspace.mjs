// The large-workspace story: the same steps at several workspace sizes, each
// size a workspace the generator makes (large-workspace.json), so a step
// whose time grows with the whole workspace, rather than with what it touches,
// is seen and named with the ratio between the sizes.
//
// usage: node large-workspace.mjs SITE_URL OUT_DIR [SERVICE_LOG…]
// with STORY_STACK naming the stack's stack.sh (for its environment). Writes
// OUT_DIR/story.json and timings.json (stories/recorder.mjs), with `sizes` and
// `growth`, and exits non-zero when a step failed, ran over its budget, or
// grew past what large-workspace.json allows.
import {execFileSync, spawnSync} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {recorder, StoryStopped} from './recorder.mjs';

const [site, out, ...logs] = process.argv.slice(2);
const stack = process.env.STORY_STACK;
if (!site || !out || !stack) {
  console.error('usage: STORY_STACK=…/stack.sh node large-workspace.mjs SITE_URL OUT_DIR [SERVICE_LOG…]');
  process.exit(2);
}
const here = dirname(fileURLToPath(import.meta.url));
const repo = join(here, '../../../..');
const plan = JSON.parse(readFileSync(join(here, 'large-workspace.json'), 'utf8'));
if (!plan.generator?.command || !plan.steps?.length || plan.sizes.length < 2) {
  console.error('large-workspace.json names no generator, no steps or fewer than two sizes');
  process.exit(2);
}

// The stack's environment, as `eval "$(stack.sh env)"` gives a shell: its API
// server and its owner's key.
const stackEnv = () => {
  const said = execFileSync('bash', ['-c', `eval "$(bash ${JSON.stringify(stack)} env)" && env -0`], {encoding: 'utf8'});
  return Object.fromEntries(said.split('\0').filter(Boolean).map(line => [line.slice(0, line.indexOf('=')), line.slice(line.indexOf('=') + 1)]));
};
const run = (command, env) => {
  const ran = spawnSync('bash', ['-c', command], {cwd: repo, env, encoding: 'utf8', maxBuffer: 64 << 20});
  if (ran.status !== 0) throw new Error(`\`${command}\` exited ${ran.status}: ${(ran.stderr || ran.stdout).slice(-800)}`);
  return ran.stdout;
};

const story = recorder({id: 'large-workspace', out, logs});
const times = new Map(plan.steps.map(step => [step.title, []]));
let error = null;
try {
  const env = {...stackEnv()};
  for (const size of plan.sizes) {
    const sized = {...env, STORY_SIZE: size.label, STORY_SIZE_PARAMS: JSON.stringify(size.params ?? {})};
    let workspace = '';
    await story.step(`The workspace is filled (${size.label})`, async () => {
      workspace = run(plan.generator.command, sized).trim().split('\n').at(-1);
      if (!workspace) throw new Error('the generator printed no workspace id');
    }, {budgetKey: 'The workspace is filled', limit: 6 * 3600_000});
    const person = await story.person(`Ada (${size.label})`);
    for (const step of plan.steps) {
      const began = Date.now();
      await story.step(`${step.title} (${size.label})`, async () => {
        if (step.command) run(step.command, {...sized, STORY_WORKSPACE: workspace});
        else {
          const module = await import(join(here, step.module));
          await module[step.export]({story, site, workspace, size, person});
        }
      }, {budgetKey: step.title});
      times.get(step.title).push(Date.now() - began);
    }
  }
} catch (thrown) {
  if (!(thrown instanceof StoryStopped)) error = thrown;
}
const smallest = plan.sizes[0], largest = plan.sizes.at(-1);
const sizeRatio = Math.round((largest.total / smallest.total) * 10) / 10;
const growth = plan.steps.map(step => {
  const ms = plan.sizes.map((_, at) => times.get(step.title)[at] ?? null);
  const ratio = ms[0] && ms.at(-1) !== null ? Math.round((ms.at(-1) / ms[0]) * 100) / 100 : null;
  return {title: step.title, ms, ratio, sizeRatio, allowed: plan.growth.allowed, ok: ratio !== null && ratio <= plan.growth.allowed};
});
for (const entry of growth) console.log(`GROWTH ${entry.ok ? 'ok' : 'OVER'} ${entry.title}: ${entry.ratio}x while the workspace grew ${sizeRatio}x (allowed ${entry.allowed}x)`);
const result = await story.finish({error, extra: {sizes: plan.sizes, growth}, extraOk: growth.every(entry => entry.ok)});
process.exit(result.completed && result.withinBudget ? 0 : 1);
