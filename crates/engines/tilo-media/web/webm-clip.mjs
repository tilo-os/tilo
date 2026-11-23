// Page recording for Camera: the browser's WebCodecs VP8 encoder.
//
// Rust hands over the RGBA frames Camera captures (the pixel format the
// capture backend produces) and muxes the chunks this class emits into a WebM
// file (apps/camera/src/webm.rs). Frames are timestamped off `performance.now`
// when they arrive, so the clip's timing is what the person recorded, and one
// frame per second is asked for as a key frame: WebM clusters start on those,
// which keeps seeking in the recorded clip working.

/** Whether this browser can encode video at all. */
export function videoEncoderAvailable() {
  return typeof globalThis.VideoEncoder === 'function';
}

/** A recording encoder for a clip `width`×`height` at `fps`; throws when the
 * browser cannot make one. */
export function createClip(width, height, fps) {
  return new WebmClip(width, height, fps);
}

export class WebmClip {
  constructor(width, height, fps) {
    if (typeof globalThis.VideoEncoder !== 'function') {
      throw new Error('this browser cannot encode video (WebCodecs is unavailable)');
    }
    this.width = width;
    this.height = height;
    this.fps = Math.max(1, Math.round(fps));
    this.chunks = null;
    this.frames = 0;
    this.started = -1;
    this.lastTimestamp = -1;
    this.error = null;
    // A visible clip at a sane size: 0.1 bits per pixel per frame, kept
    // between one and eight megabits per second.
    const bitrate = Math.min(8_000_000, Math.max(1_000_000,
      Math.round(width * height * this.fps * 0.1)));
    this.encoder = new VideoEncoder({
      output: chunk => this.emit(chunk),
      error: error => { this.error = error?.message || String(error); },
    });
    this.encoder.configure({codec: 'vp8', width, height, framerate: this.fps, bitrate});
  }

  /** Rust's handler for one encoded chunk: `(timestampUs, key, bytes)`. */
  onChunk(handler) { this.chunks = handler; }

  /** Hand one RGBA frame, tightly packed, to the encoder. Returns an error
   * message when it refused, else nothing. */
  push(rgba) {
    if (this.error) return this.error;
    try {
      const now = performance.now();
      if (this.started < 0) this.started = now;
      let timestamp = Math.round((now - this.started) * 1000);
      if (timestamp <= this.lastTimestamp) timestamp = this.lastTimestamp + 1000;
      this.lastTimestamp = timestamp;
      // The constructor copies the bytes: the view stands over the page's
      // memory, which the frame must not keep reading after this returns.
      const frame = new VideoFrame(new Uint8Array(rgba), {
        format: 'RGBA',
        codedWidth: this.width,
        codedHeight: this.height,
        timestamp,
      });
      try {
        this.encoder.encode(frame, {keyFrame: this.frames % this.fps === 0});
      } finally {
        frame.close();
      }
      this.frames += 1;
    } catch (error) {
      this.error = error?.message || String(error);
      return this.error;
    }
    return undefined;
  }

  emit(chunk) {
    if (this.error) return;
    try {
      const data = new Uint8Array(chunk.byteLength);
      chunk.copyTo(data);
      this.chunks?.(chunk.timestamp, chunk.type === 'key', data);
    } catch (error) {
      this.error = error?.message || String(error);
    }
  }

  /** Flush the encoder, then report any error (else null) once every chunk
   * has reached `done`. */
  finish(done) {
    const settle = error => {
      try { if (this.encoder.state !== 'closed') this.encoder.close(); } catch {}
      this.chunks = null;
      done(this.error ?? error ?? null);
    };
    try {
      if (this.encoder.state === 'configured') {
        this.encoder.flush().then(() => settle(null), error => settle(error?.message || String(error)));
      } else {
        settle(null);
      }
    } catch (error) {
      settle(error?.message || String(error));
    }
  }

  close() {
    try { if (this.encoder.state !== 'closed') this.encoder.close(); } catch {}
    this.chunks = null;
  }
}
