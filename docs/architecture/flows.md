# Flows Tilo must do well

Three people's use of Tilo, end to end. Each is a test in `t test` that drives
it through published actions and views. A change that breaks a step is a
regression.

## 1. A workspace per customer, on tilo.sh

A company keeps one workspace for each of its 10,000 customers on tilo.sh. Each
workspace mostly holds a few files. The company uses browser use there and now
and then runs a command.

- A workspace at rest costs close to nothing. A VM runs only while the browser
  or a program that isn't a Tilo app is in use, and stops after.
- The company creates and works with the workspaces from the TypeScript or
  Python SDK.
- Its own agent, running outside Tilo, reaches one workspace through an MCP
  endpoint scoped to it. The company creates that endpoint with one call.
- The docs can be read by an agent: the SDK and the MCP tools explain
  themselves.

## 2. One person, four devices, used as one computer

The person already has a Tilo identity. Their devices:

- a Fedora x86-64 machine, installed as a Tilo desktop with the Imager;
- an Orange Pi;
- a Lenovo laptop with a Snapdragon X Elite;
- tilo.sh as the fourth device, always on.

In this flow:

- All four use the person's identity and have all their workspaces, apps and
  files.
- They have four monitors: two on the Fedora machine, one on the Orange Pi and
  one on the laptop. They arrange the four and use the devices as one computer.
- The devices can sleep, lose power or be shut. The person's agents carry on
  on tilo.sh. They pay tilo.sh for that, say $20 a month for 1 TB, 8 vCPUs and
  24 GB of RAM. How billing works is out of scope for now.
- Their models come from:
  - the Anthropic API;
  - the Google Vertex API;
  - the Gemini API;
  - OpenAI, signed in with OAuth;
  - a custom OpenAI-compatible API.
- They work mostly through agents: signing in to sites, doing their work,
  coding, writing documents.

## 3. Laptop, phone and home server, no tilo.sh

The person runs Tilo OS on their laptop, the Tilo app on their Android phone and
Tilo server OS on a home server. The server can instead run Tilo only as a
container, for someone who doesn't want to give it a whole machine.

- They pay tilo.sh nothing, and it hosts nothing for them. They can still
  sign in there and control all their workspaces, which live on their own
  devices.
- The server serves its own Tilo web UI. That's on by default and can be
  turned off.
- The Android app connects like any other device.
- Their laptop runs a shell of their own, a 2D canvas.
- On the phone they use Tilo as a chat that writes views and apps. "Child
  mortality in Africa by country since 2000" gives a view of that data.
- A clock they set on the laptop, through their agent, rings on the phone.
- They create a workspace and invite someone who has never used Tilo. That
  person joins on the web: the 2D canvas as HTML and WASM, and single apps on
  their own.
- Their brother uses the Android app only to sync his files through Tilo.
- Everything lives on the server. They manage it from inside their workspace,
  and every device can manage every other device.
