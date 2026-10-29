# Agent workflow: review the day's applications, draft replies, lay out the windows

Status: design, 2026-10-29. The proving workflow for the agent and action story.

## The workflow

The person opens the agent chat and says: "Go through today's job applications
in my mail. Review each critically (mail, CV, GitHub if it's meaningful,
portfolio site), draft a reply to each, don't send anything, and lay them out
for me."

For each of about 8 mails, the agent:

1. Finds the mail. Search covers mail fetched over IMAP, not only mail held in
   the room.
2. Reads the mail and lists its attachments.
3. Turns the CV attachment into workspace content and reads it through the PDF
   viewer: text per page, plus page images when the model has vision.
4. Reads the candidate's GitHub and portfolio. Without a window, through
   web-search `fetch` (JSON and HTML are text, and a PDF becomes content). With
   a window, through Browser.
5. Writes a critical review into the draft's notes and drafts a reply. It never
   sends.
6. Opens three windows:
   - (a) the original mail;
   - (b) one window holding the CV, the GitHub page and the portfolio as tabs;
   - (c) the draft.

   It then arranges them: one strip per candidate, named after the candidate,
   with the three windows side by side.

The result is 8 strips × 3 windows = 24 windows, 8 drafts in Mail's drafts
folder, nothing sent, and a summary in the chat with one line per candidate.

## What is there today, and what breaks

What works today:
- **Agent tool loop:** discover, invoke and page results across every app's
  actions, under the broker's permissions.
- **Sending is gated:** `send-message` is Outside and destructive, so the agent
  asks first.
- **PDF viewer:** `read-text`, `look-page`, and `import-pdf-content` from a
  content address.
- **Browser:** navigate, open tab, read page, capture page, reader view.
- **Web-search service:** `search` and `fetch`.
- **Model providers:** 16 presets, with both protocols (OpenAI-compatible
  chat/responses and Anthropic messages). Provider config is published as
  `model-providers.root` actions, keys go to the credential store, and models
  are listed with `GET /models`.

What breaks:

1. **Mail drafts are a single field** (`draft: DraftState`). Eight drafts are
   impossible, and a new agent draft overwrites the previous one.
2. **Agent mail verbs cover held mail only.** `draft-reply`, `search-messages`
   and `read` refuse mail fetched over IMAP. `read-message` only works while a
   Mail window has the message loaded. Two models of "a message" exist where
   there should be one.
3. **An incoming attachment can't become content.** Its `content_id` is left
   empty, and its only action is `save`, which opens a picker.
4. **Message places are labelled by id,** not by subject and sender.
5. **The agent can't open things or arrange windows by id:**
   - The window and layout actions (`tab.*`, `group.*`, `strip.*`) act on the
     focused window and aren't in the broker catalogue the agent uses.
   - `DesktopCommand` is id-addressed but is only served to room governors.
   - The workspace `open` takes a hash or an action, but not a resource or a
     URL, and doesn't say which window it opened.
6. **web-search `fetch` refuses PDFs** and other non-text bodies.
7. **The run budget is too small** for about 8 × 15 steps. The run has a hard
   cap of 30 model turns and a 300 s default, and no context management for a
   long run.
8. **There is no agent chat.** A task comes from the quick composer or a Tasks
   issue thread, and neither is a conversation the person comes back to.
9. **Providers:**
   - no Anthropic-compatible preset;
   - "Add provider" is a dropdown of 16, where it should be a searchable list
     of dozens;
   - hosted models can never get `vision`, and `tool_calling` is never read;
   - origin/main's `provider-settings-ui/src/bound.rs` isn't on integration
     yet.
10. **24 windows are unmeasured.** Each app connection is capped at 32 render
    channels, and no test opens this many.

## Design

### Mail: one message model, drafts as a collection, attachments as content

- **One message resource:**
  - `mail.message:<account>:<folder>:<uid>`, labelled "subject, sender, date".
  - It covers fetched and held mail alike. Held mail is just another source
    behind the same resource.
  - `read` returns the headers, text body, links and attachments. It reads from
    Mail's synced store, never from a loaded window.
  - `search` (sender, subject, body, date range, has-attachment) runs over that
    store, so it covers IMAP mail.
- **Drafts are a collection:**
  - `mail.draft:<id>` records in the account's drafts. Each is a resource that
    opens in its own window, with `in_reply_to`, `to`, `subject`, `body`,
    `attachments` and `notes`. Notes are private and never sent; the agent
    writes its review there.
  - `draft-reply(message)` creates a new draft every time.
  - The person's drafts and the agent's drafts live side by side; nothing
    overwrites anything.
  - Drafts sync to the server's Drafts folder over IMAP APPEND, so they show
    in other mail clients too.
- **Attachments as content:**
  - `mail.attachment:<msg>:<i>` gets `content`, which fetches the part, stores
    it in the workspace content store, and returns `tilo.content:<hash>` with
    its media type.
  - `open` opens that content in the app registered for its type.
  - `save` stays, for putting the file somewhere chosen.
- **Sending stays as it is:** Outside and destructive, so it always asks.
  Nothing in this workflow sends.

### Opening and arranging windows: the desktop's id-addressed commands as actions

