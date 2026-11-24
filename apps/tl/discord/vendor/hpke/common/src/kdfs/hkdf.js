import { EMPTY } from "../consts.js";
import { InvalidParamError } from "../errors.js";
import { KdfId } from "../identifiers.js";
import { NativeAlgorithm } from "../algorithm.js";
// b"HPKE-v1"
const HPKE_VERSION = /* @__PURE__ */ new Uint8Array([
    72,
    80,
    75,
    69,
    45,
    118,
    49,
]);
export function toUint8Array(input) {
    return new Uint8Array(toArrayBuffer(input));
}
export function toArrayBuffer(input) {
    if (input instanceof ArrayBuffer) {
        return input;
    }
    if (ArrayBuffer.isView(input)) {
        return new Uint8Array(input.buffer, input.byteOffset, input.byteLength)
            .slice().buffer;
    }
    return new Uint8Array(input).slice().buffer;
}
export class HkdfNative extends NativeAlgorithm {
    constructor() {
        super();
        Object.defineProperty(this, "id", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: KdfId.HkdfSha256
        });
        Object.defineProperty(this, "hashSize", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 0
        });
        Object.defineProperty(this, "_suiteId", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: EMPTY
        });
        Object.defineProperty(this, "algHash", {
            enumerable: true,
            configurable: true,
            writable: true,
            // WebCrypto's HMAC algorithm has no key length: bun rejects a raw key whose
            // length differs from a `length` given in bits.
            value: {
                name: "HMAC",
                hash: "SHA-256",
            }
        });
    }
    init(suiteId) {
        this._suiteId = suiteId;
    }
    buildLabeledIkm(label, ikm) {
        this._checkInit();
        const ret = new Uint8Array(7 + this._suiteId.byteLength + label.byteLength + ikm.byteLength);
        ret.set(HPKE_VERSION, 0);
        ret.set(this._suiteId, 7);
        ret.set(label, 7 + this._suiteId.byteLength);
        ret.set(ikm, 7 + this._suiteId.byteLength + label.byteLength);
        return ret;
    }
    buildLabeledInfo(label, info, len) {
        this._checkInit();
        const ret = new Uint8Array(9 + this._suiteId.byteLength + label.byteLength + info.byteLength);
        ret.set(new Uint8Array([0, len]), 0);
        ret.set(HPKE_VERSION, 2);
        ret.set(this._suiteId, 9);
        ret.set(label, 9 + this._suiteId.byteLength);
        ret.set(info, 9 + this._suiteId.byteLength + label.byteLength);
        return ret;
    }
    async extract(salt, ikm) {
        await this._setup();
        const saltBuf = salt.byteLength === 0
            ? new ArrayBuffer(this.hashSize)
            : toArrayBuffer(salt);
        if (saltBuf.byteLength !== this.hashSize) {
            throw new InvalidParamError("The salt length must be the same as the hashSize");
        }
        const ikmBuf = toArrayBuffer(ikm);
        const key = await this._api.importKey("raw", saltBuf, this.algHash, false, [
            "sign",
        ]);
        return await this._api.sign("HMAC", key, ikmBuf);
    }
    async expand(prk, info, len) {
        await this._setup();
        const prkBuf = toArrayBuffer(prk);
        const key = await this._api.importKey("raw", prkBuf, this.algHash, false, [
            "sign",
        ]);
        const okm = new ArrayBuffer(len);
        const okmBytes = new Uint8Array(okm);
        let prev = EMPTY;
        const mid = toUint8Array(info);
        const tail = new Uint8Array(1);
        if (len > 255 * this.hashSize) {
            throw new Error("Entropy limit reached");
        }
        const tmp = new Uint8Array(this.hashSize + mid.length + 1);
        for (let i = 1, cur = 0; cur < okmBytes.length; i++) {
            tail[0] = i;
            tmp.set(prev, 0);
            tmp.set(mid, prev.length);
            tmp.set(tail, prev.length + mid.length);
            prev = new Uint8Array(await this._api.sign("HMAC", key, tmp.slice(0, prev.length + mid.length + 1)));
            if (okmBytes.length - cur >= prev.length) {
                okmBytes.set(prev, cur);
                cur += prev.length;
            }
            else {
                okmBytes.set(prev.slice(0, okmBytes.length - cur), cur);
                cur += okmBytes.length - cur;
            }
        }
        return okm;
    }
    async extractAndExpand(salt, ikm, info, len) {
        await this._setup();
        const ikmBuf = toArrayBuffer(ikm);
        const baseKey = await this._api.importKey("raw", ikmBuf, "HKDF", false, ["deriveBits"]);
        return await this._api.deriveBits({
            name: "HKDF",
            hash: this.algHash.hash,
            salt: toArrayBuffer(salt),
            info: toArrayBuffer(info),
        }, baseKey, len * 8);
    }
    async labeledExtract(salt, label, ikm) {
        return await this.extract(salt, this.buildLabeledIkm(label, ikm));
    }
    async labeledExpand(prk, label, info, len) {
        return await this.expand(prk, this.buildLabeledInfo(label, info, len), len);
    }
    _checkInit() {
        if (this._suiteId === EMPTY) {
            throw new Error("Not initialized. Call init()");
        }
    }
}
export class HkdfSha256Native extends HkdfNative {
    constructor() {
        super(...arguments);
        /** KdfId.HkdfSha256 (0x0001) */
        Object.defineProperty(this, "id", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: KdfId.HkdfSha256
        });
        /** 32 */
        Object.defineProperty(this, "hashSize", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 32
        });
        /** The parameters for Web Cryptography API */
        Object.defineProperty(this, "algHash", {
            enumerable: true,
            configurable: true,
            writable: true,
            // WebCrypto's HMAC algorithm has no key length: bun rejects a raw key whose
            // length differs from a `length` given in bits.
            value: {
                name: "HMAC",
                hash: "SHA-256",
            }
        });
    }
}
export class HkdfSha384Native extends HkdfNative {
    constructor() {
        super(...arguments);
        /** KdfId.HkdfSha384 (0x0002) */
        Object.defineProperty(this, "id", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: KdfId.HkdfSha384
        });
        /** 48 */
        Object.defineProperty(this, "hashSize", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 48
        });
        /** The parameters for Web Cryptography API */
        Object.defineProperty(this, "algHash", {
            enumerable: true,
            configurable: true,
            writable: true,
            // WebCrypto's HMAC algorithm has no key length: bun rejects a raw key whose
            // length differs from a `length` given in bits.
            value: {
                name: "HMAC",
                hash: "SHA-384",
            }
        });
    }
}
export class HkdfSha512Native extends HkdfNative {
    constructor() {
        super(...arguments);
        /** KdfId.HkdfSha512 (0x0003) */
        Object.defineProperty(this, "id", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: KdfId.HkdfSha512
        });
        /** 64 */
        Object.defineProperty(this, "hashSize", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: 64
        });
        /** The parameters for Web Cryptography API */
        Object.defineProperty(this, "algHash", {
            enumerable: true,
            configurable: true,
            writable: true,
            // WebCrypto's HMAC algorithm has no key length: bun rejects a raw key whose
            // length differs from a `length` given in bits.
            value: {
                name: "HMAC",
                hash: "SHA-512",
            }
        });
    }
}
