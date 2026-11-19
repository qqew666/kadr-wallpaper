import { enhanceVideo, upscaledSize } from "./scaling.js";

export function mountPlayer(host, options, callbacks) {
  const video = options.format === "mp4";
  const element = document.createElement(video ? "video" : "canvas");
  const source = new URL(`media/${options.id}`, document.baseURI);
  const abort = new AbortController();
  const canvas = video ? null : element.getContext("2d", { alpha: false });
  const enhancement = video ? enhanceVideo(host, element) : null;
  const loopFrame = video ? keepFrameBetweenLoops(host, element) : null;
  const resizeObserver = video ? null : new ResizeObserver(updateGifSize);
  let disposed = false;
  let ready = false;
  let failed = false;
  let playing = false;
  let decoder;
  let decoding = false;
  let staged;
  let timer;
  let animation;
  let deadline;
  let nextIndex = 0;
  let remaining = 80;
  let gifWidth = 0;
  let gifHeight = 0;
  let gifSize;
  let fit;

  element.classList.add("media");
  host.replaceChildren(element);
  setFit(options.fit);
  if (video) {
    element.playsInline = true;
    element.muted = true;
    element.preload = "auto";
    element.addEventListener("loadeddata", () => markReady(element.videoWidth, element.videoHeight), { once: true });
    element.addEventListener("ended", startVideo);
    element.addEventListener("error", reportError);
    element.src = source.href;
  } else {
    if (canvas) canvas.imageSmoothingQuality = "high";
    resizeObserver.observe(host);
    void openGif();
  }

  function markReady(width, height) {
    if (disposed || failed || ready) return;
    ready = true;
    element.classList.add("is-ready");
    callbacks.ready({
      width,
      height,
      duration: video && Number.isFinite(element.duration) ? element.duration : null
    });
    updatePlayback();
  }

  function reportError() {
    if (disposed || failed) return;
    failed = true;
    stopGif();
    enhancement?.dispose();
    loopFrame?.dispose();
    if (video) element.pause();
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
      void decoder.completed.catch(reportError);
      await decoder.tracks.ready;
      if (disposed) return;
      const frame = (await decoder.decode({ frameIndex: 0 })).image;
      try {
        if (disposed || failed) return;
        remaining = paintGif(frame);
        nextIndex = 1;
      } finally {
        frame.close();
      }
      markReady(gifWidth, gifHeight);
    } catch {
      reportError();
    }
  }

  function updateGifSize() {
    if (!video) gifSize = upscaledSize(host, gifWidth, gifHeight, fit, 1_000_000);
  }

  function paintGif(frame) {
    if (gifWidth !== frame.displayWidth || gifHeight !== frame.displayHeight) {
      gifWidth = frame.displayWidth;
      gifHeight = frame.displayHeight;
      updateGifSize();
    }
    const width = gifSize?.width ?? gifWidth;
    const height = gifSize?.height ?? gifHeight;
    if (element.width !== width || element.height !== height) {
      element.width = width;
      element.height = height;
      canvas.imageSmoothingQuality = "high";
    }
    canvas.drawImage(frame, 0, 0, width, height);
    return Math.max(16, (frame.duration ?? 80_000) / 1000);
  }

  async function prepareGif() {
    if (!playing || disposed || failed || decoding || staged) return;
    decoding = true;
    try {
      const track = decoder.tracks.selectedTrack;
      if (nextIndex >= track.frameCount && !decoder.complete) await decoder.completed;
      if (!playing || disposed || failed || track.frameCount <= 1) return;

      const index = nextIndex >= track.frameCount ? 0 : nextIndex;
      const frame = (await decoder.decode({ frameIndex: index })).image;
      if (!playing || disposed || failed) {
        frame.close();
        return;
      }
      staged = { frame, index };
      scheduleGif();
    } catch {
      reportError();
    } finally {
      decoding = false;
    }
  }

  function scheduleGif() {
    // Wake shortly before the deadline so a decoded frame can land on a display refresh.
    timer = setTimeout(() => {
      timer = undefined;
      if (document.hidden) {
        presentGif(performance.now());
      } else {
        animation = requestAnimationFrame(onRefresh);
        // Desktop webviews can be occluded by Explorer, which may delay animation callbacks.
        timer = setTimeout(() => presentGif(performance.now()), Math.max(30, deadline - performance.now() + 32));
      }
    }, Math.max(0, deadline - performance.now() - (document.hidden ? 0 : 17)));
  }

  function onRefresh(now) {
    if (now + 1 < deadline) {
      animation = requestAnimationFrame(onRefresh);
    } else {
      presentGif(now);
    }
  }

  function cancelPresentation() {
    clearTimeout(timer);
    if (animation !== undefined) cancelAnimationFrame(animation);
    timer = undefined;
    animation = undefined;
  }

  function presentGif(now) {
    cancelPresentation();
    if (!playing || disposed || failed || !staged) return;
    const { frame, index } = staged;
    staged = undefined;
    try {
      const duration = paintGif(frame);
      // Keep the source timing without carrying a long stall into the next frames.
      deadline = (now - deadline > Math.max(80, duration) ? now : deadline) + duration;
      nextIndex = index + 1;
      void prepareGif();
    } catch {
      reportError();
    } finally {
      frame.close();
    }
  }

  function stopGif() {
    if (deadline !== undefined) {
      remaining = Math.max(0, deadline - performance.now());
      deadline = undefined;
    }
    cancelPresentation();
    staged?.frame.close();
    staged = undefined;
  }

  function updatePlayback() {
    if (!ready || disposed || failed) return;
    if (video) {
      if (playing && (element.paused || element.ended)) {
        startVideo();
      } else if (!playing) {
        element.pause();
      }
      enhancement?.setPlaying(playing);
    } else if (playing) {
      if (deadline === undefined) deadline = performance.now() + remaining;
      void prepareGif();
    } else {
      stopGif();
    }
  }

  function startVideo() {
    if (!playing || disposed || failed) return;
    try {
      if (element.ended) loopFrame.rewind();
      element.play().catch(error => {
        if (playing && error.name !== "AbortError") reportError();
      });
    } catch {
      reportError();
    }
  }

  function setFit(value) {
    fit = value;
    element.classList.toggle("contain", fit === "contain");
    element.classList.toggle("fill", fit === "fill");
    enhancement?.setFit(fit);
    loopFrame?.setFit(fit);
    updateGifSize();
  }

  return {
    setFit,
    setPlaying(value, muted = true) {
      if (video && element.muted !== muted) element.muted = muted;
      if (playing === value) return;
      playing = value;
      updatePlayback();
    },
    dispose() {
      disposed = true;
      stopGif();
      abort.abort();
      decoder?.close();
      resizeObserver?.disconnect();
      enhancement?.dispose();
      loopFrame?.dispose();
      if (video) {
        element.pause();
        element.removeAttribute("src");
        element.load();
      }
      element.remove();
    }
  };
}

