import test from 'node:test';
import assert from 'node:assert/strict';
import {createCpuPresenter, unpremultiply, CPU_PRESENT_OVERRIDE} from './cpu-present.mjs';

// A canvas whose contexts record what is done to them: no browser is needed
// to see where a frame's pixels go.
function fakeCanvas({webgl2 = true} = {}) {
  const calls = [];
  const constants = new Map();
  const constant = name => {
    if (!constants.has(name)) constants.set(name, constants.size + 1);
    return constants.get(name);
  };
  const gl = new Proxy({calls}, {
      get: (target, name) => {
        if (name === 'calls') return calls;
        if (typeof name !== 'string') return undefined;
        if (/^[A-Z0-9_]+$/.test(name)) return constant(name);
        return (...args) => {
          calls.push([name, ...args]);
          switch (name) {
            case 'createShader': case 'createProgram': case 'createBuffer': return {name};
            case 'createTexture': return {texture: calls.length};
            case 'getShaderParameter': case 'getProgramParameter': return true;
            case 'getAttribLocation': return 0;
            case 'getUniformLocation': return {uniform: true};
            default: return undefined;
          }
        };
      },
  });
  const context2d = {putImageData: (image, x, y) => calls.push(['putImageData', image, x, y])};
  const canvas = {
    width: 0,
    height: 0,
    calls,
    gl,
    context2d,
    getContext(kind) {
      calls.push(['getContext', kind]);
      if (kind === 'webgl2') return webgl2 ? gl : null;
      if (kind === '2d') return context2d;
      return null;
    },
  };
  return canvas;
}

// Premultiplied RGBA8 for a `width` x `height` region, one colour.
function region(width, height, [r, g, b, a]) {
  const pixels = new Uint8ClampedArray(width * height * 4);
  for (let at = 0; at < pixels.length; at += 4) pixels.set([r, g, b, a], at);
  return pixels;
}

const call = (canvas, name) => canvas.calls.filter(([called]) => called === name);

// Pixels written with `putImageData`, so a test can read them back.
class RecordingImageData {
  constructor(data, width, height) {
    this.data = data;
    this.width = width;
    this.height = height;
  }
}

test('a frame is uploaded as a texture and drawn once, where the browser has WebGL2', () => {
  const canvas = fakeCanvas();
  const presenter = createCpuPresenter(canvas, {search: ''});
  assert.equal(presenter.kind, 'webgl2');
  presenter.resize(800, 600);
  assert.equal(canvas.width, 800);
  assert.equal(canvas.height, 600);
  presenter.present(10, 20, 2, 2, region(2, 2, [255, 0, 0, 255]));
  presenter.present(30, 40, 1, 1, region(1, 1, [0, 0, 255, 255]));
  // Nothing is drawn before the frame's regions are all in.
  assert.equal(call(canvas, 'drawArrays').length, 0);
  presenter.flush();
  assert.equal(call(canvas, 'drawArrays').length, 1, 'one quad a frame, however many regions');
  // [name, target, level, x, y, width, height, format, type, pixels]
  const [[, , , x, y, width, height, format, type, pixels]] = call(canvas, 'texSubImage2D');
  assert.deepEqual([x, y, width, height], [10, 20, 2, 2]);
  // The pixels go up as the CPU rasterizer wrote them: premultiplied RGBA8.
  assert.deepEqual([format, type], [canvas.gl.RGBA, canvas.gl.UNSIGNED_BYTE]);
  assert.deepEqual([...pixels], [255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255]);
});

test('a frame of no damage draws nothing, and keeps the pixels the canvas holds', () => {
  const canvas = fakeCanvas();
  const presenter = createCpuPresenter(canvas, {search: ''});
  presenter.resize(64, 64);
  presenter.present(0, 0, 64, 64, region(64, 64, [1, 2, 3, 255]));
  presenter.flush();
  presenter.flush();
  assert.equal(call(canvas, 'drawArrays').length, 1);
});

test('without WebGL2 each region is written straight onto the canvas, in straight alpha', () => {
  const canvas = fakeCanvas({webgl2: false});
  const presenter = createCpuPresenter(canvas, {search: '', ImageDataClass: RecordingImageData});
  assert.equal(presenter.kind, '2d');
  presenter.resize(320, 200);
  assert.deepEqual([canvas.width, canvas.height], [320, 200]);
  // Half-covered red, as the rasterizer writes it: premultiplied.
  presenter.present(4, 8, 2, 2, region(2, 2, [128, 0, 0, 128]));
  const [[, image, x, y]] = call(canvas, 'putImageData');
  assert.deepEqual([x, y], [4, 8]);
  assert.equal(image.width, 2);
  assert.equal(image.height, 2);
  assert.deepEqual([...image.data.slice(0, 4)], [255, 0, 0, 128], 'straight alpha, replaced in place');
});

test('the page can ask for the 2D path with the URL', () => {
  const canvas = fakeCanvas();
  const presenter = createCpuPresenter(canvas, {search: `?${CPU_PRESENT_OVERRIDE}=2d`, ImageDataClass: RecordingImageData});
  assert.equal(presenter.kind, '2d');
  assert.equal(call(canvas, 'getContext')[0][1], '2d');
});

test('premultiplied pixels become straight ones, and opaque ones are left alone', () => {
  const pixels = new Uint8ClampedArray([128, 0, 0, 128, 10, 20, 30, 255, 0, 0, 0, 0]);
  unpremultiply(pixels);
  assert.deepEqual([...pixels], [255, 0, 0, 128, 10, 20, 30, 255, 0, 0, 0, 0]);
});
