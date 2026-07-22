# Collie Voice Commands — Project Scope

## One-liner
An Android app that lets me interact with my Herdr agent sessions by ear
instead of reading the Collie web UI. Commands are typed via the phone's
native keyboard (its built-in mic/dictation covers voice input — the app
doesn't do STT), sent to a running agent through the Collie bridge, and the
result comes back as OpenRouter-generated spoken summary — hands-free
listening, no glancing at the phone required for the response side.

## Why
Collie (`~/.config/herdr/plugins/github/herdr.collie-1edf0e1e987e`) already
gives me a mobile web UI over my Herdr agent herd (Tailscale-only, bridge on
`127.0.0.1:8787`, one bridge per host fronting every running herdr session).
It's still a screen-and-thumbs interface for both ends (typing *and* reading
results). I want the output side to be audio: send a command, then hear a
short spoken summary of what the agent did instead of reading scrollback.

## Actors / components

- **Collie bridge** (existing, unmodified) — HTTP API on the tailnet
  (`https://<magicdns-name>` via `tailscale serve`, or direct `:8787` if
  `COLLIE_SKIP_SERVE=1`). Relevant routes discovered in `bridge/server.ts`:
  - `GET /api/snapshot` — polls full state: agents, shell panes, workspaces,
    tabs, per-session bridge status.
  - `POST /api/tab`, `POST /api/workspace` — create structural things.
  - `GET /api/pane/:id` — read a pane's scrollback.
  - `POST /api/pane/:id/reply` — type text + submit into a pane (this is the
    "send a command" path).
  - `POST /api/pane/:id/keys` — send raw key sequences.
  - `POST /api/notifications/*`, `/api/subscribe` — snooze / web push.
  - Auth: same-origin + optional `COLLIE_TRUSTED_USER` (Tailscale identity
    header) + optional per-device allowlist via `COLLIE_DEVICE_HEADER`. The
    Android app will need to authenticate as an authorised device to be
    allowed to write (reply/keys), per `COLLIE_DEVICE_ALLOWLIST` in Collie's
    `.env`.
- **New Android app** (this project) — a text field (native keyboard, whose
  own mic/dictation button is the "voice input" — no app-level STT), sends
  the typed text as a `reply` to the right pane, polls `/api/snapshot` / pane
  output for the result, and plays back a spoken summary via OpenRouter TTS.
