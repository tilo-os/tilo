/**
 * Group role handles: an ordered stream of frames within a track, delivered over one QUIC stream.
 *
 * @module
 */
import { type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import { Timestamp } from "./time.ts";

/** Maximum bytes of frames cached in a group before old frames are evicted from the front. */
export const MAX_GROUP_CACHE_BYTES = 32 * 1024 * 1024;

/** Maximum number of frames cached in a group before old frames are evicted from the front. */
export const MAX_GROUP_FRAMES = 1024;

/**
 * A frame buffered in a group: its presentation {@link Timestamp} and payload bytes.
 *
 * The timestamp carries its own scale, so a track can pick its units; the wire layer
 * converts it into the track's negotiated timescale.
 */
export interface Frame {
	/** The frame payload. */
	payload: Uint8Array;
	/**
	 * Presentation timestamp. Required: for a payload with no presentation time of its own
	 * (a JSON catalog, control state) pass {@link Timestamp.now} explicitly.
	 */
	timestamp: Timestamp;
}

type BufferedFrame = { frame: Frame; cachedAt: number };

// Browser and Node timers clamp larger delays inconsistently. Waking at this
// interval is harmless because the absolute cachedAt deadline remains the source
// of truth and the next timer is scheduled for the remaining duration.
const MAX_TIMER_DELAY_MS = 2_147_483_647;

/** Options for a sequence-aware frame read. */
export interface ReadOptions {
	/**
	 * The lowest sequence number the caller wants; defaults to the whole group.
	 *
	 * Frames below it are discarded instead of returned, and frames evicted below it are not
	 * a gap: the read resumes at the next retained frame rather than throwing {@link Lagged}.
	 * An eviction at or above it still throws, because the caller asked for that frame.
	 */
	from?: number;
}

/** Immutable group metadata. */
export interface Info {
	/** Sequence number of this group within its track. */
	sequence: number;
}

/**
 * Thrown by a frame read when the reader fell behind the group's eviction window: frames
 * it had not yet read were dropped to stay under the cache cap, so the stream has a gap.
 */
export class Lagged extends Error {
	constructor() {
		super("lagged: frames were evicted before being read");
		this.name = "Lagged";
	}
}

/** Reactive backing state shared by the group producer and one consumer. */
class GroupState {
	readonly sequence: number;
	frames = new Signal<BufferedFrame[]>([]);
	closed = new Once<Error | null>();
	total = new Signal<number>(0); // The total number of frames in the group thus far
	maxAgeMs?: number;
	expiryTimer?: ReturnType<typeof setTimeout>;
	evictedChanged?: (markGap: boolean) => void;

	// Absolute sequence of the frame at the front of `frames`, advanced by a read and by an
	// eviction alike: the sequence the next read returns.
	start = 0;

	// One past the newest frame the cache cap evicted before it could be read, or 0 when
	// none was. A read that wanted a frame below it has a gap, so it throws Lagged rather
	// than skipping silently; a read that starts above it never asked for the missing
	// frames, so it proceeds.
	evicted = 0;

	cacheBytes = 0;
	framesChanged = new Set<() => void>();
	expired = new Set<() => void>();

	constructor(sequence: number, maxAgeMs?: number) {
		this.sequence = sequence;
		this.maxAgeMs = maxAgeMs;
	}
}

function clearExpiryTimer(state: GroupState): void {
	if (state.expiryTimer === undefined) return;
	clearTimeout(state.expiryTimer);
	state.expiryTimer = undefined;
}

function scheduleExpiry(state: GroupState): void {
	if (state.maxAgeMs === undefined || state.expiryTimer !== undefined) return;
	const first = state.frames.peek()[0];
	if (!first) return;
	const delay = Math.min(
		MAX_TIMER_DELAY_MS,
		Math.max(0, first.cachedAt + state.maxAgeMs - performance.now()) + 1,
	);
	state.expiryTimer = setTimeout(() => {
		state.expiryTimer = undefined;
		pruneExpired(state);
	}, delay);
	const timer = state.expiryTimer as unknown as { unref?: () => void };
	timer.unref?.();
}

function setMaxAgeMs(state: GroupState, maxAgeMs: number | undefined): void {
	state.maxAgeMs = maxAgeMs;
	clearExpiryTimer(state);
	pruneExpired(state);
}

function appendFrame(state: GroupState, frame: Frame, cachedAt = performance.now()) {
	if (state.closed.peek() !== undefined) throw new Error("group is closed");
	pruneExpired(state);

	state.cacheBytes += frame.payload.byteLength;
	state.frames.mutate((frames) => {
		frames.push({ frame, cachedAt });

		while (frames.length > MAX_GROUP_FRAMES || state.cacheBytes > MAX_GROUP_CACHE_BYTES) {
			const evicted = frames.shift();
			if (!evicted) break;
			state.cacheBytes -= evicted.frame.payload.byteLength;
			state.start++;
			state.evicted = state.start;
			state.evictedChanged?.(true);
		}
	});

	state.total.update((total) => total + 1);
	for (const changed of state.framesChanged) changed();
	scheduleExpiry(state);
}

function evictFrames(state: GroupState, count: number, markGap = true): number {
	let dropped = 0;
	state.frames.mutate((frames) => {
		while (dropped < count) {
			const buffered = frames.shift();
			if (!buffered) break;
			state.cacheBytes -= buffered.frame.payload.byteLength;
			state.start++;
			if (markGap) state.evicted = state.start;
			dropped++;
		}
	});
	if (dropped > 0) state.evictedChanged?.(markGap);
	if (state.frames.peek().length === 0) clearExpiryTimer(state);
	else scheduleExpiry(state);
	return dropped;
}

function evictBefore(state: GroupState, sequence: number, markGap = true): number {
	return evictFrames(state, Math.max(0, sequence - state.start), markGap);
}

function pruneExpired(state: GroupState, now = performance.now()): number {
	if (state.maxAgeMs === undefined) return 0;
	const cutoff = now - state.maxAgeMs;
	let expired = 0;
	for (const buffered of state.frames.peek()) {
		if (buffered.cachedAt > cutoff) break;
		expired++;
	}
	// Age expiry is an intentional local deadline rather than an accidental cache
	// overrun. Advance the read cursor without reporting Lagged: a closed expired
	// group reads as finished, while an open group can still deliver fresh frames.
	const dropped = evictFrames(state, expired, false);
	if (dropped > 0) {
		for (const changed of state.expired) changed();
	}
	if (state.frames.peek().length > 0) scheduleExpiry(state);
	return dropped;
}

/**
 * The write side of an ordered stream of frames within a track.
 *
 * @public
 */
export class Producer {
	/** Sequence number of this group within its track. */
	readonly sequence: number;

	#state: GroupState;
	#mirrors?: Set<GroupState>;

	// Whether any mirror reader is attached (see {@link used}). Fetch coalescing watches it to
	// cancel an abandoned download; a group can stay open indefinitely (a catalog or JSON stream),
	// so this is what stops a reader-less fetch instead of the stream ending on its own.
	#used = new Signal<boolean>(false);

	constructor(sequence: number, maxAgeMs?: number) {
		this.#state = new GroupState(sequence, maxAgeMs);
		this.#state.evictedChanged = (markGap) => {
			if (!this.#mirrors) return;
			for (const mirror of this.#mirrors) evictBefore(mirror, this.#state.start, markGap);
		};
		this.sequence = sequence;
	}

	/**
	 * Settles once the group closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/** A read handle for this group. */
	consume(): Consumer {
		return makeConsumer(this.#state);
	}

	/**
	 * Create an independent read handle that receives every frame written here.
	 *
	 * Frames written so far are replayed synchronously; later writes and close are teed
	 * in as they happen.
	 *
	 * @internal Track fan-out and fetch coalescing only. Use {@link consume} instead.
	 */
	mirror(maxAgeMs?: number): Consumer {
		pruneExpired(this.#state);
		const dst = new GroupState(this.sequence, maxAgeMs);
		for (const buffered of this.#state.frames.peek()) appendFrame(dst, buffered.frame, buffered.cachedAt);
		dst.start = this.#state.start;
		dst.evicted = this.#state.evicted;
		// The replay only covers what is still buffered, so the count has to come from the
		// source: it is what names a frame's absolute sequence, and what a publisher reads
		// to resolve the live edge.
		dst.total.set(this.#state.total.peek());

		const closed = this.#state.closed.peek();
		if (closed !== undefined) {
			dst.closed.set(closed);
			return makeConsumer(dst);
		}

		this.#mirrors ??= new Set();
		this.#mirrors.add(dst);
		this.#used.set(true);

		// Track this mirror's close eagerly (not just lazily on the next write) so demand drops the
		// moment the last reader leaves, letting an abandoned fetch cancel even with no more frames.
		const dispose = dst.closed.subscribe((c) => {
			if (c === undefined) return;
			this.#mirrors?.delete(dst);
			this.#used.set((this.#mirrors?.size ?? 0) > 0);
			dispose();
		});

		return makeConsumer(dst);
	}

	/**
	 * Whether any mirror reader is currently attached.
	 *
	 * Pairs with {@link unused}. Fetch coalescing watches it to cancel a download once every reader
	 * has gone: a group can stay open indefinitely (a catalog track, a JSON stream), so it can't
	 * rely on the stream ending on its own.
	 *
	 * @internal Track fan-out and fetch coalescing only.
	 */
	get used(): Getter<boolean> {
		return this.#used;
	}

	/** Number of frame payloads currently retained by this group. */
	get bufferedFrames(): number {
		pruneExpired(this.#state);
		return this.#state.frames.peek().length;
	}

	/**
	 * Drop up to `count` oldest retained frames, and align every mirror to the
	 * resulting source position. Track cache owners use this to enforce hard bounds.
	 *
	 * @internal
	 */
	evictFrames(count: number, markGap = true): number {
		return evictFrames(this.#state, count, markGap);
	}

	/** Apply or remove an absolute retained-frame age bound. @internal */
	setMaxAgeMs(maxAgeMs: number | undefined): void {
		setMaxAgeMs(this.#state, maxAgeMs);
	}

	/** Observe retained-frame growth. Returns a function that removes the observer. @internal */
	onFramesChanged(changed: () => void): () => void {
		this.#state.framesChanged.add(changed);
		return () => this.#state.framesChanged.delete(changed);
	}

	/** Observe age-based frame eviction. Returns a function that removes the observer. @internal */
	onExpired(changed: () => void): () => void {
		this.#state.expired.add(changed);
		return () => this.#state.expired.delete(changed);
	}

	/**
	 * Resolves once no mirror reader remains (or the group closes).
	 *
	 * @internal Track fan-out and fetch coalescing only.
	 */
	async unused(): Promise<void> {
		while (this.#used.peek() && this.#state.closed.peek() === undefined) {
			await Signal.race(this.#used, this.#state.closed);
		}
	}

	/** Writes a frame to the group. */
	writeFrame(frame: Frame) {
		const cachedAt = performance.now();
		appendFrame(this.#state, frame, cachedAt);

		if (this.#mirrors) {
			for (const mirror of this.#mirrors) {
				if (mirror.closed.peek() !== undefined) this.#mirrors.delete(mirror);
				else appendFrame(mirror, frame, cachedAt);
			}
		}
	}

	/** Write a string as a single UTF-8 encoded frame, stamped with {@link Timestamp.now}. */
	writeString(str: string) {
		this.writeFrame({ payload: new TextEncoder().encode(str), timestamp: Timestamp.now() });
	}

	/** Write a value as a single JSON-encoded frame, stamped with {@link Timestamp.now}. */
	writeJson(json: unknown) {
		this.writeString(JSON.stringify(json));
	}

	/** Write a boolean as a single one-byte frame, stamped with {@link Timestamp.now}. */
	writeBool(bool: boolean) {
		this.writeFrame({ payload: new Uint8Array([bool ? 1 : 0]), timestamp: Timestamp.now() });
	}

	/** True once the group has been closed. */
	get isClosed(): boolean {
		return this.#state.closed.peek() !== undefined;
	}

	/** Closes the group, optionally with an error to abort readers. */
	close(abort?: Error) {
		if (this.#state.closed.peek() !== undefined) return;
		this.#state.closed.set(abort ?? null);

		if (this.#mirrors) {
			for (const mirror of this.#mirrors) {
				if (mirror.closed.peek() === undefined) mirror.closed.set(abort ?? null);
			}
			this.#mirrors.clear();
		}
	}
}

let makeConsumer: (state: GroupState) => Consumer;

/**
 * The read side of an ordered stream of frames within a track.
 *
 * Created internally: obtain one from {@link Producer.consume} or a track subscriber's
 * group reads.
 *
 * @public
 */
export class Consumer {
	/** Sequence number of this group within its track. */
	readonly sequence: number;

	#state: GroupState;

	private constructor(state: GroupState) {
		this.#state = state;
		this.sequence = state.sequence;
	}

	/**
	 * Settles once the group closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	static {
		makeConsumer = (state) => new Consumer(state);
	}

	#readBufferedFrame(): { sequence: number; frame: Frame } | undefined {
		pruneExpired(this.#state);
		const frames = this.#state.frames.peek();
		const buffered = frames.shift();
		if (!buffered) return undefined;

		this.#state.cacheBytes -= buffered.frame.payload.byteLength;
		if (frames.length === 0) clearExpiryTimer(this.#state);
		return { sequence: this.#state.start++, frame: buffered.frame };
	}

	/** True once no further frames can be read: the group has closed and every buffered frame is read. */
	get done(): boolean {
		pruneExpired(this.#state);
		return this.#state.frames.peek().length === 0 && this.#state.closed.peek() !== undefined;
	}

	/** True once the group has been closed, regardless of whether buffered frames remain unread. Synchronous complement to the {@link closed} promise. */
	get isClosed(): boolean {
		return this.#state.closed.peek() !== undefined;
	}

	/** True if frames were evicted from the front of this group before being read. */
	get skipped(): boolean {
		return this.#state.evicted > 0;
	}

	/**
	 * How many frames the group has held, including any already read or evicted.
	 *
	 * It is also the next frame's sequence number, so `frameCount - 1` names the newest
	 * frame written so far. A publisher snapshots it to resolve the track's live edge.
	 */
	get frameCount(): number {
		return this.#state.total.peek();
	}

	/** Number of frame payloads currently retained for this consumer. */
	get bufferedFrames(): number {
		pruneExpired(this.#state);
		return this.#state.frames.peek().length;
	}

	/** Drop up to `count` oldest retained frames. Track cache owners use this to enforce hard bounds. @internal */
	evictFrames(count: number, markGap = true): number {
		return evictFrames(this.#state, count, markGap);
	}

	/** Apply or remove an absolute retained-frame age bound. @internal */
	setMaxAgeMs(maxAgeMs: number | undefined): void {
		setMaxAgeMs(this.#state, maxAgeMs);
	}

	/** Observe retained-frame growth. Returns a function that removes the observer. @internal */
	onFramesChanged(changed: () => void): () => void {
		this.#state.framesChanged.add(changed);
		return () => this.#state.framesChanged.delete(changed);
	}

	/**
	 * Reads the next already-buffered frame without blocking.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 *
	 * Returns `undefined` when nothing is buffered right now. That is not by itself
	 * end-of-group: check {@link done} to tell "no frame buffered yet" from "finished".
	 */
	tryReadFrame(): Frame | undefined {
		const read = this.#readBufferedFrame();
		return read?.frame;
	}

	/** Like {@link tryReadFrame} but also reports the frame's sequence number within the group. */
	tryReadFrameSequence(): ({ sequence: number } & Frame) | undefined {
		const read = this.#readBufferedFrame();
		if (!read) return undefined;
		return { sequence: read.sequence, payload: read.frame.payload, timestamp: read.frame.timestamp };
	}

	/** Resolves once {@link readFrame} would not block. */
	async readable(): Promise<void> {
		for (;;) {
			pruneExpired(this.#state);
			if (this.#state.frames.peek().length > 0) return;
			if (this.#state.closed.peek() !== undefined) return;
			await Signal.race(this.#state.frames, this.#state.closed);
		}
	}

	/**
	 * Reads the next frame from the group.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrame(): Promise<Frame | undefined> {
		for (;;) {
			if (this.#state.evicted > 0) throw new Lagged();

			const read = this.#readBufferedFrame();
			if (read) return read.frame;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return;

			await Signal.race(this.#state.frames, this.#state.closed);
		}
	}

	/**
	 * Reads the next frame along with its sequence number within the group.
	 * Treat the returned frame bytes as read-only; they are shared with other consumers.
	 */
	async readFrameSequence(options?: ReadOptions): Promise<({ sequence: number } & Frame) | undefined> {
		const from = options?.from ?? 0;

		for (;;) {
			if (this.#state.evicted > from) throw new Lagged();

			const read = this.#readBufferedFrame();
			if (read) {
				if (read.sequence < from) continue;
				return { sequence: read.sequence, payload: read.frame.payload, timestamp: read.frame.timestamp };
			}

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return;

			await Signal.race(this.#state.frames, this.#state.closed);
		}
	}

	/** Reads the next frame and decodes its payload as a UTF-8 string. */
	async readString(): Promise<string | undefined> {
		const frame = await this.readFrame();
		return frame ? new TextDecoder().decode(frame.payload) : undefined;
	}

	/** Reads the next frame and parses its payload as JSON. */
	async readJson(): Promise<unknown | undefined> {
		const frame = await this.readString();
		return frame ? JSON.parse(frame) : undefined;
	}

	/** Reads the next frame and decodes its payload as a one-byte boolean. */
	async readBool(): Promise<boolean | undefined> {
		const frame = await this.readFrame();
		return frame ? frame.payload[0] === 1 : undefined;
	}

	/** Closes the group, optionally with an error to abort readers. Idempotent. */
	close(abort?: Error) {
		if (this.#state.closed.peek() !== undefined) return; // already closed
		this.#state.closed.set(abort ?? null);
	}
}
