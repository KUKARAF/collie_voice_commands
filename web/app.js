const invoke = window.__TAURI__.core.invoke;
const tauriListen = window.__TAURI__.event.listen;

const SUPERVISOR_ID = "supervisor";
const TRANSCRIPTS_KEY = "collie_transcripts";
const CURRENT_PANE_KEY = "collie_current_pane";
const MAX_TRANSCRIPT = 200;

const ACTIVE_VIEW_KEY = "collie_active_view";

const state = {
  settings: null,
  snapshot: null,
  bridgeReachable: null,
  prevStatus: {}, // paneId -> status, to edge-detect transitions into "blocked"
  // A real Collie pane id, or the literal SUPERVISOR_ID. New installs start on Supervisor
  // since it can route anywhere, rather than erroring with "no pane selected."
  currentPaneId: localStorage.getItem(CURRENT_PANE_KEY) || SUPERVISOR_ID,
  transcripts: loadTranscripts(), // { [paneId | "supervisor"]: Turn[] }
  blockedPane: null, // { paneId, name, prompt } currently shown in the overlay
  dismissedBlocked: new Set(), // paneIds snoozed/dismissed until they clear "blocked"
  // "chat" | "tasks" — which of the two swipeable view-panes is active. Settings is a separate
  // full-screen overlay, not a third pane, so it isn't tracked here.
  activeView: localStorage.getItem(ACTIVE_VIEW_KEY) || "chat",
  tasks: [], // flat list from list_todos, rendered as a parent/child tree client-side
  tasksBusy: false, // suppresses auto-refresh while a mutating tasks action is in flight
  tasksError: null,
};

// ---------- storage ----------

