const invoke = window.__TAURI__.core.invoke;

const TRANSCRIPT_KEY = "collie_transcript";
const CURRENT_PANE_KEY = "collie_current_pane";
const AUTOPLAY_KEY = "collie_autoplay";
const SPEAK_BLOCKED_KEY = "collie_speak_blocked";
const MAX_TRANSCRIPT = 200;
const POLL_MS = 6000;

const state = {
  settings: null,
  snapshot: null,
  bridgeReachable: null,
  prevStatus: {}, // paneId -> status, to edge-detect transitions into "blocked"
  currentPaneId: localStorage.getItem(CURRENT_PANE_KEY) || null,
  transcript: loadTranscript(),
  blockedPane: null, // { paneId, name, prompt } currently shown in the overlay
  dismissedBlocked: new Set(), // paneIds snoozed/dismissed until they clear "blocked"
  pending: null, // in-flight turn while send_command is running
  audio: null,
};

// ---------- storage ----------

function loadTranscript() {
  try {
    const raw = localStorage.getItem(TRANSCRIPT_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch {
    return [];
  }
}

function saveTranscript() {
  const trimmed = state.transcript.slice(-MAX_TRANSCRIPT);
  localStorage.setItem(TRANSCRIPT_KEY, JSON.stringify(trimmed));
}

function getBoolPref(key, fallback) {
  const raw = localStorage.getItem(key);
  return raw === null ? fallback : raw === "1";
}

function setBoolPref(key, value) {
  localStorage.setItem(key, value ? "1" : "0");
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

function playAudio(audioBase64, format) {
  stopAudio();
  const audio = new Audio(`data:audio/${format || "mp3"};base64,${audioBase64}`);
  state.audio = audio;
  const bar = document.getElementById("now-playing");
  const wf = document.getElementById("now-playing-waveform");
  const timeEl = document.getElementById("now-playing-time");
  const toggle = document.getElementById("now-playing-toggle");
  bar.style.display = "flex";
  renderWaveform(wf, 24, 0);
  toggle.textContent = "❚❚";
  audio.addEventListener("timeupdate", () => {
    const frac = audio.duration ? audio.currentTime / audio.duration : 0;
    renderWaveform(wf, 24, frac);
    const remaining = Math.max(0, (audio.duration || 0) - audio.currentTime);
    const mm = Math.floor(remaining / 60);
    const ss = Math.floor(remaining % 60).toString().padStart(2, "0");
    timeEl.textContent = `${mm}:${ss}`;
  });
  audio.addEventListener("ended", () => {
    bar.style.display = "none";
    state.audio = null;
  });
  toggle.onclick = () => {
    if (audio.paused) {
      audio.play();
      toggle.textContent = "❚❚";
    } else {
      audio.pause();
      toggle.textContent = "▶";
    }
  };
  audio.play().catch(() => {
    // Autoplay can be blocked; the now-playing bar's toggle still lets the user start it.
    toggle.textContent = "▶";
  });
}

function stopAudio() {
  if (state.audio) {
    state.audio.pause();
    state.audio = null;
  }
  document.getElementById("now-playing").style.display = "none";
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

window.addEventListener("hashchange", renderApp);

// ---------- top chrome (app bar / back header / dock visibility) ----------

function renderChrome(view) {
  const appBar = document.getElementById("app-bar");
  const backHeader = document.getElementById("back-header");
  const dock = document.getElementById("input-dock");

  const netChip = document.getElementById("net-chip");
  netChip.className = "chip " + (state.bridgeReachable ? "chip--accent chip--pulse" : "chip--orange");
  const keyChip = document.getElementById("key-chip");
  const hasKey = !!(state.settings && state.settings.openrouterApiKey);
  keyChip.className = "chip " + (hasKey ? "chip--accent" : "chip--orange");

  if (view === "conversation") {
    appBar.style.display = "";
    backHeader.style.display = "none";
    dock.style.display = "flex";
    updateTalkingToBar();
  } else {
    appBar.style.display = "none";
    backHeader.style.display = "flex";
    dock.style.display = "none";
    const title = document.getElementById("back-header-title");
    const chip = document.getElementById("back-header-chip");
    chip.style.display = "none";
    if (view === "fleet") {
      title.textContent = "FLEET";
      const blockedCount = allPanes(state.snapshot).filter((p) => p.status === "blocked").length;
      if (blockedCount > 0) {
        chip.style.display = "inline-flex";
        chip.className = "chip chip--orange chip--pulse";
        chip.innerHTML = `<span class="chip__dot"></span>${blockedCount} BLOCKED`;
      }
    } else if (view === "settings") {
      title.textContent = "SETTINGS";
    } else if (view === "turn") {
      title.textContent = "TURN";
    }
  }
}

function updateTalkingToBar() {
  const pane = findPane(state.snapshot, state.currentPaneId);
  document.getElementById("talking-to-name").textContent = paneDisplayName(pane);
  const dot = document.getElementById("talking-to-dot");
  const chip = document.getElementById("talking-to-status");
  const meta = statusMeta(pane ? pane.status : "unknown");
  dot.className = "status-dot " + meta.dot;
  chip.className = "chip " + meta.chip;
  chip.textContent = pane ? meta.label : "—";
}

// ---------- conversation screen ----------

function turnCardHtml(turn, { linkToDetail }) {
  const openAttr = linkToDetail ? `data-turn="${turn.id}"` : "";
  const sentLabel = turn.sentMode === "keys" ? "SENT · KEYS" : "SENT · TYPED";
  const sentBoxClass = turn.sentMode === "keys" ? "sent-box sent-box--keys" : "sent-box";
  let audioHtml = "";
  if (turn.summary) {
    audioHtml = `
      <div class="audio-strip">
        <div class="audio-strip__row">
          <button class="audio-strip__play" data-replay="${turn.id}">▶</button>
          <div class="waveform" id="wf-${turn.id}"></div>
          <span class="audio-strip__time">${turn.audioDuration || "0:00"}</span>
        </div>
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
      <div>
        <div class="card__section-label">${sentLabel}</div>
        <div class="${sentBoxClass}">${escapeHtml(turn.sentContent || turn.inputText)}</div>
      </div>
      ${audioHtml}
      ${turn.summary ? `<div class="turn-actions">
        <a href="#/turn/${turn.id}">▸ raw output</a>
        <button class="is-muted" data-replay="${turn.id}">↻ replay</button>
      </div>` : ""}
    </div>`;
}

function renderConversation() {
  const screen = document.getElementById("screen");
  if (state.transcript.length === 0 && !state.pending) {
    screen.innerHTML = `<div class="empty-state">no turns yet — dictate or type a command below</div>`;
    return;
  }
  const turns = state.transcript.slice().reverse(); // newest last visually handled by scroll
  let html = state.transcript.map((t) => turnCardHtml(t, { linkToDetail: true })).join("");
  screen.innerHTML = html;
  screen.querySelectorAll("[data-replay]").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      const turn = state.transcript.find((t) => t.id === btn.dataset.replay);
      if (turn && turn.audioBase64) playAudio(turn.audioBase64, turn.audioFormat);
    });
  });
  screen.querySelectorAll("[data-turn]").forEach((card) => {
    card.addEventListener("click", () => setHash(`turn/${card.dataset.turn}`));
  });
  state.transcript.forEach((t) => {
    if (t.summary) {
      const wf = document.getElementById(`wf-${t.id}`);
      if (wf) renderWaveform(wf, 18, null);
    }
  });
  screen.scrollTop = screen.scrollHeight;
}

// ---------- turn detail screen ----------

function renderTurnDetail(id) {
  const screen = document.getElementById("screen");
  const turn = state.transcript.find((t) => t.id === id);
  if (!turn) {
    screen.innerHTML = `<div class="empty-state">turn not found</div>`;
    return;
  }
  document.getElementById("back-header-title").textContent =
    "TURN " + (state.transcript.findIndex((t) => t.id === id) + 1);
  if (turn.status !== "pending") {
    const chip = document.getElementById("back-header-chip");
    chip.style.display = "inline-flex";
    chip.className = "chip " + (turn.status === "error" ? "chip--orange" : "chip--accent");
    chip.textContent = turn.status === "error" ? "FAILED" : "DONE";
  }

  let menuHtml = "";
  if (turn.sentMode === "keys") {
    const context = stripAnsi(turn.preSendContext || "").trim().split("\n").slice(-4).join("\n");
    menuHtml = `
      <div class="card card--accent">
        <div class="card__label" style="color:var(--kv-orange); margin-bottom:9px;">MENU RESOLVED · AUTO</div>
        <pre style="white-space:pre-wrap; font-family:var(--font-term); font-size:var(--type-data); color:var(--kv-ink); margin:0 0 8px;">${escapeHtml(context)}</pre>
        <div class="card__section-label">KEYS SENT</div>
        <div class="sent-box sent-box--keys">${escapeHtml(turn.sentContent)}</div>
      </div>`;
  }

  const rawText = stripAnsi(turn.rawOutput || "");
  const rawHtml = `
    <div class="card" style="padding:0">
      <div class="raw-output__header" data-toggle-raw="${turn.id}">
        <span class="card__label" style="color:var(--kv-accent)">▾ RAW OUTPUT${turn.paneId ? " · " + escapeHtml(turn.paneId) : ""}</span>
        <span class="card__time">tap to toggle</span>
      </div>
      <div class="raw-output" id="raw-${turn.id}" style="display:none">
        <pre>${escapeHtml(rawText) || "(empty)"}</pre>
      </div>
    </div>`;

  screen.innerHTML = turnCardHtml(turn, { linkToDetail: false }) + menuHtml + rawHtml;
  const replay = screen.querySelector("[data-replay]");
  if (replay) {
    replay.addEventListener("click", () => turn.audioBase64 && playAudio(turn.audioBase64, turn.audioFormat));
  }
  const wf = document.getElementById(`wf-${turn.id}`);
  if (wf) renderWaveform(wf, 18, null);
  const toggle = screen.querySelector("[data-toggle-raw]");
  if (toggle) {
    toggle.addEventListener("click", () => {
      const el = document.getElementById(`raw-${turn.id}`);
      el.style.display = el.style.display === "none" ? "block" : "none";
    });
  }
}

// ---------- fleet screen ----------

function renderFleet() {
  const screen = document.getElementById("screen");
  if (!state.snapshot) {
    screen.innerHTML = `<div class="empty-state">no data yet — waiting for Collie…</div>`;
    return;
  }
  const groups = new Map();
  for (const pane of allPanes(state.snapshot)) {
    const key = pane.workspaceLabel || "workspace";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(pane);
  }
  if (groups.size === 0) {
    screen.innerHTML = `<div class="empty-state">no panes running</div>`;
    return;
  }
  let html = `<div class="fleet-session">
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
  screen.querySelectorAll("[data-pane]").forEach((row) => {
    row.addEventListener("click", () => {
      state.currentPaneId = row.dataset.pane;
      localStorage.setItem(CURRENT_PANE_KEY, state.currentPaneId);
      setHash("conversation");
    });
  });
}

// ---------- settings screen ----------

function renderSettings() {
  const screen = document.getElementById("screen");
  const s = state.settings || {};
  const autoplay = getBoolPref(AUTOPLAY_KEY, true);
  const speakBlocked = getBoolPref(SPEAK_BLOCKED_KEY, true);
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
        <div class="settings-row">
          <div class="settings-row__field">
            <span class="settings-row__field-label">OPENROUTER API KEY</span>
            <span class="settings-row__field-value">${s.openrouterApiKey ? "•••• " + s.openrouterApiKey.slice(-4) : "not set"}</span>
          </div>
          <span class="chip ${s.openrouterApiKey ? "chip--accent" : "chip--orange"}">${s.openrouterApiKey ? "SET" : "MISSING"}</span>
        </div>
      </div>
    </div>

    <div class="settings-section">
      <div class="settings-section__label">MODELS</div>
      <div class="card">
        <div class="settings-row">
          <div class="settings-row__field" style="width:100%">
            <span class="settings-row__field-label">RESOLVER · SPOKEN → COMMAND</span>
            <input type="text" id="f-reply-model" value="${escapeHtml(s.replyModel || "")}" />
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row__field" style="width:100%">
            <span class="settings-row__field-label">SUMMARIZER · OUTPUT → SPEECH</span>
            <input type="text" id="f-summarize-model" value="${escapeHtml(s.summarizeModel || "")}" />
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row__field" style="width:100%">
            <span class="settings-row__field-label">TTS MODEL</span>
            <input type="text" id="f-tts-model" value="${escapeHtml(s.ttsModel || "")}" />
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row__field" style="width:100%">
            <span class="settings-row__field-label">TTS VOICE</span>
            <input type="text" id="f-tts-voice" value="${escapeHtml(s.ttsVoice || "")}" />
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row__field" style="width:100%">
            <span class="settings-row__field-label">TTS AUDIO FORMAT</span>
            <input type="text" id="f-tts-format" value="${escapeHtml(s.ttsFormat || "")}" />
          </div>
        </div>
      </div>
    </div>

    <div class="settings-section">
      <div class="settings-section__label">CONNECTION SETUP</div>
      <div class="card">
        <div class="settings-row">
          <div class="settings-row__field" style="width:100%">
            <span class="settings-row__field-label">COLLIE BASE URL</span>
            <input type="text" id="f-collie-url" value="${escapeHtml(s.collieBaseUrl || "")}" />
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row__field" style="width:100%">
            <span class="settings-row__field-label">OPENROUTER API KEY</span>
            <input type="password" id="f-api-key" value="${escapeHtml(s.openrouterApiKey || "")}" />
          </div>
        </div>
      </div>
    </div>

    <div class="settings-section">
      <div class="settings-section__label">VOICE OUT</div>
      <div class="card">
        <div class="settings-row">
          <label style="display:flex; align-items:center; gap:9px; width:100%;">
            <input type="checkbox" id="f-autoplay" ${autoplay ? "checked" : ""} />
            <span style="font-size:var(--type-data); color:var(--kv-ink);">auto-play summary per turn</span>
          </label>
        </div>
        <div class="settings-row">
          <label style="display:flex; align-items:center; gap:9px; width:100%;">
            <input type="checkbox" id="f-speak-blocked" ${speakBlocked ? "checked" : ""} />
            <span style="font-size:var(--type-data); color:var(--kv-ink);">speak blocked-pane alerts aloud</span>
          </label>
        </div>
      </div>
    </div>

    <button class="btn btn--lg" id="save-settings" style="align-self:flex-start">SAVE</button>
    <div class="empty-state" id="settings-status"></div>
  `;

  document.getElementById("f-autoplay").addEventListener("change", (e) => setBoolPref(AUTOPLAY_KEY, e.target.checked));
  document.getElementById("f-speak-blocked").addEventListener("change", (e) => setBoolPref(SPEAK_BLOCKED_KEY, e.target.checked));

  document.getElementById("save-settings").addEventListener("click", async () => {
    const newSettings = {
      collieBaseUrl: document.getElementById("f-collie-url").value.trim(),
      openrouterApiKey: document.getElementById("f-api-key").value.trim(),
      replyModel: document.getElementById("f-reply-model").value.trim(),
      summarizeModel: document.getElementById("f-summarize-model").value.trim(),
      ttsModel: document.getElementById("f-tts-model").value.trim(),
      ttsVoice: document.getElementById("f-tts-voice").value.trim(),
      ttsFormat: document.getElementById("f-tts-format").value.trim(),
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

function showBlockedOverlay(pane, promptText) {
  state.blockedPane = { paneId: pane.paneId, name: paneDisplayName(pane), since: Date.now() };
  document.getElementById("blocked-pane-name").textContent = state.blockedPane.name;
  document.getElementById("blocked-prompt").textContent = stripAnsi(promptText || "").trim().split("\n").pop() || "(no prompt captured)";
  document.getElementById("blocked-since").textContent = "blocked";
  document.getElementById("blocked-overlay").classList.add("is-open");

  if (getBoolPref(SPEAK_BLOCKED_KEY, true) && state.settings && state.settings.openrouterApiKey) {
    invoke("speak", { text: `${state.blockedPane.name} needs you` })
      .then((audioBase64) => playAudio(audioBase64, state.settings.ttsFormat))
      .catch(() => {});
  }
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
    document.getElementById("command-input").focus();
  });
  document.getElementById("blocked-yes").addEventListener("click", () => quickBlockedReply("yes"));
  document.getElementById("blocked-no").addEventListener("click", () => quickBlockedReply("no"));
}

function quickBlockedReply(text) {
  if (!state.blockedPane) return;
  const paneId = state.blockedPane.paneId;
  hideBlockedOverlay();
  sendCommand(text, paneId);
}

// ---------- sending commands ----------

async function sendCommand(text, paneIdOverride) {
  const paneId = paneIdOverride || state.currentPaneId;
  const turn = {
    id: `t${Date.now()}${Math.random().toString(36).slice(2, 6)}`,
    paneId: paneId || null,
    timestamp: Date.now(),
    inputText: text,
    status: "pending",
  };
  state.transcript.push(turn);
  saveTranscript();
  if (parseHash().view === "conversation") renderConversation();

  try {
    const result = await invoke("send_command", { text, paneId: paneId || null });
    Object.assign(turn, {
      status: "done",
      paneId: result.paneId,
      sentMode: result.sentMode,
      sentContent: result.sentContent,
      preSendContext: result.preSendContext,
      rawOutput: result.rawOutput,
      summary: result.summary,
      audioBase64: result.audioBase64,
      audioFormat: state.settings ? state.settings.ttsFormat : "mp3",
    });
    if (!state.currentPaneId) {
      state.currentPaneId = result.paneId;
      localStorage.setItem(CURRENT_PANE_KEY, result.paneId);
    }
    if (getBoolPref(AUTOPLAY_KEY, true)) playAudio(result.audioBase64, turn.audioFormat);
  } catch (err) {
    Object.assign(turn, { status: "error", error: String(err) });
  }
  saveTranscript();
  if (parseHash().view === "conversation") renderConversation();
  renderChrome(parseHash().view);
}

// ---------- polling ----------

async function pollSnapshot() {
  try {
    const snapshot = await invoke("get_snapshot");
    state.bridgeReachable = true;
    state.snapshot = snapshot;

    for (const pane of allPanes(snapshot)) {
      const prev = state.prevStatus[pane.paneId];
      if (pane.status === "blocked" && prev !== "blocked" && !state.dismissedBlocked.has(pane.paneId)) {
        try {
          const read = await invoke("read_pane", { paneId: pane.paneId, lines: 60 });
          showBlockedOverlay(pane, read.text);
        } catch {
          showBlockedOverlay(pane, "");
        }
      }
      if (pane.status !== "blocked") state.dismissedBlocked.delete(pane.paneId);
      state.prevStatus[pane.paneId] = pane.status;
    }
  } catch {
    state.bridgeReachable = false;
  }
  const view = parseHash().view;
  renderChrome(view);
  if (view === "fleet") renderFleet();
}

function startPolling() {
  pollSnapshot();
  setInterval(() => {
    if (document.visibilityState === "visible") pollSnapshot();
  }, POLL_MS);
}

// ---------- app render dispatch ----------

function renderApp() {
  const { view, param } = parseHash();
  renderChrome(view);
  if (view === "conversation") renderConversation();
  else if (view === "fleet") renderFleet();
  else if (view === "settings") renderSettings();
  else if (view === "turn") renderTurnDetail(param);
  else setHash("conversation");
}

// ---------- wiring ----------

function wireGlobalControls() {
  document.getElementById("settings-btn").addEventListener("click", () => setHash("settings"));
  document.getElementById("back-btn").addEventListener("click", () => {
    if (parseHash().view === "turn") setHash("conversation");
    else setHash("conversation");
  });
  document.getElementById("talking-to-bar").addEventListener("click", () => setHash("fleet"));

  const input = document.getElementById("command-input");
  const submit = () => {
    const text = input.value.trim();
    if (!text) return;
    input.value = "";
    sendCommand(text);
  };
  document.getElementById("talk-btn").addEventListener("click", submit);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submit();
  });
}

async function init() {
  wireGlobalControls();
  wireBlockedOverlay();
  try {
    state.settings = await invoke("get_settings");
  } catch {
    state.settings = null;
  }
  renderApp();
  startPolling();
}

init();
