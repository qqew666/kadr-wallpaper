// Keep the extra rendering work bounded even on high-DPI displays.
export function upscaledSize(host, width, height, fit, pixelLimit) {
  if (!width || !height || !host.clientWidth || !host.clientHeight) return null;
  const horizontal = host.clientWidth * devicePixelRatio / width;
  const vertical = host.clientHeight * devicePixelRatio / height;
  const displayScale = fit === "contain" ? Math.min(horizontal, vertical) : Math.max(horizontal, vertical);
  const scale = Math.min(displayScale, 2, Math.sqrt(pixelLimit / (width * height)));
  if (scale < 1.12) return null;
  return { width: Math.floor(width * scale), height: Math.floor(height * scale) };
}

const vertexSource = `#version 300 es
out vec2 uv;
void main() {
  vec2 point = vec2((gl_VertexID == 1) ? 3.0 : -1.0, (gl_VertexID == 2) ? 3.0 : -1.0);
  uv = vec2((point.x + 1.0) * 0.5, (1.0 - point.y) * 0.5);
  gl_Position = vec4(point, 0.0, 1.0);
}`;

const fragmentSource = `#version 300 es
precision highp float;
uniform sampler2D sourceFrame;
uniform vec2 texel;
in vec2 uv;
out vec4 color;
void main() {
  vec3 mid = texture(sourceFrame, uv).rgb;
  vec3 left = texture(sourceFrame, uv - vec2(texel.x, 0.0)).rgb;
  vec3 right = texture(sourceFrame, uv + vec2(texel.x, 0.0)).rgb;
  vec3 top = texture(sourceFrame, uv - vec2(0.0, texel.y)).rgb;
  vec3 bottom = texture(sourceFrame, uv + vec2(0.0, texel.y)).rgb;
  vec3 low = min(mid, min(min(left, right), min(top, bottom)));
  vec3 high = max(mid, max(max(left, right), max(top, bottom)));
  vec3 detail = mid - (left + right + top + bottom) * 0.25;
  color = vec4(clamp(mid + detail * 0.42, low, high), 1.0);
}`;

function createRenderer(canvas) {
  const gl = canvas.getContext("webgl2", {
    alpha: false,
    antialias: false,
    depth: false,
    stencil: false,
    powerPreference: "low-power",
    failIfMajorPerformanceCaveat: true
  });
  if (!gl) return null;
  const program = gl.createProgram();
  if (!program) {
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return null;
  }

  for (const [type, source] of [[gl.VERTEX_SHADER, vertexSource], [gl.FRAGMENT_SHADER, fragmentSource]]) {
    const shader = gl.createShader(type);
    if (!shader) break;
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    gl.attachShader(program, shader);
    gl.deleteShader(shader);
  }
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    gl.deleteProgram(program);
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return null;
  }

  const texture = gl.createTexture();
  if (!texture) {
    gl.deleteProgram(program);
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return null;
  }
  gl.useProgram(program);
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  const texel = gl.getUniformLocation(program, "texel");
  let sourceWidth = 0;
  let sourceHeight = 0;

  return {
    draw(video) {
      const { videoWidth, videoHeight } = video;
      const newSize = videoWidth !== sourceWidth || videoHeight !== sourceHeight;
      if (newSize) {
        sourceWidth = videoWidth;
        sourceHeight = videoHeight;
        gl.uniform2f(texel, 0.75 / sourceWidth, 0.75 / sourceHeight);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, video);
      } else {
        gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, video);
      }
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      return !newSize || gl.getError() === gl.NO_ERROR;
    },
    dispose() {
      gl.deleteTexture(texture);
      gl.deleteProgram(program);
      gl.getExtension("WEBGL_lose_context")?.loseContext();
    }
  };
}

export function enhanceVideo(host, video) {
  if (!video.requestVideoFrameCallback) return null;
  const layer = document.createElement("canvas");
  layer.hidden = true;
  layer.setAttribute("aria-hidden", "true");
  let fit;
  let renderer;
  let requestedFrame;
  let playing = false;
  let unavailable = false;
  let frames = 0;
  let renderTime = 0;
  let quality = null;

  const observer = new ResizeObserver(refresh);
  observer.observe(host);
  video.addEventListener("resize", refresh);
  layer.addEventListener("webglcontextlost", disable, { once: true });

  function refresh() {
    if (!playing || unavailable) return;
    const size = upscaledSize(host, video.videoWidth, video.videoHeight, fit, 1920 * 1080);
    if (!size) {
      cancel();
      layer.hidden = true;
      return;
    }
    if (!renderer) {
      try {
        renderer = createRenderer(layer);
      } catch {
        disable();
        return;
      }
      if (!renderer) {
        disable();
        return;
      }
      host.append(layer);
    }
    if (layer.width !== size.width || layer.height !== size.height) {
      layer.hidden = true;
      layer.width = size.width;
      layer.height = size.height;
    }
    if (requestedFrame === undefined) requestedFrame = video.requestVideoFrameCallback(draw);
  }

  function draw() {
    requestedFrame = undefined;
    if (!playing || unavailable) return;
    try {
      const started = performance.now();
      if (!renderer.draw(video)) {
        disable();
        return;
      }
      renderTime += performance.now() - started;
      layer.hidden = false;
      frames += 1;
      if (frames === 36) {
        const current = video.getVideoPlaybackQuality?.();
        const total = quality && current ? current.totalVideoFrames - quality.totalVideoFrames : 0;
        const dropped = quality && current ? current.droppedVideoFrames - quality.droppedVideoFrames : 0;
        if (renderTime / frames > 4 || (total >= 24 && dropped / total > 0.1)) {
          disable();
          return;
        }
        resetStats();
      }
      requestedFrame = video.requestVideoFrameCallback(draw);
    } catch {
      disable();
    }
  }

  function resetStats() {
    frames = 0;
    renderTime = 0;
    quality = video.getVideoPlaybackQuality?.() ?? null;
  }

  function cancel() {
    if (requestedFrame !== undefined) video.cancelVideoFrameCallback(requestedFrame);
    requestedFrame = undefined;
  }

  function disable() {
    if (unavailable) return;
    unavailable = true;
    cancel();
    observer.disconnect();
    video.removeEventListener("resize", refresh);
    layer.removeEventListener("webglcontextlost", disable);
    layer.remove();
    renderer?.dispose();
    renderer = undefined;
  }

  return {
    setFit(value) {
      fit = value;
      layer.className = `media video-upscale ${value}`;
      refresh();
    },
    setPlaying(value) {
      if (playing === value || unavailable) return;
      playing = value;
      if (value) {
        resetStats();
        refresh();
      } else {
        cancel();
        layer.hidden = true;
      }
    },
    dispose: disable
  };
}
