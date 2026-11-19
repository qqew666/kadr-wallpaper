import { mountPlayer } from "./player.js";

const data = window.kadrWallpaper;
const playback = mountPlayer(document.querySelector("#stage"), data, {
  ready: () => window.ipc.postMessage(JSON.stringify({ kind: "ready" })),
  error: message => window.ipc.postMessage(JSON.stringify({ kind: "error", message }))
});

window.kadrPlayback = playing => playback.setPlaying(playing, data.muted);
