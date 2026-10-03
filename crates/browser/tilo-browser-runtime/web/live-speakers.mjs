/**
 * One presence per live layer, however many mounts share it (`peer.mjs`'s
 * `createLiveLayer`): the mount the person (or agent) acted in last
 * publishes, and idle or hidden mounts never overwrite it. Input in a mount
 * claims its layer at once; an action through a mount claims it only if it
 * moved that mount's focus or caret (an agent posting in Chat after editing
 * the notes stays in the notes). A layer nobody claimed yet belongs to the
 * first mount that publishes through it.
 */

/** Publications after an action that may still carry its caret change (the
 * caret can settle a presented frame later). */
const ACTION_PUBLICATIONS = 3;

export function createSpeakers() {
  const speaker = new Map();
  return {
    mount() {
      const token = {};
      const views = new Map();
      let own = '[null,null]';
      let focus = null;
      let selection = null;
      const said = new Set();
      let actionPublications = 0;
      const speaks = layer => {
        if ((speaker.get(layer) ?? token) !== token) return false;
        speaker.set(layer, token);
        return true;
      };
      return {
        /** Input in this mount: it speaks for `layer` now, and says again
         * where its focus and caret are (another mount spoke meanwhile). */
        claim(layer) {
          const previous = speaker.get(layer);
          speaker.set(layer, token);
          if (previous === undefined || previous === token || !said.size) return;
          const again = {};
          if (said.has('focus')) again.focus = focus;
          if (said.has('selection')) again.selection = selection;
          layer.publishPresence(again);
        },
        /** An action ran through this mount. */
        acted() { actionPublications = ACTION_PUBLICATIONS; },
        release() { for (const [layer, holder] of speaker) if (holder === token) speaker.delete(layer); },
        /** `layer` as this mount publishes through it; everything else is the layer's. */
        view(layer) {
          let found = views.get(layer);
          if (!found) {
            found = Object.create(layer, {
              publishPresence: {value: partial => {
                if ('focus' in partial) { focus = partial.focus; said.add('focus'); }
                if ('selection' in partial) { selection = partial.selection; said.add('selection'); }
                const next = JSON.stringify([focus, selection]);
                if (actionPublications > 0) {
                  actionPublications--;
                  if (next !== own) speaker.set(layer, token);
                }
                own = next;
                return speaks(layer) && layer.publishPresence(partial);
              }},
              publishPreview: {value: (record, ops) => speaks(layer) && layer.publishPreview(record, ops)},
            });
            views.set(layer, found);
          }
          return found;
        },
      };
    },
  };
}
