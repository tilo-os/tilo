/**
 * Types for the apps this workspace has, written by `tilo sdk gen`.
 *
 * `tilo sdk gen --lang ts -w <workspace>` writes them from the apps installed
 * there; without `-w`, every app this build ships, native and tilolang. Each app has a
 * module of its own, exported here as a namespace: a type is the SDK's `Resource`
 * of one resource kind, made by its factory with the resource's path.
 *
 * ```ts
 * import { pdfViewer } from "./tilo-types/index.ts";
 *
 * const form: pdfViewer.Document = pdfViewer.pdfDocument(ws, "pdf-viewer/app.pdf");
 * await form.load();
 * ```
 */

export * as discord from "./discord.ts";
export * as messages from "./messages.ts";
export * as osk from "./osk.ts";
export * as poker from "./poker.ts";
export * as remote from "./remote.ts";
export * as slides from "./slides.ts";
export * as terminal from "./terminal.ts";
export * as totp from "./totp.ts";
export * as activity from "./activity.ts";
export * as apiTester from "./api_tester.ts";
export * as backups from "./backups.ts";
export * as board from "./board.ts";
export * as browser from "./browser.ts";
export * as calculator from "./calculator.ts";
export * as calendar from "./calendar.ts";
export * as camera from "./camera.ts";
export * as canvas from "./canvas.ts";
export * as chat from "./chat.ts";
export * as chipDesign from "./chip_design.ts";
export * as clocks from "./clocks.ts";
export * as codeEditor from "./code_editor.ts";
export * as console from "./console.ts";
export * as contacts from "./contacts.ts";
export * as containers from "./containers.ts";
export * as counter from "./counter.ts";
export * as dbClient from "./db_client.ts";
export * as diffmerge from "./diffmerge.ts";
export * as preview from "./preview.ts";
export * as discussion from "./discussion.ts";
export * as diskUsage from "./disk_usage.ts";
export * as disks from "./disks.ts";
export * as ebook from "./ebook.ts";
export * as feeds from "./feeds.ts";
export * as fileExplorer from "./file_explorer.ts";
export * as firewall from "./firewall.ts";
export * as gallery from "./gallery.ts";
export * as gitClient from "./git_client.ts";
export * as hexEditor from "./hex_editor.ts";
export * as logs from "./logs.ts";
export * as mail from "./mail.ts";
export * as maps from "./maps.ts";
export * as migrate from "./migrate.ts";
export * as monitor from "./monitor.ts";
export * as music from "./music.ts";
export * as notes from "./notes.ts";
export * as passwords from "./passwords.ts";
export * as pdfViewer from "./pdf_viewer.ts";
export * as photos from "./photos.ts";
export * as playback from "./playback.ts";
export * as printCentre from "./print_centre.ts";
export * as publisher from "./publisher.ts";
export * as scratchpad from "./scratchpad.ts";
export * as settings from "./settings.ts";
export * as sheets from "./sheets.ts";
export * as shell from "./shell.ts";
export * as shortcuts from "./shortcuts.ts";
export * as snippets from "./snippets.ts";
export * as speedTest from "./speed_test.ts";
export * as store from "./store.ts";
export * as tasks from "./tasks.ts";
export * as today from "./today.ts";
export * as tube from "./tube.ts";
export * as typstEditor from "./typst_editor.ts";
export * as video from "./video.ts";
export * as videoEditor from "./video_editor.ts";
export * as views from "./views.ts";
export * as weather from "./weather.ts";
export * as windows from "./windows.ts";
export * as wordProcessor from "./word_processor.ts";
export * as sections from "./sections.ts";