function loadTranscripts() {
  try {
    const raw = localStorage.getItem(TRANSCRIPTS_KEY);
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

function saveTranscripts() {
  const capped = {};
  for (const [paneId, turns] of Object.entries(state.transcripts)) {
    capped[paneId] = turns.slice(-MAX_TRANSCRIPT);
  }
  state.transcripts = capped;
  localStorage.setItem(TRANSCRIPTS_KEY, JSON.stringify(capped));
}

function pushTurn(paneId, turn) {
  if (!state.transcripts[paneId]) state.transcripts[paneId] = [];
  state.transcripts[paneId].push(turn);
}

/// Turn ids are globally unique, but which pane's bucket they live in isn't known by the
/// caller (e.g. a link from the conversation view) — search all buckets.
function findTurnAnywhere(id) {
  for (const paneId of Object.keys(state.transcripts)) {
    const list = state.transcripts[paneId];
    const index = list.findIndex((t) => t.id === id);
    if (index !== -1) return { turn: list[index], paneId, index };
  }
  return { turn: null, paneId: null, index: -1 };
}

// ---------- small helpers ----------

function escapeHtml(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[c]);
}

function stripAnsi(s) {
  // eslint-disable-next-line no-control-regex
  return String(s ?? "").replace(/\x1b\[[0-9;]*m/g, "");
}

function timeAgo(ts) {
  const s = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  return `${h}h ago`;
}

const STATUS_META = {
  idle: { chip: "chip--dim", dot: "status-dot--idle", label: "IDLE" },
  working: { chip: "chip--accent", dot: "status-dot--working", label: "WORKING" },
  blocked: { chip: "chip--orange", dot: "status-dot--blocked", label: "BLOCKED" },
  done: { chip: "chip--accent", dot: "status-dot--done", label: "DONE" },
  unknown: { chip: "", dot: "status-dot--unknown", label: "UNKNOWN" },
};

function statusMeta(status) {
  return STATUS_META[status] || STATUS_META.unknown;
}

function allPanes(snapshot) {
  if (!snapshot) return [];
  return [...(snapshot.agents || []), ...(snapshot.shellPanes || [])];
}

function findPane(snapshot, paneId) {
  return allPanes(snapshot).find((p) => p.paneId === paneId) || null;
}

function paneDisplayName(pane) {
  if (!pane) return "no pane selected";
  const label = pane.paneLabel || pane.sessionName || pane.agent;
  return `${pane.agent} · ${label}`;
}

function renderWaveform(container, count, playedFraction) {
  container.innerHTML = "";
  for (let i = 0; i < count; i++) {
    const bar = document.createElement("span");
    bar.className = "waveform__bar";
    const h = 5 + Math.round(19 * Math.abs(Math.sin(i * 12.9898 + 4.1)));
    bar.style.height = `${h}px`;
    if (playedFraction != null && i / count > playedFraction) {
      bar.classList.add(bar.parentElement === null ? "" : "");
      bar.classList.add("waveform__bar--faint");
    }
    container.appendChild(bar);
  }
}

// ---------- audio playback ----------

const MEDIA_ERROR_LABELS = {
  1: "aborted",
  2: "network error",
  3: "decode error",
  4: "format not supported",
};

function showPlaybackError(message) {
  const el = document.getElementById("now-playing-error");
  if (!el) return;
  if (!message) {
    el.style.display = "none";
    el.textContent = "";
    return;
  }
  el.textContent = message;
  el.style.display = "block";
}

// One persistent <audio> element, reused for every play. Creating a fresh `new Audio()` per
// call (the previous approach) meant every playback was a brand-new, never-gestured element —
// Chromium's autoplay allowance is tied to the page having received a user gesture at all, but
// a never-before-played element can still be treated more strictly, and re-registering
// listeners per call was also wasteful. One element, created once, is both more correct and
// simpler.
let ttsAudioEl = null;

function getTtsAudioElement() {
  if (ttsAudioEl) return ttsAudioEl;
  ttsAudioEl = new Audio();
  const bar = document.getElementById("now-playing");
  const wf = document.getElementById("now-playing-waveform");
  const timeEl = document.getElementById("now-playing-time");
  const toggle = document.getElementById("now-playing-toggle");
  ttsAudioEl.addEventListener("timeupdate", () => {
    const frac = ttsAudioEl.duration ? ttsAudioEl.currentTime / ttsAudioEl.duration : 0;
    renderWaveform(wf, 24, frac);
    const remaining = Math.max(0, (ttsAudioEl.duration || 0) - ttsAudioEl.currentTime);
    const mm = Math.floor(remaining / 60);
    const ss = Math.floor(remaining % 60).toString().padStart(2, "0");
    timeEl.textContent = `${mm}:${ss}`;
  });
  ttsAudioEl.addEventListener("ended", () => {
    bar.style.display = "none";
  });
  ttsAudioEl.addEventListener("error", () => {
    const code = ttsAudioEl.error ? ttsAudioEl.error.code : null;
    console.error("tts playback error", ttsAudioEl.error);
    showPlaybackError(`playback failed: ${MEDIA_ERROR_LABELS[code] || "unknown error"}`);
    toggle.textContent = "▶";
  });
  toggle.addEventListener("click", () => {
    if (ttsAudioEl.paused) {
      ttsAudioEl
        .play()
        .then(() => showPlaybackError(null))
        .catch((err) => {
          console.error("tts playback failed", err);
          showPlaybackError(`play blocked: ${err.message || err}`);
        });
      toggle.textContent = "❚❚";
    } else {
      ttsAudioEl.pause();
      toggle.textContent = "▶";
    }
  });
  return ttsAudioEl;
}

// Call from inside a genuine click handler (e.g. TALK), before any async work — Chromium grants
// autoplay permission for the page from a real user gesture like this, and reusing the same
// element afterward (see above) is what makes that permission actually carry over to the
// programmatic play() calls that happen later once a network response comes back.
function primeAudioPlayback() {
  const audio = getTtsAudioElement();
  audio.play().catch(() => {});
  audio.pause();
}

// collie-server now serves TTS audio as a real fetchable URL (`/api/audio/:id`) instead of
// base64 JSON — just point the <audio> element straight at it, resolved against the configured
// collie-server base URL.
function playAudio(audioUrl, text) {
  const audio = getTtsAudioElement();
  audio.pause();
  const base = ((state.settings && state.settings.collieBaseUrl) || "").replace(/\/$/, "");
  audio.src = base + audioUrl;
  const bar = document.getElementById("now-playing");
  const wf = document.getElementById("now-playing-waveform");
  const toggle = document.getElementById("now-playing-toggle");
  document.getElementById("now-playing-text").textContent = text || "";
  showPlaybackError(null);
  bar.style.display = "block";
  renderWaveform(wf, 24, 0);
  toggle.textContent = "❚❚";
  audio.play().catch((err) => {
    // Playback blocked or failed — the now-playing bar's toggle still lets the user start it
    // manually (a direct tap is its own valid user gesture).
    console.error("tts autoplay failed", err);
    showPlaybackError(`autoplay blocked: ${err.message || err} — tap ▶ to play`);
    toggle.textContent = "▶";
  });
}

function stopAudio() {
  if (ttsAudioEl) ttsAudioEl.pause();
  const bar = document.getElementById("now-playing");
  if (bar) bar.style.display = "none";
}

// ---------- router ----------

function parseHash() {
  const h = location.hash.replace(/^#\/?/, "");
  const [view, param] = h.split("/");
  return { view: view || "conversation", param };
}

function setHash(path) {
  location.hash = path;
}

window.addEventListener("hashchange", () => {
  renderApp();
  reportForegroundContext();
});

// ---------- top chrome (persistent brand/tabs/cog + per-pane chat header) ----------

function renderChrome(view) {
  const netChip = document.getElementById("net-chip");
  netChip.className = "chip " + (state.bridgeReachable ? "chip--accent chip--pulse" : "chip--orange");

  // Visible from every screen — chrome-bar is persistent above both swipe panes now, so this
  // was already true, but is finally reachable from Tasks too, not just Chat.
  const attentionChip = document.getElementById("attention-chip");
  const blockedCount = allPanes(state.snapshot).filter((p) => p.status === "blocked").length;
  if (blockedCount > 0) {
    attentionChip.style.display = "inline-flex";
    attentionChip.innerHTML = `<span class="chip__dot"></span>${blockedCount} NEED${blockedCount === 1 ? "S" : ""} ATTENTION`;
  } else {
    attentionChip.style.display = "none";
  }

  document.getElementById("tab-chat").classList.toggle("chrome-tab--active", state.activeView === "chat");
  document.getElementById("tab-tasks").classList.toggle("chrome-tab--active", state.activeView === "tasks");

  // Chat-pane-local header: talking-to-bar for the conversation subview, back-header (with a
  // FLEET/TURN title) for fleet/turn — same behavior as before the swipe restructure, just
  // scoped inside #view-chat instead of driving the whole page.
  const talkingToBar = document.getElementById("talking-to-bar");
  const backHeader = document.getElementById("back-header");
  const dock = document.getElementById("input-dock");

  if (view === "conversation") {
    talkingToBar.style.display = "flex";
    backHeader.style.display = "none";
    dock.style.display = "flex";
    updateTalkingToBar();
  } else {
    talkingToBar.style.display = "none";
    backHeader.style.display = "flex";
    dock.style.display = "none";
    const title = document.getElementById("back-header-title");
    const chip = document.getElementById("back-header-chip");
    chip.style.display = "none";
    if (view === "fleet") {
      title.textContent = "FLEET";
      if (blockedCount > 0) {
        chip.style.display = "inline-flex";
        chip.className = "chip chip--orange chip--pulse";
        chip.innerHTML = `<span class="chip__dot"></span>${blockedCount} BLOCKED`;
      }
    } else if (view === "turn") {
      title.textContent = "TURN";
    }
  }
}

function setActiveView(view) {
  state.activeView = view;
  localStorage.setItem(ACTIVE_VIEW_KEY, view);
  document.getElementById("view-track").style.transform = view === "tasks" ? "translateX(-50%)" : "translateX(0)";
  document.getElementById("tab-chat").classList.toggle("chrome-tab--active", view === "chat");
  document.getElementById("tab-tasks").classList.toggle("chrome-tab--active", view === "tasks");
  if (view === "tasks") loadTasks();
}

function updateTalkingToBar() {
  const dot = document.getElementById("talking-to-dot");
  const chip = document.getElementById("talking-to-status");
  if (state.currentPaneId === SUPERVISOR_ID) {
    document.getElementById("talking-to-name").textContent = "SUPERVISOR · fleet-wide";
    dot.className = "status-dot status-dot--done";
    chip.className = "chip chip--accent";
    chip.textContent = "AUTO";
    return;
  }
  const pane = findPane(state.snapshot, state.currentPaneId);
  document.getElementById("talking-to-name").textContent = paneDisplayName(pane);
  const meta = statusMeta(pane ? pane.status : "unknown");
  dot.className = "status-dot " + meta.dot;
  chip.className = "chip " + meta.chip;
  chip.textContent = pane ? meta.label : "—";
}

// ---------- conversation screen ----------

// Single-pane turns carry one sentMode/sentContent; supervisor turns carry `dispatches`, one
// per pane it touched (or none at all, when it just answered from fleet status).
function sentSectionHtml(turn) {
  if (turn.isSupervisor) {
    if (!turn.dispatches || turn.dispatches.length === 0) return "";
    return turn.dispatches
      .map((d) => {
        const label = d.sentMode === "keys" ? "SENT · KEYS" : "SENT · TYPED";
        const cls = d.sentMode === "keys" ? "sent-box sent-box--keys" : "sent-box";
        return `<div>
          <div class="card__section-label">${escapeHtml(d.paneName || d.paneId)} — ${label}</div>
          <div class="${cls}">${escapeHtml(d.sentContent)}</div>
        </div>`;
      })
      .join("");
  }
  const sentLabel = turn.sentMode === "keys" ? "SENT · KEYS" : "SENT · TYPED";
  const sentBoxClass = turn.sentMode === "keys" ? "sent-box sent-box--keys" : "sent-box";
  return `<div>
    <div class="card__section-label">${sentLabel}</div>
    <div class="${sentBoxClass}">${escapeHtml(turn.sentContent || turn.inputText)}</div>
  </div>`;
}

const CATEGORY_META = {
  success: { chip: "chip--accent", label: "SUCCESS" },
  issue: { chip: "chip--orange", label: "ISSUE" },
  decision_needed: { chip: "chip--orange", label: "DECISION NEEDED" },
};

function turnCardHtml(turn, { linkToDetail }) {
  const openAttr = linkToDetail ? `data-turn="${turn.id}"` : "";
  let audioHtml = "";
  if (turn.summary) {
    const cat = CATEGORY_META[turn.category];
    const catChip = cat ? `<span class="chip ${cat.chip}">${cat.label}</span>` : "";
    // A category can be toggled off in Settings — still classified/summarized for the
    // transcript, just not spoken, so there's no audio to play back.
    const player = turn.audioUrl
      ? `<div class="audio-strip__row">
          <button class="audio-strip__play" data-replay="${turn.id}">▶</button>
          <div class="waveform" id="wf-${turn.id}"></div>
          <span class="audio-strip__time">${turn.audioDuration || "0:00"}</span>
        </div>`
      : "";
    audioHtml = `
      <div class="audio-strip">
        ${catChip ? `<div style="margin-bottom:8px;">${catChip}</div>` : ""}
        ${player}
        <div class="audio-strip__summary">${escapeHtml(turn.summary)}</div>
      </div>`;
  } else if (turn.status === "pending") {
    audioHtml = `
      <div class="working-row">
        <span class="status-dot status-dot--working"></span>
        <span class="working-row__label">working…</span>
      </div>`;
  } else if (turn.status === "error") {
    audioHtml = `<div class="working-row"><span class="status-dot status-dot--blocked"></span>
      <span style="color:var(--kv-danger)">${escapeHtml(turn.error || "failed")}</span></div>`;
  }
  return `
    <div class="card" ${openAttr}>
      <div class="card__meta">
        <span class="card__label">YOU · DICTATED</span>
        <span class="card__time">${timeAgo(turn.timestamp)}</span>
      </div>
      <div class="card__you-text">"${escapeHtml(turn.inputText)}"</div>
      ${sentSectionHtml(turn)}
      ${audioHtml}
      ${turn.summary ? `<div class="turn-actions">
        <a href="#/turn/${turn.id}">▸ raw output</a>
        ${turn.audioUrl ? `<button class="is-muted" data-replay="${turn.id}">↻ replay</button>` : ""}
      </div>` : ""}
    </div>`;
}

function renderConversation() {
  const screen = document.getElementById("screen");
  const turns = state.transcripts[state.currentPaneId] || [];
  if (turns.length === 0) {
    const hint =
      state.currentPaneId === SUPERVISOR_ID
        ? "no turns yet — ask the supervisor to check on or dispatch to your fleet"
        : "no turns yet — dictate or type a command below";
    screen.innerHTML = `<div class="empty-state">${hint}</div>`;
    return;
  }
  screen.innerHTML = turns.map((t) => turnCardHtml(t, { linkToDetail: true })).join("");
  screen.querySelectorAll("[data-replay]").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      const turn = turns.find((t) => t.id === btn.dataset.replay);
      if (turn && turn.audioUrl) playAudio(turn.audioUrl, turn.summary);
    });
  });
  screen.querySelectorAll("[data-turn]").forEach((card) => {
    card.addEventListener("click", () => setHash(`turn/${card.dataset.turn}`));
  });
  turns.forEach((t) => {
    if (t.summary) {
      const wf = document.getElementById(`wf-${t.id}`);
      if (wf) renderWaveform(wf, 18, null);
    }
  });
  screen.scrollTop = screen.scrollHeight;
}

