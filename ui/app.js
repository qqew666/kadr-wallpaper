import { mountPlayer } from "./player.js";

const get = id => document.getElementById(id);
const demo = "00000000-0000-0000-0000-000000000001";
const send = (kind, data = {}) => window.ipc?.postMessage(JSON.stringify({ kind, ...data }));
const playButton = get("preview-play");
const applyButton = get("apply");
const formatSize = new Intl.NumberFormat("ru-RU", { maximumFractionDigits: 1 });

let state;
let selected;
let player;
let previewReady = false;
let playPreview = true;
let managerVisible = true;
let selectionError = "";

function notice(message, error = false) {
  const element = get("notice");
  element.hidden = !message;
  element.classList.toggle("error", error);
  element.textContent = message || "";
}

function renderLibrary() {
  const list = get("library");
  const focused = list.contains(document.activeElement) ? document.activeElement.dataset.id : null;
  list.replaceChildren();
  get("count").textContent = state.items.length;
  for (const item of state.items) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "library-item";
    button.dataset.id = item.id;
    button.title = item.name;
    button.setAttribute("aria-pressed", String(item.id === selected?.id));
    const builtIn = item.id === demo;
    const thumbnail = document.createElement(builtIn ? "img" : "span");
    thumbnail.className = "thumbnail";
    if (builtIn) {
      thumbnail.src = "aurora.webp";
      thumbnail.alt = "";
    } else {
      thumbnail.textContent = item.format.toUpperCase();
    }
    const meta = document.createElement("span");
    meta.className = "item-info";
    const title = document.createElement("strong");
    title.textContent = item.name;
    const subtitle = document.createElement("small");
    subtitle.textContent = builtIn ? "Встроенные обои" : item.format.toUpperCase();
    meta.append(title, subtitle);
    button.append(thumbnail, meta);
    if (state.wallpapers_enabled && state.active?.id === item.id) {
      const active = document.createElement("span");
      active.className = "active-indicator";
      active.title = "На рабочем столе";
      button.append(active);
    }
    button.addEventListener("click", () => {
      if (selected?.id !== item.id) select(item);
    });
    list.append(button);
    if (focused === item.id) button.focus();
  }
}

function select(item) {
  selected = item;
  player?.dispose();
  previewReady = false;
  playPreview = true;
  selectionError = "";
  notice("");
  get("title").textContent = item.name;
  const stage = get("stage");
  stage.classList.toggle("demo", item.id === demo);
  const baseDetail = item.id === demo
    ? "Встроенные обои"
    : `${item.format.toUpperCase()} · ${formatSize.format(item.bytes / 1048576)} МБ`;
  get("details").textContent = baseDetail;
  get("audio").disabled = item.format === "gif";
  get("sound-label").textContent = get("audio").disabled ? "Без звука"
    : get("audio").checked ? "Включён" : "Выключен";
  get("remove").hidden = item.id === demo;
  renderLibrary();
  renderControls();
  player = mountPlayer(stage, { ...item, fit: get("fit").value }, {
    ready: meta => {
      previewReady = true;
      updatePreview();
      renderControls();
      get("details").textContent = `${baseDetail} · ${meta.width} × ${meta.height}`;
    },
    error: message => {
      previewReady = false;
      selectionError = message;
      notice(message, true);
      renderControls();
    }
  });
}

function setMonitors(list, fallback) {
  const select = get("monitor");
  const current = state ? select.value : fallback;
  select.replaceChildren(new Option("Все мониторы", "all"));
  select.add(new Option("Основной монитор", "primary"));
  for (const entry of list) select.add(new Option(entry.label, entry.id));
  if (current && !Array.from(select.options).some(option => option.value === current)) {
    const disconnected = new Option("Монитор отключён", current);
    disconnected.disabled = true;
    select.add(disconnected);
  }
  select.value = current;
}

