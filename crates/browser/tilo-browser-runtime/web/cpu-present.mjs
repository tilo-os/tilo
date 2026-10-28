// Presenting frames the runtime rasterized on the CPU, for a page whose
// browser offers no WebGPU (Firefox on Linux ships `dom.webgpu.enabled` off,
// so `navigator.gpu` is absent and vello's compute pipeline cannot run).
//
// The runtime replays the same scene through `tilo-cpu-raster` into a
// retained canvas of premultiplied RGBA8 and hands each changed region to
// `present`: uploaded to a WebGL2 texture and drawn as one textured quad per
// frame, or, where the browser has no WebGL2, written with `putImageData`.
// Both keep the frame on the canvas between presents, which the page's
// capture path (`panorama.mjs`) reads back through `drawImage`.
//
// `?cpuPresent=2d` picks the 2D path anyway, and `?cpuPresent=webgl2` the
// WebGL2 one, so the two can be measured against each other.

export const CPU_PRESENT_OVERRIDE = 'cpuPresent';

const VERTEX = `#version 300 es
in vec2 corner;
out vec2 uv;
void main() {
  uv = vec2(corner.x * 0.5 + 0.5, 0.5 - corner.y * 0.5);
  gl_Position = vec4(corner, 0.0, 1.0);
}`;

// The texture holds the frame whole, so the quad reads it as it is.
const FRAGMENT = `#version 300 es
precision mediump float;
uniform sampler2D frame;
in vec2 uv;
out vec4 colour;
void main() { colour = texture(frame, uv); }`;

// Premultiplied RGBA8 to straight alpha, in place: what `ImageData` holds,
// and what the CPU rasterizer does not write.
export function unpremultiply(rgba) {
  for (let at = 0; at < rgba.length; at += 4) {
    const alpha = rgba[at + 3];
    if (alpha === 0 || alpha === 255) continue;
    for (let channel = at; channel < at + 3; channel++) {
      rgba[channel] = Math.min(255, Math.round((rgba[channel] * 255) / alpha));
    }
  }
  return rgba;
}

/**
 * The presenter for one mount's `canvas`: `resize(width, height)` when the
 * viewport changes, `present(x, y, width, height, pixels)` for each region
 * the runtime rasterized (premultiplied RGBA8, rows of `width * 4` bytes),
 * and `flush()` once the frame's regions are all in. Never throws for a
 * canvas that cannot be presented: a context that cannot be made is the
 * next one's.
 */
export function createCpuPresenter(canvas, {search = globalThis.location?.search ?? '', ...rest} = {}) {
  const override = new URLSearchParams(search).get(CPU_PRESENT_OVERRIDE);
  if (override !== '2d') {
    const webgl = createWebGl2Presenter(canvas, rest);
    if (webgl) return webgl;
  }
  return create2dPresenter(canvas, rest);
}

/** Uploading each region to a texture and drawing it as one quad a frame. */
function createWebGl2Presenter(canvas, {getContext = 'getContext'} = {}) {
  const gl = canvas[getContext]?.('webgl2', {
    alpha: true,
    premultipliedAlpha: true,
    antialias: false,
    depth: false,
    stencil: false,
    // The page reads the presented frame back (`panorama.mjs`), and a
    // browser that clears the drawing buffer after compositing would hand it
    // nothing.
    preserveDrawingBuffer: true,
  });
  if (!gl) return undefined;
  const program = link(gl);
  if (!program) return undefined;
  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
  const corner = gl.getAttribLocation(program, 'corner');
  gl.enableVertexAttribArray(corner);
  gl.vertexAttribPointer(corner, 2, gl.FLOAT, false, 0, 0);
  gl.useProgram(program);
  gl.uniform1i(gl.getUniformLocation(program, 'frame'), 0);
  // Every pixel is the frame's: the quad replaces what the canvas showed.
  gl.disable(gl.BLEND);
  gl.disable(gl.DEPTH_TEST);
  let texture;
  let uploaded = false;
  const allocate = () => {
    if (texture && gl.isTexture(texture)) gl.deleteTexture(texture);
    texture = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, canvas.width, canvas.height, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    uploaded = false;
  };
  return {
    kind: 'webgl2',
    resize(width, height) {
      canvas.width = width;
      canvas.height = height;
      gl.viewport(0, 0, width, height);
      allocate();
    },
    present(x, y, width, height, pixels) {
      if (!texture) allocate();
      // The rows are premultiplied already, and a texture uploaded from one
      // keeps them as they are: the canvas, made with `premultipliedAlpha`,
      // composites that as it is.
      gl.bindTexture(gl.TEXTURE_2D, texture);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, x, y, width, height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
      uploaded = true;
    },
    flush() {
      if (!uploaded) return;
      uploaded = false;
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    },
  };
}

/** Writing each region straight onto the canvas with `putImageData`. */
function create2dPresenter(canvas, {getContext = 'getContext', ImageDataClass = globalThis.ImageData} = {}) {
  const context = canvas[getContext]?.('2d', {alpha: true, desynchronized: true});
  if (!context) throw Error('this browser has no canvas the desktop can be drawn on');
  return {
    kind: '2d',
    resize(width, height) {
      canvas.width = width;
      canvas.height = height;
    },
    present(x, y, width, height, pixels) {
      // `putImageData` takes straight alpha, and replacing the pixels is
      // what a damage region means.
      context.putImageData(new ImageDataClass(unpremultiply(pixels), width, height), x, y);
    },
    flush() {},
  };
}

function link(gl) {
  const build = (type, source) => {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader;
    console.error(`CPU presenter shader: ${gl.getShaderInfoLog(shader)}`);
    return undefined;
  };
  const vertex = build(gl.VERTEX_SHADER, VERTEX);
  const fragment = build(gl.FRAGMENT_SHADER, FRAGMENT);
  if (!vertex || !fragment) return undefined;
  const program = gl.createProgram();
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    console.error(`CPU presenter program: ${gl.getProgramInfoLog(program)}`);
    return undefined;
  }
  return program;
}