// ---------- turn detail screen ----------

// One "menu resolved" card (if the reply was sent as keys) + one raw-output toggle card, for a
// single pane's dispatch. `domId` must be unique within the turn-detail screen — a supervisor
// turn renders one of these per dispatched pane.
function dispatchDetailHtml(domId, { sentMode, sentContent, preSendContext, rawOutput, menuLabel, rawLabel }) {
  let html = "";
  if (sentMode === "keys") {
    const context = stripAnsi(preSendContext || "").trim().split("\n").slice(-4).join("\n");
    html += `
      <div class="card card--accent">
        <div class="card__label" style="color:var(--kv-orange); margin-bottom:9px;">${menuLabel ? escapeHtml(menuLabel) + " · " : ""}MENU RESOLVED · AUTO</div>
        <pre style="white-space:pre-wrap; font-family:var(--font-term); font-size:var(--type-data); color:var(--kv-ink); margin:0 0 8px;">${escapeHtml(context)}</pre>
        <div class="card__section-label">KEYS SENT</div>
        <div class="sent-box sent-box--keys">${escapeHtml(sentContent)}</div>
      </div>`;
  }
  const rawText = stripAnsi(rawOutput || "");
  html += `
    <div class="card" style="padding:0">
      <div class="raw-output__header" data-toggle-raw="${domId}">
        <span class="card__label" style="color:var(--kv-accent)">▾ RAW OUTPUT${rawLabel ? " · " + escapeHtml(rawLabel) : ""}</span>
        <span class="card__time">tap to toggle</span>
      </div>
      <div class="raw-output" id="raw-${domId}" style="display:none">
        <pre>${escapeHtml(rawText) || "(empty)"}</pre>
      </div>
    </div>`;
  return html;
}

