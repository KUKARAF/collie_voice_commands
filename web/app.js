const invoke = window.__TAURI__.core.invoke;

const settingsFields = [
  "collie_base_url",
  "openrouter_api_key",
  "reply_model",
  "summarize_model",
  "tts_model",
  "tts_voice",
  "tts_format",
];

const statusEl = document.getElementById("status");
const playerEl = document.getElementById("player");

function setStatus(text, isError) {
  statusEl.textContent = text;
  statusEl.classList.toggle("error", Boolean(isError));
}

async function loadSettings() {
  const settings = await invoke("get_settings");
  for (const field of settingsFields) {
    document.getElementById(field).value = settings[field] ?? "";
  }
}

async function saveSettings() {
  const settings = {};
  for (const field of settingsFields) {
    settings[field] = document.getElementById(field).value;
  }
  await invoke("save_settings", { newSettings: settings });
  setStatus("Settings saved.");
}

async function sendCommand() {
  const text = document.getElementById("command").value.trim();
  if (!text) return;
  const sendButton = document.getElementById("send");
  sendButton.disabled = true;
  playerEl.style.display = "none";
  setStatus("Sending…");
  try {
    const result = await invoke("send_command", { text });
    setStatus(result.summary);
    const format = document.getElementById("tts_format").value || "mp3";
    playerEl.src = `data:audio/${format};base64,${result.audio_base64}`;
    playerEl.style.display = "block";
    await playerEl.play();
  } catch (err) {
    setStatus(String(err), true);
  } finally {
    sendButton.disabled = false;
  }
}

document.getElementById("save-settings").addEventListener("click", () => {
  saveSettings().catch((err) => setStatus(String(err), true));
});
document.getElementById("send").addEventListener("click", () => {
  sendCommand().catch((err) => setStatus(String(err), true));
});

loadSettings().catch((err) => setStatus(String(err), true));