function keepFrameBetweenLoops(host, video) {
  const frame = document.createElement("canvas");
  frame.hidden = true;
  frame.setAttribute("aria-hidden", "true");
  const context = frame.getContext("2d", { alpha: false });
  let captured = false;
  let frameRequest;
  let presentation;
  let fallback;

  video.addEventListener("timeupdate", saveNearEnd);
  video.addEventListener("seeked", waitForNewFrame);

  function capture() {
    if (!context || video.seeking || video.readyState < video.HAVE_CURRENT_DATA) return;
    const { videoWidth, videoHeight } = video;
    if (!videoWidth || !videoHeight) return;
    // The still only covers the seek, so keep its backing buffer small.
    const scale = Math.min(1, Math.sqrt((1280 * 720) / (videoWidth * videoHeight)));
    const width = Math.max(1, Math.floor(videoWidth * scale));
    const height = Math.max(1, Math.floor(videoHeight * scale));
    if (frame.width !== width || frame.height !== height) {
      captured = false;
      frame.width = width;
      frame.height = height;
    }
    try {
      context.drawImage(video, 0, 0, width, height);
      captured = true;
    } catch {
      // A decoder may temporarily withhold its frame at the end of a clip.
    }
  }

  function saveNearEnd() {
    if (frame.hidden && video.duration - video.currentTime < 0.35) capture();
  }

  function cancelWait() {
    clearTimeout(fallback);
    if (frameRequest !== undefined) video.cancelVideoFrameCallback(frameRequest);
    if (presentation !== undefined) cancelAnimationFrame(presentation);
    frameRequest = undefined;
    presentation = undefined;
    fallback = undefined;
  }

  function revealVideo() {
    cancelWait();
    if (video.seeking || video.readyState < video.HAVE_CURRENT_DATA) {
      waitForNewFrame();
    } else {
      frame.hidden = true;
    }
  }

  function waitForNewFrame() {
    if (frame.hidden) return;
    cancelWait();
    if (video.requestVideoFrameCallback) {
      frameRequest = video.requestVideoFrameCallback(() => {
        frameRequest = undefined;
        presentation = requestAnimationFrame(revealVideo);
        fallback = setTimeout(revealVideo, 50);
      });
    } else {
      fallback = setTimeout(revealVideo, 32);
    }
  }

  return {
    setFit(fit) {
      frame.className = `media video-loop-frame ${fit}`;
    },
    rewind() {
      cancelWait();
      capture();
      if (captured) {
        host.append(frame);
        frame.hidden = false;
      }
      video.currentTime = video.seekable.length ? video.seekable.start(0) : 0;
    },
    dispose() {
      cancelWait();
      video.removeEventListener("timeupdate", saveNearEnd);
      video.removeEventListener("seeked", waitForNewFrame);
      frame.remove();
    }
  };
}