function renderTurnDetail(id) {
  const screen = document.getElementById("screen");
  const { turn, index } = findTurnAnywhere(id);
  if (!turn) {
    screen.innerHTML = `<div class="empty-state">turn not found</div>`;
    return;
  }
  document.getElementById("back-header-title").textContent = "TURN " + (index + 1);
  if (turn.status !== "pending") {
    const chip = document.getElementById("back-header-chip");
    chip.style.display = "inline-flex";
    chip.className = "chip " + (turn.status === "error" ? "chip--orange" : "chip--accent");
    chip.textContent = turn.status === "error" ? "FAILED" : "DONE";
  }

  let detailHtml = "";
  if (turn.isSupervisor) {
    for (const d of turn.dispatches || []) {
      const label = d.paneName || d.paneId;
      detailHtml += dispatchDetailHtml(`${turn.id}-${d.paneId}`, {
        sentMode: d.sentMode,
        sentContent: d.sentContent,
        preSendContext: d.preSendContext,
        rawOutput: d.rawOutput,
        menuLabel: label,
        rawLabel: label,
      });
    }
  } else {
    detailHtml = dispatchDetailHtml(turn.id, {
      sentMode: turn.sentMode,
      sentContent: turn.sentContent,
      preSendContext: turn.preSendContext,
      rawOutput: turn.rawOutput,
      menuLabel: null,
      rawLabel: turn.paneId,
    });
  }

  screen.innerHTML = turnCardHtml(turn, { linkToDetail: false }) + detailHtml;
  const replay = screen.querySelector("[data-replay]");
  if (replay) {
    replay.addEventListener("click", () => turn.audioUrl && playAudio(turn.audioUrl, turn.summary));
  }
  const wf = document.getElementById(`wf-${turn.id}`);
  if (wf) renderWaveform(wf, 18, null);
  screen.querySelectorAll("[data-toggle-raw]").forEach((toggle) => {
    toggle.addEventListener("click", () => {
      const el = document.getElementById(`raw-${toggle.dataset.toggleRaw}`);
      el.style.display = el.style.display === "none" ? "block" : "none";
    });
  });
}

// ---------- fleet screen ----------

function renderFleet() {
  const screen = document.getElementById("screen");
  const supervisorTalking = state.currentPaneId === SUPERVISOR_ID ? " fleet-pane--talking" : "";
  let html = `<div class="fleet-pane${supervisorTalking}" data-pane="${SUPERVISOR_ID}" style="margin-bottom:18px;">
    <span class="status-dot status-dot--done"></span>
    <div class="fleet-pane__body">
      <div class="fleet-pane__name">SUPERVISOR</div>
      <div class="fleet-pane__meta">fleet-wide — decides which pane(s) to act on, or answers directly</div>
    </div>
  </div>`;

  if (!state.snapshot) {
    html += `<div class="empty-state">no data yet — waiting for Collie…</div>`;
    screen.innerHTML = html;
    wireFleetPaneClicks(screen);
    return;
  }
  const groups = new Map();
  for (const pane of allPanes(state.snapshot)) {
    const key = pane.workspaceLabel || "workspace";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(pane);
  }
  if (groups.size === 0) {
    html += `<div class="empty-state">no panes running</div>`;
    screen.innerHTML = html;
    wireFleetPaneClicks(screen);
    return;
  }
  html += `<div class="fleet-session">
    <div class="fleet-session__header">
      <span class="status-dot ${state.bridgeReachable ? "status-dot--working" : "status-dot--blocked"}"></span>
      <span class="fleet-session__label">${escapeHtml((state.settings && state.settings.collieBaseUrl) || "collie")}</span>
    </div>`;
  for (const [wsLabel, panes] of groups) {
    html += `<div class="fleet-workspace">
      <div class="fleet-workspace__label">└ ${escapeHtml(wsLabel)}</div>`;
    for (const pane of panes) {
      const meta = statusMeta(pane.status);
      const talking = pane.paneId === state.currentPaneId ? " fleet-pane--talking" : "";
      html += `<div class="fleet-pane${talking}" data-pane="${pane.paneId}">
        <span class="status-dot ${meta.dot}"></span>
        <div class="fleet-pane__body">
          <div class="fleet-pane__name truncate">${escapeHtml(paneDisplayName(pane))}</div>
          <div class="fleet-pane__meta truncate">${escapeHtml(pane.cwd || "")}</div>
        </div>
        <span class="chip ${meta.chip}">${meta.label}</span>
      </div>`;
    }
    html += `</div>`;
  }
  html += `</div><div class="empty-state">— end of fleet —</div>`;
  screen.innerHTML = html;
  wireFleetPaneClicks(screen);
}

