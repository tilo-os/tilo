import test from 'node:test';
import assert from 'node:assert/strict';

import {MAX_DROPPED_FILE, MAX_DROPPED_FILES} from './host-core.mjs';
import {chooseFiles, dropRoute, droppedEntries, walkDropped} from './upload-files.mjs';

/** A file as a drop hands it over. */
const file = (name, size = 1) => ({name, size});

/** A dropped file's entry, as a browser's file system entries answer. */
const fileEntry = (name, size = 1) => ({name, isFile: true, isDirectory: false, file: done => done(file(name, size))});

/** A dropped folder's entry; its reader hands its entries over two at a time, then an empty
 * batch, as a browser's does for a large folder. */
const folderEntry = (name, children) => ({
  name, isFile: false, isDirectory: true,
  createReader() {
    let at = 0;
    return {readEntries(done) {
      const batch = children.slice(at, at + 2);
      at += batch.length;
      queueMicrotask(() => done(batch));
    }};
  },
});

/** A drop's `DataTransfer` holding `taken`: entries, and a file a browser gives no entry for. */
const dropOf = taken => ({
  items: taken.map(thing => ({kind: 'file', webkitGetAsEntry: () => thing.entry ?? null, getAsFile: () => thing.file ?? null})),
  files: taken.map(thing => thing.file).filter(Boolean),
});

test('a dropped folder is walked whole, each file named from the folder\'s own place', async () => {
  const docs = folderEntry('docs', [
    fileEntry('readme.md'),
    folderEntry('2026', [fileEntry('plan.md'), fileEntry('budget.xlsx'), folderEntry('empty', [])]),
    fileEntry('scan.pdf'),
  ]);
  const loose = {file: file('photo.png')};
  const {files, folder} = await walkDropped(droppedEntries(dropOf([{entry: docs}, {entry: fileEntry('notes.txt')}, loose])));
  assert.equal(folder, true);
  assert.deepEqual(files.map(({path}) => path), [
    'docs/readme.md', 'docs/2026/plan.md', 'docs/2026/budget.xlsx', 'docs/scan.pdf', 'notes.txt', 'photo.png',
  ], 'several files and folders dropped at once all go, in the order dropped');
  assert.equal(files[0].file.name, 'readme.md', 'each carries its own file');
});

test('a browser that offers no entries hands its files over as they are', async () => {
  const {files, folder} = await walkDropped(droppedEntries({files: [file('a.txt'), file('b.txt')]}));
  assert.equal(folder, false);
  assert.deepEqual(files.map(({path}) => path), ['a.txt', 'b.txt']);
});

test('a few plain files on a window go to it; anything else goes into the workspace', () => {
  const few = [{file: file('picture.png')}, {file: file('caption.txt')}];
  assert.equal(dropRoute({files: few, folder: false, onWindow: true}), 'window', 'a picture into a note');
  assert.equal(dropRoute({files: few, folder: false, onWindow: false}), 'upload', 'the desk itself takes uploads');
  assert.equal(dropRoute({files: few, folder: true, onWindow: true}), 'upload', 'a folder goes up, wherever it lands');
  assert.equal(dropRoute({files: [{file: file('film.mov', MAX_DROPPED_FILE + 1)}], folder: false, onWindow: true}), 'upload',
    'a file too large for a window goes up');
  assert.equal(dropRoute({files: [{file: file('film.mov', MAX_DROPPED_FILE)}], folder: false, onWindow: true}), 'window');
  const many = Array.from({length: MAX_DROPPED_FILES + 1}, (_, at) => ({file: file(`f${at}.txt`)}));
  assert.equal(dropRoute({files: many, folder: false, onWindow: true}), 'upload', 'more than a drop hands a window go up');
  assert.equal(dropRoute({files: many.slice(1), folder: false, onWindow: true}), 'window');
});

test('the chooser asks for files, or a folder whose files keep their paths', async () => {
  const appended = [];
  const document = {
    body: {append: node => appended.push(node)},
    createElement: () => {
      const listeners = {};
      return {
        style: {}, dataset: {}, files: [],
        addEventListener: (type, listener) => { listeners[type] = listener; },
        remove() { this.removed = true; },
        click() {
          this.files = this.webkitdirectory
            ? [{...file('plan.md'), webkitRelativePath: 'docs/2026/plan.md'}]
            : [file('scan.pdf'), file('photo.png')];
          queueMicrotask(() => listeners.change());
        },
      };
    },
  };
  assert.deepEqual((await chooseFiles({document})).map(({path}) => path), ['scan.pdf', 'photo.png']);
  assert.equal(appended[0].multiple, true);
  const chosen = await chooseFiles({folder: true, document});
  assert.equal(appended[1].webkitdirectory, true);
  assert.deepEqual(chosen.map(({path}) => path), ['docs/2026/plan.md']);
  assert.ok(appended.every(input => input.removed), 'the chooser leaves nothing behind');
});

test('a chooser closed with nothing chosen uploads nothing', async () => {
  const document = {
    body: {append() {}},
    createElement: () => {
      const listeners = {};
      return {style: {}, dataset: {}, addEventListener: (type, listener) => { listeners[type] = listener; }, remove() {},
        click: () => queueMicrotask(() => listeners.cancel())};
    },
  };
  assert.deepEqual(await chooseFiles({document}), []);
});
