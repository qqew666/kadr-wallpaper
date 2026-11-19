export function mountPlayer(host, options, callbacks) {
  const video = options.format === "mp4";
  const element = document.createElement(video ? "video" : "canvas");
  const source = new URL(`media/${options.id}`, document.baseURI);
  const abort = new AbortController();
  let disposed = false;
  let ready = false;
  let failed = false;
  let playing = false;
  let decoder;
  let timer;
  let decoding = false;
  let index = 0;
  let delay = 80;
  const canvas = video ? null : element.getContext("2d", { alpha: false });

  host.replaceChildren(element);
  setFit(options.fit);
  if (video) {
    element.playsInline = true;
    element.loop = true;
    element.muted = true;
    element.preload = "auto";
    element.addEventListener("loadeddata", markReady, { once: true });
    element.addEventListener("error", reportError);
    element.src = source.href;
  } else {
    void openGif();
  }

  function markReady() {
    if (disposed || failed || ready) return;
    ready = true;
    callbacks.ready({
      width: video ? element.videoWidth : element.width,
      height: video ? element.videoHeight : element.height,
      duration: video && Number.isFinite(element.duration) ? element.duration : null
    });
    updatePlayback();
  }

  function reportError() {
    if (disposed || failed) return;
    failed = true;
    clearTimeout(timer);
    callbacks.error(video
      ? "Не удалось воспроизвести MP4. Проверьте файл и кодек. Рекомендуется H.264."
      : "Не удалось прочитать GIF. Проверьте файл и обновите Microsoft Edge WebView2.");
  }

  async function openGif() {
    try {
      if (!("ImageDecoder" in window) || !canvas) throw new Error("GIF decoding unavailable");
      const response = await fetch(source, { signal: abort.signal });
      if (!response.ok || !response.body || disposed) throw new Error("GIF unavailable");
      decoder = new ImageDecoder({ type: "image/gif", data: response.body });
      const completed = decoder.completed;
      void completed.catch(reportError);
      await decoder.tracks.ready;
      if (disposed) return;
      await drawFrame();
      markReady();
    } catch {
      reportError();
    }
  }

  async function drawFrame() {
    const frame = (await decoder.decode({ frameIndex: index })).image;
    try {
      if (disposed) return;
      if (element.width !== frame.displayWidth || element.height !== frame.displayHeight) {
        element.width = frame.displayWidth;
        element.height = frame.displayHeight;
      }
      canvas.drawImage(frame, 0, 0);
      delay = Math.max(20, (frame.duration ?? 80_000) / 1000);
      index += 1;
    } finally {
      frame.close();
    }
  }

  async function nextFrame() {
    timer = undefined;
    if (!playing || disposed || failed) return;
    decoding = true;
    const started = performance.now();
    try {
      const track = decoder.tracks.selectedTrack;
      if (index >= track.frameCount && !decoder.complete) await decoder.completed;
      if (!playing || disposed || failed) return;
      if (track.frameCount <= 1) return;
      if (index >= track.frameCount) index = 0;
      await drawFrame();
    } catch {
      reportError();
    } finally {
      decoding = false;
    }
    schedule(Math.max(0, delay - (performance.now() - started)));
  }

  function schedule(wait) {
    if (playing && !disposed && !failed && !decoding && timer === undefined) {
      timer = setTimeout(nextFrame, wait);
    }
  }

  function updatePlayback() {
    if (!ready || disposed || failed) return;
    if (video) {
      if (playing) {
        element.play().catch(error => {
          if (playing && error.name !== "AbortError") reportError();
        });
      } else {
        element.pause();
      }
    } else if (playing) {
      schedule(delay);
    } else {
      clearTimeout(timer);
      timer = undefined;
    }
  }

  function setFit(fit) {
    element.className = fit === "contain" || fit === "fill" ? `media ${fit}` : "media";
  }

  return {
    setFit,
    setPlaying(value, muted = true) {
      if (video) element.muted = muted;
      playing = value;
      updatePlayback();
    },
    dispose() {
      disposed = true;
      clearTimeout(timer);
      abort.abort();
      decoder?.close();
      if (video) {
        element.pause();
        element.removeAttribute("src");
        element.load();
      }
      element.remove();
    }
  };
}