function wireFleetPaneClicks(screen) {
  screen.querySelectorAll("[data-pane]").forEach((row) => {
    row.addEventListener("click", () => {
      state.currentPaneId = row.dataset.pane;
      localStorage.setItem(CURRENT_PANE_KEY, state.currentPaneId);
      setHash("conversation");
      reportForegroundContext();
    });
  });
}

// ---------- tasks view ----------
//
// Ports collie-server's static/tasks.html reference UI 1:1 (nested parent/child cards, status
// badges, create form, split/dispatch/delete buttons, the busy-flag-gates-auto-refresh pattern)
// onto invoke("list_todos"/"create_todo"/...) instead of fetch, styled with app.css's --kv-*
// tokens instead of tasks.html's literal hex colors so Chat and Tasks share one visual language.

const TODO_STATUSES = ["pending", "dispatched", "in_progress", "blocked", "done", "failed"];

async function loadTasks() {
  if (state.tasksBusy) return;
  try {
    state.tasks = await invoke("list_todos");
    state.tasksError = null;
  } catch (err) {
    state.tasksError = String(err);
  }
  renderTasks();
}

function paneSelectOptionsHtml(selectedPaneId) {
  return allPanes(state.snapshot)
    .map(
      (p) =>
        `<option value="${escapeHtml(p.paneId)}" ${String(p.paneId) === String(selectedPaneId) ? "selected" : ""}>${escapeHtml(paneDisplayName(p))} (#${escapeHtml(p.paneId)})</option>`,
    )
    .join("");
}

function renderParentOptions() {
  const select = document.getElementById("task-parent");
  const current = select.value;
  const topLevel = state.tasks.filter((t) => !t.parentId);
  select.innerHTML =
    '<option value="">— top-level —</option>' +
    topLevel.map((t) => `<option value="${escapeHtml(t.id)}">${escapeHtml(t.title)}</option>`).join("");
  select.value = current;
}

function renderTasks() {
  const screen = document.getElementById("tasks-screen");
  if (state.tasksError) {
    screen.innerHTML = `<div class="empty-state">failed to load tasks: ${escapeHtml(state.tasksError)}</div>`;
    return;
  }
  renderParentOptions();
  const byParent = new Map();
  for (const t of state.tasks) {
    const key = t.parentId || "";
    if (!byParent.has(key)) byParent.set(key, []);
    byParent.get(key).push(t);
  }
  const topLevel = byParent.get("") || [];
  if (topLevel.length === 0) {
    screen.innerHTML = `<div class="empty-state">no tasks yet — add one above.</div>`;
    return;
  }
  screen.innerHTML = "";
  for (const t of topLevel) {
    screen.appendChild(taskCardEl(t, byParent, false));
  }
}

function taskCardEl(t, byParent, isChild) {
  const wrap = document.createElement("div");
  wrap.className = "task" + (isChild ? " task--child" : "");

  const top = document.createElement("div");
  top.className = "task-top";
  top.innerHTML = `<span class="task-title">${escapeHtml(t.title)}</span><span class="badge badge-${t.status}">${t.status}</span>`;
  wrap.appendChild(top);

  if (t.description) {
    const desc = document.createElement("div");
    desc.className = "task-desc";
    desc.textContent = t.description;
    wrap.appendChild(desc);
  }

  const meta = document.createElement("div");
  meta.className = "task-meta";
  meta.textContent = `#${t.id.slice(0, 8)}` + (t.assignedPaneId != null ? ` · pane ${t.assignedPaneId}` : "");
  wrap.appendChild(meta);

  const controls = document.createElement("div");
  controls.className = "task-controls";

  const statusSelect = document.createElement("select");
  statusSelect.innerHTML = TODO_STATUSES.map(
    (s) => `<option value="${s}" ${s === t.status ? "selected" : ""}>${s}</option>`,
  ).join("");
  statusSelect.addEventListener("change", () => setTaskStatus(t.id, statusSelect.value));
  controls.appendChild(statusSelect);

  const splitBtn = document.createElement("button");
  splitBtn.textContent = "Split";
  splitBtn.addEventListener("click", () => splitTask(t.id, splitBtn));
  controls.appendChild(splitBtn);

  const panes = allPanes(state.snapshot);
  const paneSelect = document.createElement("select");
  paneSelect.innerHTML = panes.length
    ? paneSelectOptionsHtml(t.assignedPaneId ?? "")
    : '<option value="">no panes available</option>';
  controls.appendChild(paneSelect);

  const dispatchBtn = document.createElement("button");
  dispatchBtn.textContent = "Dispatch";
  dispatchBtn.disabled = panes.length === 0;
  dispatchBtn.addEventListener("click", () => dispatchTask(t.id, paneSelect.value, dispatchBtn));
  controls.appendChild(dispatchBtn);

  const deleteBtn = document.createElement("button");
  deleteBtn.textContent = "Delete";
  deleteBtn.className = "danger";
  deleteBtn.addEventListener("click", () => deleteTask(t.id, deleteBtn));
  controls.appendChild(deleteBtn);

  wrap.appendChild(controls);

  const children = byParent.get(t.id) || [];
  if (children.length) {
    const childrenWrap = document.createElement("div");
    childrenWrap.className = "task-children";
    for (const c of children) {
      childrenWrap.appendChild(taskCardEl(c, byParent, true));
    }
    wrap.appendChild(childrenWrap);
  }

  return wrap;
}

async function setTaskStatus(id, status) {
  state.tasksBusy = true;
  try {
    await invoke("patch_todo", { id, status });
  } catch (err) {
    alert(`Failed to update status: ${err}`);
  } finally {
    state.tasksBusy = false;
    loadTasks();
  }
}

async function splitTask(id, btn) {
  state.tasksBusy = true;
  btn.disabled = true;
  btn.textContent = "splitting...";
  try {
    await invoke("split_todo", { id });
  } catch (err) {
    alert(`Split failed: ${err}`);
  } finally {
    state.tasksBusy = false;
    loadTasks();
  }
}

