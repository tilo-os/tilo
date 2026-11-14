/**
 * Files and folders a person puts into the workspace from the page.
 *
 * A drop is read for what it holds: every file of every folder in it, each
 * with its path from the dropped folder's own place (`docs/2026/plan.md`),
 * as the command line names them. The desktop decides what is under the
 * pointer (`drop_on_window`); a small drop of plain files onto a window
 * goes to that window as before, and everything else goes up into the
 * workspace (`desktop_upload`): a drop on the desk itself, a folder, a file
 * too large for a window to take, or more files than one drop hands a
 * window. The desk menu's "Upload files…" and "Upload a folder…" choose
 * them with the browser's own chooser.
 */
import {MAX_DROPPED_FILE, MAX_DROPPED_FILES} from './host-core.mjs';

/** The desk menu's rows (offered by the desktop where the person may change
 * the workspace's records). */
export const UPLOAD_FILES = 'upload-files';
export const UPLOAD_FOLDER = 'upload-folder';
export const UPLOAD_ACTIONS = Object.freeze([UPLOAD_FILES, UPLOAD_FOLDER]);

/** What a drop's `DataTransfer` holds, taken while the drop event lasts
 * (its items are gone after it): each dropped thing's entry where the
 * browser gives one, else its file. */
export function droppedEntries(data) {
  const items = [...(data?.items ?? [])].filter(item => item.kind === 'file');
  if (!items.length) return [...(data?.files ?? [])].map(file => ({file}));
  return items.map(item => {
    const entry = item.webkitGetAsEntry?.() ?? null;
    return entry ? {entry} : {file: item.getAsFile()};
  }).filter(taken => taken.entry || taken.file);
}

/** Every entry of the folder `directory`: a reader hands them over in
 * batches until one comes back empty. */
async function folderEntries(directory) {
  const reader = directory.createReader();
  const all = [];
  for (;;) {
    const batch = await new Promise((resolve, reject) => reader.readEntries(resolve, reject));
    if (!batch.length) return all;
    all.push(...batch);
  }
}

const fileOf = entry => new Promise((resolve, reject) => entry.file(resolve, reject));

/** The files `taken` (from {@link droppedEntries}) hold, folders walked
 * through: `{files: [{file, path}], folder}`, `folder` saying whether a
 * folder was among them. A path starts at the dropped thing itself:
 * `docs/2026/plan.md` for a folder `docs`, `plan.md` for a file. */
export async function walkDropped(taken) {
  const files = [];
  let folder = false;
  const walk = async (entry, path) => {
    if (entry.isDirectory) {
      folder = true;
      for (const child of await folderEntries(entry)) await walk(child, `${path}/${child.name}`);
    } else if (entry.isFile) {
      files.push({file: await fileOf(entry), path});
    }
  };
  for (const {entry, file} of taken) {
    if (entry) await walk(entry, entry.name);
    else if (file) files.push({file, path: file.name});
  }
  return {files, folder};
}

/** Where a drop goes: `'window'`, the window under the pointer taking the
 * files as it always has, or `'upload'`, into the workspace. `onWindow` is
 * the desktop's word on whether a window is under the pointer. */
export function dropRoute({files, folder, onWindow}) {
  if (folder || !onWindow || files.length > MAX_DROPPED_FILES) return 'upload';
  return files.some(({file}) => file.size > MAX_DROPPED_FILE) ? 'upload' : 'window';
}

/** Ask the person for files with the browser's chooser (a folder and
 * everything in it, with `folder`): resolves to `[{file, path}]`, empty
 * when they chose nothing. */
export function chooseFiles({folder = false, document = globalThis.document} = {}) {
  return new Promise(resolve => {
    const input = document.createElement('input');
    input.type = 'file';
    input.multiple = true;
    if (folder) input.webkitdirectory = true;
    input.style.display = 'none';
    input.dataset.upload = folder ? 'folder' : 'files';
    const done = files => {
      input.remove();
      resolve(files);
    };
    input.addEventListener('change', () => done([...(input.files ?? [])]
      .map(file => ({file, path: file.webkitRelativePath || file.name}))), {once: true});
    input.addEventListener('cancel', () => done([]), {once: true});
    document.body.append(input);
    input.click();
  });
}