function renderControls() {
  if (!state || !selected) return;
  const applying = state.status === "applying";
  applyButton.disabled = applying || !previewReady || get("monitor").selectedOptions[0]?.disabled === true;
  const applyLabel = applying ? "Проверяем обои…" : "Установить обои";
  applyButton.lastChild.textContent = applyLabel;
  playButton.disabled = !previewReady;
  const previewLabel = playPreview ? "Приостановить предпросмотр" : "Запустить предпросмотр";
  playButton.title = previewLabel;
  playButton.setAttribute("aria-label", previewLabel);
  playButton.firstElementChild.className = playPreview ? "pause-icon" : "play-icon";

  const live = state.status === "running";
  const paused = state.status === "paused";
  get("wallpapers").hidden = !state.active;
  get("wallpapers").textContent = state.wallpapers_enabled ? "Выключить обои" : "Включить обои";
  get("pause").hidden = !live && !paused;
  get("pause").textContent = paused ? "Продолжить" : "Пауза";
  get("remove").disabled = applying;
  get("status-dot").className = live ? "live" : paused ? "paused" : "";
  get("status-description").textContent = applying ? "Подключаем к рабочему столу…"
    : live ? state.items.find(item => item.id === state.active.id)?.name ?? "Воспроизводится"
    : paused ? "Воспроизведение на паузе"
    : state.status === "disabled" ? "Живые обои выключены"
    : state.active ? "Ожидаем рабочий стол или монитор"
    : "Обои не установлены";
}

function updatePreview() {
  player?.setPlaying(playPreview && managerVisible && !document.hidden);
}

window.kadr = {
  render(payload) {
    const first = !state;
    setMonitors(payload.monitors, payload.draft.monitor);
    state = payload;
    managerVisible = state.window_visible;
    get("startup").checked = state.startup_enabled;
    get("startup").disabled = false;
    get("startup-label").textContent = state.startup_enabled ? "Включён" : "Выключен";
    if (first) {
      get("fit").value = state.draft.fit;
      get("audio").checked = !state.draft.muted;
      get("sound-label").textContent = state.draft.muted ? "Выключен" : "Включён";
    }
    const preferred = state.select ?? selected?.id ?? state.active?.id;
    const item = state.items.find(entry => entry.id === preferred) ?? state.items[0];
    if (selected?.id !== item.id) select(item);
    else renderLibrary();
    updatePreview();
    renderControls();
    if (state.notice) notice(state.notice.message, state.notice.error);
    else if (selectionError) notice(selectionError, true);
  },
  drag(visible) {
    get("drop-overlay").hidden = !visible;
  },
  visible(visible) {
    managerVisible = visible;
    updatePreview();
  }
};

get("import").addEventListener("click", () => send("import"));
get("remove").addEventListener("click", () => send("remove", { id: selected.id }));
get("monitor").addEventListener("change", renderControls);
get("fit").addEventListener("change", () => player?.setFit(get("fit").value));
get("audio").addEventListener("change", () => {
  get("sound-label").textContent = get("audio").checked ? "Включён" : "Выключен";
});
playButton.addEventListener("click", () => {
  playPreview = !playPreview;
  updatePreview();
  renderControls();
});
applyButton.addEventListener("click", () => {
  notice("");
  send("apply", {
    id: selected.id,
    settings: {
      monitor: get("monitor").value,
      fit: get("fit").value,
      muted: selected.format === "gif" || !get("audio").checked
    }
  });
});
get("pause").addEventListener("click", () => send("pause"));
get("wallpapers").addEventListener("click", () => {
  notice("");
  send("toggle_wallpapers");
});
get("startup").addEventListener("change", event => {
  notice("");
  event.currentTarget.disabled = true;
  send("set_startup", { enabled: event.currentTarget.checked });
});
document.addEventListener("visibilitychange", updatePreview);
document.addEventListener("keydown", event => {
  if (event.ctrlKey && event.code === "KeyO" && !event.repeat) {
    event.preventDefault();
    send("import");
  }
});
send("ready");