async function dispatchTask(id, paneId, btn) {
  if (!paneId) {
    alert("Pick a pane to dispatch to first.");
    return;
  }
  state.tasksBusy = true;
  btn.disabled = true;
  btn.textContent = "dispatching...";
  try {
    await invoke("dispatch_todo", { id, paneId });
  } catch (err) {
    alert(`Dispatch failed: ${err}`);
  } finally {
    state.tasksBusy = false;
    loadTasks();
  }
}

async function deleteTask(id, btn) {
  if (!confirm("Delete this task?")) return;
  state.tasksBusy = true;
  btn.disabled = true;
  try {
    await invoke("delete_todo", { id });
  } catch (err) {
    alert(`Delete failed: ${err}`);
  } finally {
    state.tasksBusy = false;
    loadTasks();
  }
}

function wireTasksControls() {
  const titleInput = document.getElementById("task-title");
  const descInput = document.getElementById("task-desc");
  const parentSelect = document.getElementById("task-parent");
  const addBtn = document.getElementById("task-add");

  const addTask = async () => {
    const title = titleInput.value.trim();
    if (!title) return;
    state.tasksBusy = true;
    addBtn.disabled = true;
    try {
      await invoke("create_todo", {
        title,
        description: descInput.value.trim() || null,
        parentId: parentSelect.value || null,
      });
      titleInput.value = "";
      descInput.value = "";
    } catch (err) {
      alert(`Failed to add task: ${err}`);
    } finally {
      state.tasksBusy = false;
      addBtn.disabled = false;
      loadTasks();
    }
  };

  addBtn.addEventListener("click", addTask);
  titleInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") addTask();
  });
}

// ---------- settings screen ----------

function renderSettings() {
  const screen = document.getElementById("settings-screen");
  const s = state.settings || {};
  screen.innerHTML = `
    <div class="settings-section">
      <div class="settings-section__label">CONNECTION</div>
      <div class="card">
        <div class="settings-row">
          <div class="settings-row__field">
            <span class="settings-row__field-label">COLLIE BRIDGE</span>
            <span class="settings-row__field-value">${escapeHtml(s.collieBaseUrl || "not set")}</span>
          </div>
          <span class="chip ${state.bridgeReachable ? "chip--accent" : "chip--orange"}">
            <span class="chip__dot"></span>${state.bridgeReachable ? "REACHABLE" : "UNREACHABLE"}
          </span>
        </div>
      </div>
    </div>

    <div class="settings-section">
      <div class="settings-section__label">CONNECTION SETUP</div>
      <p style="font-size:var(--type-meta); color:var(--kv-dim); margin:-4px 0 12px;">
        model/TTS/voice-toggle settings now live on collie-server itself, not in this app.
      </p>
      <div class="card">
        <div class="settings-row">
          <div class="settings-row__field" style="width:100%">
            <span class="settings-row__field-label">COLLIE BASE URL</span>
            <input type="text" id="f-collie-url" value="${escapeHtml(s.collieBaseUrl || "")}" />
          </div>
        </div>
      </div>
    </div>

    <div class="settings-section">
      <div class="settings-section__label">NOTIFICATIONS</div>
      <div class="card">
        <div class="settings-row">
          <div class="settings-row__field" style="width:100%">
            <span class="settings-row__field-label">ESCALATE TO SPEECH AFTER (SECONDS)</span>
            <input type="text" inputmode="numeric" id="f-notify-timeout" value="${escapeHtml(String(s.notificationResponseTimeoutSecs ?? 90))}" />
          </div>
        </div>
      </div>
    </div>

    <button class="btn btn--lg" id="save-settings" style="align-self:flex-start">SAVE</button>
    <div class="empty-state" id="settings-status"></div>
  `;

  document.getElementById("save-settings").addEventListener("click", async () => {
    const timeoutRaw = parseInt(document.getElementById("f-notify-timeout").value, 10);
    const newSettings = {
      collieBaseUrl: document.getElementById("f-collie-url").value.trim(),
      notificationResponseTimeoutSecs: Number.isFinite(timeoutRaw) && timeoutRaw > 0 ? timeoutRaw : 90,
    };
    const status = document.getElementById("settings-status");
    try {
      await invoke("save_settings", { newSettings });
      state.settings = newSettings;
      status.textContent = "saved.";
      renderChrome(parseHash().view);
    } catch (err) {
      status.textContent = "error: " + err;
    }
  });
}

// ---------- blocked overlay ----------

async function showBlockedOverlay(pane) {
  state.blockedPane = { paneId: pane.paneId, name: paneDisplayName(pane), since: Date.now() };
  document.getElementById("blocked-pane-name").textContent = state.blockedPane.name;
  document.getElementById("blocked-since").textContent = "blocked";
  document.getElementById("blocked-overlay").classList.add("is-open");

  const promptEl = document.getElementById("blocked-prompt");
  const optionsEl = document.getElementById("blocked-options");
  promptEl.textContent = "…";
  optionsEl.innerHTML = "";

  let description;
  try {
    description = await invoke("describe_blocked_prompt", { paneId: pane.paneId });
  } catch (err) {
    promptEl.textContent = String(err);
    return;
  }
  // The overlay might already be dismissed/pointed elsewhere by the time this resolves.
  if (!state.blockedPane || state.blockedPane.paneId !== pane.paneId) return;

  promptEl.textContent = description.question || "(no prompt captured)";
  (description.options || []).forEach((option, i) => {
    const btn = document.createElement("button");
    btn.className = "btn btn--outline";
    // The model occasionally leaves `label` blank while still filling `instruction` in
    // correctly (the button still works, it's just visually empty) — fall back rather than
    // ever show a blank button.
    const label = (option.label && option.label.trim()) || (option.instruction && option.instruction.trim()) || `Option ${i + 1}`;
    btn.textContent = label;
    btn.addEventListener("click", () => quickBlockedReply(option.instruction || label));
    optionsEl.appendChild(btn);
  });
}