The desktop model already names everything by id (`DesktopCommand`: `OpenTab`,
`OpenApp`, `MoveGroup`, `CreateStrip`, `RenameStrip`, `SetArrangement`,
`PlaceGroup`). Publish these to the local broker as the `so.tilo.desktop`
resource's actions, with the same rules as every other action: Undoable, in
History, and attributed to the agent on behalf of its person. They don't get a
second model.

- `open { target: resource | tilo.content:<hash> | url, place: new-window | tab-of <group> | beside <group>, strip?, background? }`
  - Opens the resource in the app that handles it.
  - Returns the window id and group id.
  - A URL opens in Browser; content opens in the app for its media type.
- `create-strip { name }` returns the strip id; `rename-strip`.
- `move-group { group, strip, index }`.
- `arrange { strip, widths? }`: strips lay windows out side by side, so
  this sets the columns' order and widths. There's no rows or grid layout,
  and asking for one is refused in words.
- `list-windows` returns windows with id, app, resource, title, group and
  strip, so the agent can find what it opened before.
- The palette's `tab.*`, `group.*` and `strip.*` actions become thin wrappers
  around these, applied to the focused ids. The palette and agents then share
  one path.

The agent's run groups its layout into one undo entry ("Arranged 8 candidates")
in the person's History.

### Web reading

- `fetch` returns text for text bodies (HTML is turned into readable text;
  JSON is pretty-printed).
- Any other body (PDF, images) is stored as content, and `fetch` returns
  `tilo.content:<hash>` and its media type. The agent can then use
  `import-pdf-content` or `open` it.
- GitHub needs no special code. For example,
  `https://api.github.com/users/<u>/repos?sort=updated` returns JSON, which is
  text. Rate limits and errors are reported in plain words.

### The agent run

- **Chat:**
  - An agent chat is a Tasks issue shown as a conversation: a window with the
    thread, a composer, the steps as they happen, confirmations inline, and a
    Stop button.
  - The quick composer's "Open as chat" moves the request into it.
  - Use the existing records: issue, thread and run. Don't add a new store.
- **Budget:**
  - Remove the hard 30-turn cap. A run's limit is its budget: turns, time and
    tokens. The issue's `execution_budget` sets it, and the chat picks a
    default sized for the task.
  - Long runs keep going by compacting: old tool results are replaced by their
    `read_result` page handles and a summary. The model can re-read any handle.
  - Progress is posted per item ("3 of 8: reviewed Jane Doe").
- **Vision:**
  - The vision and tool-calling flags of a hosted model come from the
    provider's listing where it gives them (OpenRouter's
    `architecture.input_modalities` and `supported_parameters`; Anthropic's
    and Gemini's model metadata).
  - Otherwise a shipped capability table covers known model families.
  - The person can override either flag per model.
  - The run refuses a model without tool calling, and names it.

### Providers

- **Add provider:** a searchable, scrollable list, not a dropdown.
  - It shows the name, a one-line description, the sign-in or key method, and
    a docs link.
  - Picking one opens its form: key, endpoint if editable, Test connection,
    and the model list with capability badges.
  - The person can add any number of profiles of the same preset (two
    OpenRouter keys, say).
- **Catalogue:**
  - Every widely used provider:
    - **OpenAI-compatible endpoints:**
      - Azure OpenAI, Cerebras, Perplexity, Cohere (compat), Moonshot/Kimi,
        Alibaba DashScope/Qwen, Zhipu/Z.ai, MiniMax, Nebius, SambaNova,
        Novita, DeepInfra, Hyperbolic, Venice;
      - gateways: Cloudflare Workers AI and AI Gateway, Vercel AI Gateway,
        Requesty, LiteLLM proxy;
      - local servers: vLLM, llama.cpp server, LocalAI, Jan.
    - **Anthropic-compatible endpoints:** Z.ai, Moonshot, MiniMax.
  - Plus a generic "OpenAI-compatible" and a generic "Anthropic-compatible"
    entry.
  - Each entry is data (`presets.rs`): a protocol, an endpoint, a key header,
    and a models path. It isn't code.
- **Model choice:**
  - Chosen per agent and per interaction, as today.
  - The model chooser shows context size and the vision and tools badges.
  - A model without tool calling can't be picked for an agent.
- **Managed by agents:**
  - The `model-providers.root` actions already exist. Add `list-presets`,
    `add-provider { preset }` and `set-model-capabilities`.
  - An agent can configure providers and models through the action system
    under the usual grants. Writing a key still needs `manage_credentials`,
    and the key is never readable back.
- **Bring origin/main's `bound.rs` work into integration first,** so nobody
  builds on the old provider UI.

## Proof

- **Integration test:**
  - A shared nested session, plus a fixture IMAP and SMTP server holding 8
    application mails, each with a PDF CV and links. Links go to a local
    fixture web server: a GitHub-like JSON API and a portfolio page.
  - A scripted model drives the real agent loop through the real actions.
  - It asserts:
    - 8 drafts exist and none was sent; the SMTP fixture saw nothing;
    - 24 windows are open, on 8 strips named after the candidates;
    - each candidate's resource window holds its CV and pages as tabs;
    - one undo puts the layout back;
    - frame time stays within budget with 24 windows open.
- **Live run:** the same workflow with a real model through a configured
  provider (OpenRouter or Anthropic), on the shared session. The review and
  draft quality is read by a person.
