# Collie Voice Commands — Frontend Design Brief

## Status

The current `web/` frontend is a bare debug form (text box + Send button + a
collapsible settings `<details>`). It proved the Rust core loop works
end-to-end but isn't a real interface. This document exists to drive a real
frontend design before building it.

## Feature list — conversational Collie interface

**Conversation core**
- Persistent, scrollable transcript: every turn shows what you said, what
  literally got sent to the pane (text or keys), and the spoken-back
  summary — not a single-shot request/response that vanishes.
- Multi-turn continuity — follow-ups ("now do X instead") work without
  re-explaining context each time.
- Audio auto-plays per turn, but every turn's summary is also
  readable/re-playable in the transcript.

**Multi-agent awareness** (Collie already exposes all of this via
`/api/snapshot` — v1 only used the `focused` pane)
- Dashboard of every running agent/pane/workspace/tab across all sessions,
  with live status (idle/working/blocked/done).
- Ability to switch which pane you're "talking to" mid-conversation, not
  just whatever's `focused` in the desktop UI.
- Visual + audio alert when a pane goes `blocked` (it's waiting on you)
  even if you're not actively looking at the app.

**Visibility beyond the summary**
- On-demand raw pane output view (expand a turn to see exactly what the
  agent printed, not just the TTS summary).
- Confirmation/menu prompts shown visually (not just silently resolved by
  the agentic layer) so you can see what got chosen and why.

**Session/notification plumbing** (Collie has the API, nothing in the app
uses it yet)
- Native surfacing of Collie's notifications + snooze controls.
- Connection/health indicator — Tailscale reachable? Collie bridge
  reachable? API key valid? — instead of a raw error string.

**Settings, carried over from v1**
- Model/voice pickers (already built) but integrated into this richer
  shell rather than a bare `<details>` block.

## Design brief prompt

Hand this to a design tool/agent as-is:

```
Design a mobile-first frontend for "Collie Voice Commands" — an Android app (Tauri 2.0,
WebView-rendered, plain HTML/CSS/JS, no framework required but one is fine) that lets someone
carry an ongoing conversation with a fleet of AI coding agents running on their home server,
instead of reading a terminal.

Context: the backend is "Collie," a bridge that fronts one or more "Herdr" sessions, each of
which manages multiple workspaces > tabs > panes, where a pane is either an AI agent (e.g.
Claude) or a bare shell. Each agent pane has a status: idle, working, blocked (needs input),
done, or unknown. The user types or dictates a command (via the OS keyboard's built-in
dictation — the app itself does no speech-to-text), the app sends it to a target pane, waits
for the agent to finish, and plays back a spoken summary of what happened via TTS.

Design for these things (not a single-shot command box):

1. A conversation view — a persistent, scrollable transcript. Each turn shows: what the user
   said, what was literally sent to the agent (typed text, or a raw key sequence like arrow
   keys + Enter for menu navigation), and the spoken-back summary. Audio auto-plays per turn
   but is replayable. Each turn can expand to show the agent's raw terminal output, not just
   the summary.

2. A dashboard/switcher across everything currently running — every workspace, tab, and pane,
   across possibly multiple Herdr sessions, each showing live status (idle/working/
   blocked/done/unknown) at a glance. The user picks which pane the conversation is currently
   "talking to" from here; it's not locked to whichever pane happens to be focused elsewhere.

3. Attention/alerting — a pane going "blocked" (waiting on the user) should be visually and
   audibly distinct, even if the user isn't currently looking at that pane's conversation.

4. Connection health — a persistent, unobtrusive indicator of whether the backend (reached
   over a private network) is reachable and whether the API key for the AI provider is
   configured, replacing raw error text with something legible.

5. Settings — the AI models used for (a) resolving loose spoken/typed input into the exact
   thing to send an agent, (b) summarizing what happened, and (c) text-to-speech + voice
   choice, are all user-editable, not hardcoded. Keep this reachable but out of the way of the
   main conversation flow.

Tone: this is a personal power-user tool, used one-handed, often glanced at rather than read
carefully — the person is doing something else (walking, driving passenger seat, cooking) and
mostly listening. Optimize for "what needs my attention right now" over information density.
Dark-mode-first is fine. Should feel closer to a chat/messaging app (e.g. a voice-memo-meets-
Slack hybrid) than a dashboard or admin panel.

Deliverable: screen-by-screen UI design (conversation view, pane switcher/dashboard, settings)
with enough visual/interaction detail — layout, states (idle/working/blocked), component
behavior — that a frontend engineer could implement it directly in HTML/CSS/JS.
```

## Next steps

1. Run the prompt above through a design tool/agent.
2. Bring the result back here — either replace `web/index.html`/`web/app.js`
   directly, or paste the design output back into this file under a new
   "Design output" section before implementing.

## Known issues / TODO

- **Blocked-overlay's Y/N buttons assume a binary decision, but real agent
  prompts are often multi-option** (a numbered menu — "1) do X 2) do Y
  3) cancel" — not a yes/no). Right now `web/app.js`'s blocked overlay
  (`#blocked-yes`/`#blocked-no`) always offers exactly two fixed quick-reply
  buttons regardless of what the pane is actually asking. The agentic
  reply-resolution layer (`commands.rs::resolve_reply`) already knows how to
  turn free-form text into the right keystrokes for a numbered menu when the
  *user* types something — the overlay's fixed Y/N shortcuts just don't use
  that path, they hardcode "yes"/"no" as the quick-reply text. Needs a look
  at: detecting from the pane's prompt text whether it's actually a binary
  question vs. a numbered/lettered menu, and rendering dynamic quick-reply
  buttons (one per option) instead of always showing Y/N. Reported after
  seeing this happen live with a Claude Code menu prompt.