function hideBlockedOverlay() {
  document.getElementById("blocked-overlay").classList.remove("is-open");
}

function wireBlockedOverlay() {
  document.getElementById("blocked-overlay-backdrop").addEventListener("click", hideBlockedOverlay);
  document.getElementById("blocked-dismiss").addEventListener("click", () => {
    if (state.blockedPane) state.dismissedBlocked.add(state.blockedPane.paneId);
    hideBlockedOverlay();
  });
  document.getElementById("blocked-snooze").addEventListener("click", () => {
    if (state.blockedPane) {
      state.dismissedBlocked.add(state.blockedPane.paneId);
      setTimeout(() => state.dismissedBlocked.delete(state.blockedPane.paneId), 5 * 60 * 1000);
    }
    hideBlockedOverlay();
  });
  document.getElementById("blocked-voice-reply").addEventListener("click", () => {
    if (!state.blockedPane) return;
    state.currentPaneId = state.blockedPane.paneId;
    localStorage.setItem(CURRENT_PANE_KEY, state.currentPaneId);
    hideBlockedOverlay();
    setHash("conversation");
    reportForegroundContext();
    document.getElementById("command-input").focus();
  });
}

function quickBlockedReply(text) {
  if (!state.blockedPane) return;
  const paneId = state.blockedPane.paneId;
  primeAudioPlayback();
  hideBlockedOverlay();
  sendCommand(text, paneId);
}

// ---------- sending commands ----------

async function sendCommand(text, paneIdOverride) {
  // An explicit override (e.g. a blocked-pane quick reply) always targets that specific pane,
  // regardless of what's currently selected — only the un-overridden "current selection" can be
  // the supervisor.
  const targetPaneId = paneIdOverride || state.currentPaneId;
  const isSupervisor = !paneIdOverride && targetPaneId === SUPERVISOR_ID;
  const bucketKey = isSupervisor ? SUPERVISOR_ID : targetPaneId || "unassigned";

  const turn = {
    id: `t${Date.now()}${Math.random().toString(36).slice(2, 6)}`,
    paneId: isSupervisor ? null : targetPaneId || null,
    isSupervisor,
    timestamp: Date.now(),
    inputText: text,
    status: "pending",
  };
  pushTurn(bucketKey, turn);
  saveTranscripts();
  if (parseHash().view === "conversation") renderConversation();

  try {
    let audioUrl;
    if (isSupervisor) {
      const result = await invoke("send_supervisor_command", { text });
      Object.assign(turn, {
        status: "done",
        dispatches: result.dispatches,
        summary: result.summary,
        category: result.category,
        audioUrl: result.audioUrl,
      });
      audioUrl = result.audioUrl;
    } else {
      const result = await invoke("send_command", { text, paneId: targetPaneId || null });
      const d = result.dispatch;
      Object.assign(turn, {
        status: "done",
        paneId: d.paneId,
        sentMode: d.sentMode,
        sentContent: d.sentContent,
        preSendContext: d.preSendContext,
        rawOutput: d.rawOutput,
        summary: result.summary,
        category: result.category,
        audioUrl: result.audioUrl,
      });
      audioUrl = result.audioUrl;
      if (!paneIdOverride && !state.currentPaneId) {
        state.currentPaneId = d.paneId;
        localStorage.setItem(CURRENT_PANE_KEY, d.paneId);
      }
    }
    // The backend already decided whether this category should be spoken — an empty string
    // means it was deliberately skipped (toggled off in Settings), not a failure.
    if (audioUrl) playAudio(audioUrl, turn.summary);
  } catch (err) {
    Object.assign(turn, { status: "error", error: String(err) });
  }
  saveTranscripts();
  if (parseHash().view === "conversation") renderConversation();
  renderChrome(parseHash().view);
}

// ---------- polling ----------
//
// The 6s poll now runs once, in Rust (monitor.rs's poll_once), so it keeps running whether the
// app is foregrounded, backgrounded, or the screen is off — that's the whole point of moving it
// off a JS setInterval. This file only reacts to what Rust reports: `collie://snapshot` fires
// every cycle with the latest snapshot, and `collie://tts-playing` fires when the monitor
// escalates a blocked pane to speech. `get_snapshot` stays as an on-demand command for first
// paint (before the background service has started) and manual refresh.

function applySnapshot(snapshot) {
  state.bridgeReachable = true;
  state.snapshot = snapshot;

  // The in-webview blocked overlay is only for the pane the user is actively looking at — every
  // other blocked pane is Rust's job to notify about (monitor.rs's own foreground-suppression
  // check skips exactly that one pane so this and the OS notification don't double up).
  for (const pane of allPanes(snapshot)) {
    const prev = state.prevStatus[pane.paneId];
    const isViewedInForeground =
      document.visibilityState === "visible" && pane.paneId === state.currentPaneId && parseHash().view === "conversation";
    if (
      pane.status === "blocked" &&
      prev !== "blocked" &&
      isViewedInForeground &&
      !state.dismissedBlocked.has(pane.paneId)
    ) {
      showBlockedOverlay(pane);
    }
    if (pane.status !== "blocked") state.dismissedBlocked.delete(pane.paneId);
    state.prevStatus[pane.paneId] = pane.status;
  }

  const view = parseHash().view;
  renderChrome(view);
  if (view === "fleet") renderFleet();
  if (state.activeView === "tasks") loadTasks();
}

async function fetchSnapshotOnce() {
  try {
    applySnapshot(await invoke("get_snapshot"));
  } catch {
    state.bridgeReachable = false;
    renderChrome(parseHash().view);
  }
}

function startPolling() {
  fetchSnapshotOnce();
  tauriListen("collie://snapshot", (event) => applySnapshot(event.payload));
  tauriListen("collie://tts-playing", (event) => {
    const { audioUrl, text } = event.payload;
    playAudio(audioUrl, text);
  });
  // Re-check on resume — the OS may have paused JS timers/network while backgrounded, so the
  // last-known snapshot on screen could be stale by the time the user looks at the app again.
  document.addEventListener("visibilitychange", () => {
    reportForegroundContext();
    if (document.visibilityState === "visible") fetchSnapshotOnce();
  });
}

