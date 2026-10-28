// Presenting frames the runtime rasterized on the CPU, for a page whose
// browser offers no WebGPU (Firefox on Linux ships `dom.webgpu.enabled` off,
// so `navigator.gpu` is absent and vello's compute pipeline cannot run).
//
// The runtime replays the same scene through `tilo-cpu-raster` and hands the
// page one retained target per layer of the frame (`ui::cpu_layers`): the
// desktop's ground and chrome, each window of a strip, the overlays above
// them. Each layer is uploaded to its own WebGL2 texture, and `composite`
// draws one quad per layer where the runtime says it goes — so a frame that
// only panned the strip uploads no pixels and moves the textures it has.
// Where the browser has no WebGL2 the frame is written whole with
// `putImageData` instead (`layered` is false, and the runtime draws one
// target). Both keep the frame on the canvas between presents, which the
// page's capture path (`panorama.mjs`) reads back through `drawImage`.
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

// One layer: the quad covers the layer's place on the canvas, in device
// pixels, and reads its own texture from its top left corner.
const LAYER_VERTEX = `#version 300 es
in vec2 corner;
uniform vec4 place;
uniform vec4 crop;
uniform vec2 canvas;
out vec2 uv;
void main() {
  vec2 pixel = mix(place.xy, place.zw, corner);
  uv = mix(crop.xy, crop.zw, corner);
  gl_Position = vec4(pixel.x / canvas.x * 2.0 - 1.0, 1.0 - pixel.y / canvas.y * 2.0, 0.0, 1.0);
}`;

const LAYER_FRAGMENT = `#version 300 es
precision mediump float;
uniform sampler2D layer;
uniform float alpha;
in vec2 uv;
out vec4 colour;
void main() { colour = texture(layer, uv) * alpha; }`;

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
  const layers = createLayerProgram(gl);
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
  // The texture each layer's target was uploaded to, by the runtime's slot,
  // and the targets the page drew it with.
  const textures = new Map();
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
    // Layers are composited here: the runtime draws each of the frame's
    // layers into a target of its own and this draws them where they go.
    layered: layers !== undefined,
    resize(width, height) {
      canvas.width = width;
      canvas.height = height;
      gl.viewport(0, 0, width, height);
      allocate();
      for (const layer of textures.values()) gl.deleteTexture(layer.texture);
      textures.clear();
    },
    /** Clear the canvas to the frame's background colour (premultiplied). */
    beginFrame(red, green, blue, alpha) {
      gl.clearColor(red, green, blue, alpha);
      gl.clear(gl.COLOR_BUFFER_BIT);
    },
    /** Make room for layer `slot`'s texture, `width` x `height` pixels. */
    layer(slot, width, height) {
      const held = textures.get(slot);
      if (held && held.width === width && held.height === height) return;
      if (held) gl.deleteTexture(held.texture);
      const texture = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, texture);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
      textures.set(slot, {texture, width, height});
    },
    /** Put a region of layer `slot`'s pixels (premultiplied RGBA8). */
    upload(slot, x, y, width, height, pixels) {
      const held = textures.get(slot);
      if (!held) return;
      gl.bindTexture(gl.TEXTURE_2D, held.texture);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, x, y, width, height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
    },
    /**
     * Draw each layer of the frame, in draw order: `slots` names the layer,
     * and `places` holds `x, y, width, height, u0, v0, u1, v1` per layer —
     * where it goes, in device pixels, and which part of its texture that
     * shows, in texture coordinates. `alphas` is each layer's opacity.
     */
    composite(slots, places, alphas) {
      if (!layers) return;
      gl.useProgram(layers.program);
      gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
      gl.enableVertexAttribArray(layers.corner);
      gl.vertexAttribPointer(layers.corner, 2, gl.FLOAT, false, 0, 0);
      gl.uniform2f(layers.canvas, canvas.width, canvas.height);
      gl.uniform1i(layers.layer, 0);
      // Source over: the layers were rasterized premultiplied.
      gl.enable(gl.BLEND);
      gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
      gl.enable(gl.SCISSOR_TEST);
      for (let index = 0; index < slots.length; index++) {
        const held = textures.get(slots[index]);
        if (!held) continue;
        const at = index * 8;
        const x = places[at];
        const y = places[at + 1];
        const width = places[at + 2];
        const height = places[at + 3];
        // A layer partly off the canvas is drawn for the part that shows.
        const left = Math.max(0, x);
        const top = Math.max(0, y);
        const right = Math.min(canvas.width, x + width);
        const bottom = Math.min(canvas.height, y + height);
        if (right <= left || bottom <= top) continue;
        gl.scissor(left, canvas.height - bottom, right - left, bottom - top);
        gl.bindTexture(gl.TEXTURE_2D, held.texture);
        gl.uniform4f(layers.place, x, y, x + width, y + height);
        gl.uniform4f(layers.crop, places[at + 4], places[at + 5], places[at + 6], places[at + 7]);
        gl.uniform1f(layers.alpha, alphas?.[index] ?? 1);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      }
      gl.disable(gl.SCISSOR_TEST);
      gl.disable(gl.BLEND);
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

/**
 * The shader that draws one layer where it goes. `undefined` when the page
 * cannot have one, which leaves the runtime drawing a single target.
 */
function createLayerProgram(gl) {
  const program = link(gl, LAYER_VERTEX, LAYER_FRAGMENT);
  if (!program) return undefined;
  return {
    program,
    corner: gl.getAttribLocation(program, 'corner'),
    place: gl.getUniformLocation(program, 'place'),
    crop: gl.getUniformLocation(program, 'crop'),
    canvas: gl.getUniformLocation(program, 'canvas'),
    layer: gl.getUniformLocation(program, 'layer'),
    alpha: gl.getUniformLocation(program, 'alpha'),
  };
}

/** Writing each region straight onto the canvas with `putImageData`. */
function create2dPresenter(canvas, {getContext = 'getContext', ImageDataClass = globalThis.ImageData} = {}) {
  const context = canvas[getContext]?.('2d', {alpha: true, desynchronized: true});
  if (!context) throw Error('this browser has no canvas the desktop can be drawn on');
  return {
    kind: '2d',
    layered: false,
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

function link(gl, vertexSource = VERTEX, fragmentSource = FRAGMENT) {
  const build = (type, source) => {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader;
    console.error(`CPU presenter shader: ${gl.getShaderInfoLog(shader)}`);
    return undefined;
  };
  const vertex = build(gl.VERTEX_SHADER, vertexSource);
  const fragment = build(gl.FRAGMENT_SHADER, fragmentSource);
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