- **OpenRouter** — three uses, all confirmed available (see below):
  1. **Reply resolution (agentic)**: turns loose user input into the exact
     text/keys Collie should send to the pane — see "Agentic reply
     resolution" below.
  2. **Summarization**: a cheap/fast chat model condenses raw pane
     scrollback output into a short spoken-friendly summary ("what did the
     agent just do").
  3. **Text-to-speech**: OpenRouter has a dedicated `/api/v1/audio/speech`
     endpoint (separate from chat completions). Candidate models: Gemini 3.1
     Flash TTS (70+ languages, inline emotion tags), Grok Voice TTS 1.0,
     Voxtral Mini TTS (Mistral, $16/M chars), Kokoro 82M (lightweight,
     open-weight). Picks a voice + turns the summary into audio played back
     on the phone.
- **Settings screen** (this app) — every OpenRouter model used above (reply
  resolution, summarization, TTS + voice) is a user-editable setting, not
  hardcoded. Ships with defaults (see "Still open" — exact default slugs
  TBD at implementation time against OpenRouter's live catalog/pricing) but
  must be changeable without a rebuild.

## Agentic reply resolution

Input stays free-form (user types/dictates naturally), but what gets sent to
Collie's `reply` endpoint isn't always the raw text verbatim. Before sending,
the raw pane output (what the agent just displayed) + the user's utterance go
to an OpenRouter chat model with **structured/JSON output** (OpenRouter
supports `response_format: json_schema` the same way OpenAI's API does),
which decides the literal reply. This covers cases like:

- Agent shows a numbered menu ("1) do X  2) do Y  3) cancel"), user says
  "do the second one" → resolver emits `{"reply": "2"}`.
- Agent asks a yes/no confirmation, user says "yeah go ahead" → resolver
  emits `{"reply": "y"}` (or whatever the agent's expected token is, inferred
  from the prompt text).
- Agent is just waiting for a normal command, user says something that isn't
  answering a prompt → resolver passes the text through mostly unchanged.

Schema is small: something like `{"reply": string, "keys"?: string[]}` so it
can drive either `/api/pane/:id/reply` (typed text) or `/api/pane/:id/keys`
(raw key sequences, e.g. arrow-key menu navigation) depending on what the
agent's current prompt looks like. This is a second, separate OpenRouter call
from the post-hoc summarization call (different job: resolving *before*
sending vs. summarizing *after* the agent responds) — likely worth a cheap/
fast model here too, latency matters more than depth for this one.

No STT anywhere in this app — confirmed out of scope. Groq was considered
for STT/speech-to-speech but Groq doesn't offer true speech-to-speech either
(only separate Whisper STT + Orpheus English/Arabic-only TTS), and it's moot
now since the native keyboard's dictation covers voice input.

## Rough flow

1. User types (or uses the keyboard's mic/dictation button) a command into
   the app's text field.
2. App resolves which session/pane the command targets (default: primary
   session, `focused: true` pane, per the resolved questions below).
3. App reads the pane's current tail (what the agent last displayed) and
   sends it + the user's text to the OpenRouter reply-resolution model
   (structured JSON output) → gets back the literal reply/keys to send.
4. App `POST`s that resolved reply to `/api/pane/:id/reply` (or `keys` to
   `/api/pane/:id/keys`) on the Collie bridge.
5. App polls the pane (or `/api/snapshot`) until the agent's turn looks done
   (needs a "is it still working" signal — check `state-engine.ts` /
   `SnapshotResponse.agents` status fields).
6. Raw pane output goes to an OpenRouter chat model for summarization.
7. Summary text goes to OpenRouter's `/api/v1/audio/speech` endpoint → audio
   is played back on the phone.

## Explicitly out of scope (for now)

- Modifying Collie itself (bridge stays as-is; this is a separate client).
- Anything beyond a single tailnet / single Herdr host — no multi-host
  federation.
- Any app-level speech-to-text — the native keyboard's dictation is the
  voice-input mechanism; the app never touches raw audio in.
- Building a wakeword/always-listening pipeline — not applicable without STT.
- Handling `COLLIE_DEVICE_ALLOWLIST` provisioning UX beyond "paste a device
  id into Collie's `.env` manually" for v1.

## Resolved questions

- **Auth: use Variant A (`tailscale serve` + `COLLIE_TRUSTED_USER`), already
  live on this host.** Confirmed by checking the actual running deployment:
  `tailscale serve status` shows `https://thinkpad.sparidae-chinstrap.ts.net`
  proxying to `127.0.0.1:8787`, and `.env` has
  `COLLIE_TRUSTED_USER=rafal.kuka94@gmail.com` set, no
  `COLLIE_DEVICE_HEADER`. Per the README, `tailscale serve` injects
  `Tailscale-User-Login` at the network layer for *any* tailnet client (not
  browser-specific — that was an earlier wrong assumption in this doc,
  corrected now). The phone (`pixel-9-pro`) is already a tailnet member. So:
  no reverse proxy, no device-header setup, no embedded Tailscale SDK — the
  app just needs to be a normal Tailscale-connected Android client hitting
  `https://thinkpad.sparidae-chinstrap.ts.net`.
- **Target pane: default to whichever pane has `focused: true` in
  `/api/snapshot`.** `AgentView` / `TabView` / `WorkspaceView` in
  `bridge/types.ts` already expose a read-only `focused` flag — Collie's own
  "currently active in the desktop TUI" concept. No new logic needed, just
  read it.
- **Default session: omit `?session=` to target the primary session.**
  Confirmed in `bridge/sessions.ts`: `registry.get(undefined)` returns the
  primary session runtime.

## Decided, with defaults to revisit at implementation time

- **Models are user-configurable, not hardcoded.** All three OpenRouter uses
  (reply resolution, summarization, TTS+voice) are settings, editable in-app
  without a rebuild. Ships with defaults; the exact default model slugs are
  a small implementation-time task (pick from OpenRouter's live catalog —
  slugs/pricing shift, so don't lock these into the scope doc prematurely).
  Rough starting point: a cheap/fast chat model (e.g. Gemini Flash-tier or
  Llama 8B-class) for both reply resolution and summarization, and Kokoro
  82M or Gemini 3.1 Flash TTS as the starting TTS voice — finalize when
  actually wiring the OpenRouter client.
- **Command grammar: free-form input, agentic reply resolution before
  send.** See "Agentic reply resolution" above — not plain pass-through,
  Collie's own `reply` endpoint takes literal text but this app adds a
  resolution step in front of it so loose input ("do the second one",
  "yeah go ahead") still produces the correct literal reply.
- **Framework: Tauri 2.0 (mobile target).** Final — see "Rust on Android"
  below for reasoning (WebView UI + Rust backend, most mature Rust→signed-APK
  pipeline, trivial audio playback for the TTS response via the WebView).

## Repo

- `/var/home/rafa/dev/collie_voice_commands` — new, independent git repo
  (not a submodule of anything). Android app, written in **Rust** (not
  Kotlin) — see "Rust on Android" below for framework choice. Talks to
  Collie's HTTP API over the tailnet the same way `kv_apk` talks to
  `kv_manager`'s API, but is not modeled on `kv_apk`'s Kotlin/Compose
  structure since the language differs.

## Rust on Android — framework choice

Dropping STT removes most of the reason to bridge into Android SDK-level
voice APIs at all: the only Android-specific surface left is a text input
field and audio playback, both of which are thin/well-supported in any
Rust mobile framework (no `SpeechRecognizer`/`TextToSpeech`/foreground-service
JNI bridging needed). This tips the choice toward "pure Rust" options:

- **Tauri 2.0 (mobile target)** — Rust backend + WebView-rendered UI (a
  minimal local HTML/JS front end, no separate web server). Most mature
  "ship a real signed APK from a Rust project" pipeline right now. Audio
  playback for the TTS response can go through a small JS `<audio>` element
  in the WebView, or a Tauri plugin — either is simple compared to the
  STT case. Current leaning choice.
- **Dioxus (mobile)** — React-like, all-Rust UI code (no separate JS/HTML).
  Audio playback needs a small platform call (JNI or an existing plugin);
  less mature Android story than Tauri but worth a look given the surface
  is now this small.
- **Slint** — lightweight declarative UI, official Android support, smaller
  ecosystem overall but plausible given how little native bridging is
  actually needed now.
- **UniFFI hybrid** (thin Kotlin shell + Rust core) — no longer clearly
  worth the extra language/build complexity now that there's no
  Android-SDK-heavy voice API work driving the split.

**Decision: Tauri 2.0.**