// Tells monitor.rs what the user is currently looking at, so it can skip firing a duplicate OS
// notification for the one pane already covered by the in-webview blocked overlay.
function reportForegroundContext() {
  const visible = document.visibilityState === "visible" && parseHash().view === "conversation";
  const viewingPaneId = state.currentPaneId !== SUPERVISOR_ID ? state.currentPaneId : null;
  invoke("set_foreground_context", { visible, viewingPaneId }).catch(() => {});
}

// ---------- app render dispatch ----------

function renderApp() {
  const { view, param } = parseHash();
  renderChrome(view);
  if (view === "conversation") renderConversation();
  else if (view === "fleet") renderFleet();
  else if (view === "turn") renderTurnDetail(param);
  else setHash("conversation");
}

// ---------- settings overlay (persistent cog, not a swipe pane) ----------

function openSettings() {
  renderSettings();
  document.getElementById("settings-overlay").classList.add("is-open");
}

function closeSettings() {
  document.getElementById("settings-overlay").classList.remove("is-open");
}

// ---------- wiring ----------

function wireGlobalControls() {
  document.getElementById("settings-btn").addEventListener("click", openSettings);
  document.getElementById("settings-back-btn").addEventListener("click", closeSettings);
  document.getElementById("attention-chip").addEventListener("click", () => {
    setActiveView("chat");
    setHash("fleet");
  });
  document.getElementById("back-btn").addEventListener("click", () => setHash("conversation"));
  document.getElementById("talking-to-bar").addEventListener("click", () => setHash("fleet"));
  document.getElementById("tab-chat").addEventListener("click", () => setActiveView("chat"));
  document.getElementById("tab-tasks").addEventListener("click", () => setActiveView("tasks"));

  const input = document.getElementById("command-input");
  const submit = () => {
    const text = input.value.trim();
    if (!text) return;
    input.value = "";
    // Must happen synchronously inside this gesture handler, before any async work — see
    // primeAudioPlayback's comment.
    primeAudioPlayback();
    sendCommand(text);
  };
  document.getElementById("talk-btn").addEventListener("click", submit);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submit();
  });
}

// Horizontal swipe between the Chat and Tasks view-panes — live-tracks the finger while
// dragging, snaps to whichever pane is closer than a threshold on release. Replaces
// wireSwipeToSettings outright: settings is reached via the persistent cog now, and running
// both gesture handlers at once would have them fight over the same touches.
function wireViewSwipe() {
  const track = document.getElementById("view-track");
  const SNAP_FRACTION = 0.25; // fraction of screen width to trigger a view change
  let startX = null;
  let startY = null;
  let dragging = false;
  let baseOffsetPx = 0; // -window.innerWidth when on "tasks", 0 when on "chat"

  document.addEventListener(
    "touchstart",
    (e) => {
      if (e.touches.length !== 1) return;
      const t = e.touches[0];
      startX = t.clientX;
      startY = t.clientY;
      dragging = false;
      baseOffsetPx = state.activeView === "tasks" ? -window.innerWidth : 0;
    },
    { passive: true },
  );

  document.addEventListener(
    "touchmove",
    (e) => {
      if (startX == null || e.touches.length !== 1) return;
      const t = e.touches[0];
      const dx = t.clientX - startX;
      const dy = Math.abs(t.clientY - startY);
      if (!dragging) {
        if (Math.abs(dx) < 10 || dy > Math.abs(dx)) return; // vertical scroll, not a swipe
        dragging = true;
        track.classList.add("is-dragging");
      }
      const clamped = Math.min(0, Math.max(-window.innerWidth, baseOffsetPx + dx));
      track.style.transform = `translateX(${clamped}px)`;
    },
    { passive: true },
  );

  document.addEventListener(
    "touchend",
    (e) => {
      if (!dragging) {
        startX = null;
        return;
      }
      track.classList.remove("is-dragging");
      const t = e.changedTouches[0];
      const dx = t.clientX - startX;
      const goingToTasks = baseOffsetPx + dx < -window.innerWidth * SNAP_FRACTION;
      setActiveView(goingToTasks ? "tasks" : "chat");
      startX = null;
      startY = null;
      dragging = false;
    },
    { passive: true },
  );
}

async function init() {
  wireGlobalControls();
  wireBlockedOverlay();
  wireViewSwipe();
  wireTasksControls();
  // Broad safety net: primes audio on literally the first tap anywhere, so a blocked-pane alert
  // that fires from background polling (no gesture of its own) still has a chance to play if
  // the operator has touched the app at all already this session.
  document.addEventListener("pointerdown", primeAudioPlayback, { once: true });
  try {
    state.settings = await invoke("get_settings");
  } catch {
    state.settings = null;
  }
  // Consumes a pane id left behind by monitor.rs's take_pending_navigation() if the app was
  // just opened after a "pane needs you" notification fired — jumps straight to that pane's
  // chat view via the same pane-switch mechanism wireFleetPaneClicks uses.
  try {
    const pendingPaneId = await invoke("take_pending_navigation");
    if (pendingPaneId) {
      state.currentPaneId = pendingPaneId;
      localStorage.setItem(CURRENT_PANE_KEY, pendingPaneId);
      setActiveView("chat");
      setHash("conversation");
    }
  } catch {
    // no-op — deep link is a nicety, not required for the app to work
  }
  setActiveView(state.activeView);
  renderApp();
  startPolling();
  reportForegroundContext();
  // Keeps monitor.rs's poll_once running with the app backgrounded/screen-off — the always-on
  // notify→escalate pipeline this whole redesign exists for. No npm package: there's no bundler
  // in this project, so this calls the plugin's raw invoke command directly instead of importing
  // its guest-js bindings.
  try {
    await invoke("plugin:background-service|start", {
      config: { serviceLabel: "Collie monitoring", foregroundServiceType: "dataSync" },
    });
  } catch (err) {
    console.error("background-service start failed", err);
  }
}

init();
