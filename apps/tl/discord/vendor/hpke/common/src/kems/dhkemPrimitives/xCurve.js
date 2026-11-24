import { EMPTY } from "../../consts.js";
import { DeriveKeyPairError, DeserializeError, NotSupportedError, SerializeError, } from "../../errors.js";
import { toArrayBuffer } from "../../kdfs/hkdf.js";
import { KEM_USAGES, LABEL_DKP_PRK, LABEL_SK, } from "../../interfaces/dhkemPrimitives.js";
import { base64UrlToBytes } from "../../utils/misc.js";
import { XCryptoKey } from "../../xCryptoKey.js";
/**
 * Base DhkemPrimitives implementation for Montgomery curves (X25519/X448).
 *
 * Subclasses pass curve-specific parameters (algorithm name, key size, curve)
 * and optionally add extra methods (e.g., raw derive for X25519).
 */
export class XCurveDhkemPrimitives {
    constructor(algName, keySize, curve, hkdf) {
        Object.defineProperty(this, "_algName", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: void 0
        });
        Object.defineProperty(this, "_curve", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: void 0
        });
        Object.defineProperty(this, "_hkdf", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: void 0
        });
        Object.defineProperty(this, "_nPk", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: void 0
        });
        Object.defineProperty(this, "_nSk", {
            enumerable: true,
            configurable: true,
            writable: true,
            value: void 0
        });
        this._algName = algName;
        this._nPk = keySize;
        this._nSk = keySize;
        this._curve = curve;
        this._hkdf = hkdf;
    }
    serializePublicKey(key) {
        try {
            return Promise.resolve(key.key.buffer);
        }
        catch (e) {
            return Promise.reject(new SerializeError(e));
        }
    }
    async deserializePublicKey(key) {
        try {
            return await this._importRawKey(toArrayBuffer(key), true);
        }
        catch (e) {
            throw new DeserializeError(e);
        }
    }
    serializePrivateKey(key) {
        try {
            return Promise.resolve(key.key.buffer);
        }
        catch (e) {
            return Promise.reject(new SerializeError(e));
        }
    }
    async deserializePrivateKey(key) {
        try {
            return await this._importRawKey(toArrayBuffer(key), false);
        }
        catch (e) {
            throw new DeserializeError(e);
        }
    }
    async importKey(format, key, isPublic) {
        try {
            if (format === "raw") {
                return await this._importRawKey(key, isPublic);
            }
            // jwk
            if (key instanceof ArrayBuffer) {
                throw new Error("Invalid jwk key format");
            }
            return await this._importJWK(key, isPublic);
        }
        catch (e) {
            throw new DeserializeError(e);
        }
    }
    async generateKeyPair() {
        try {
            const rawSk = await this._curve.utils.randomSecretKey();
            const sk = new XCryptoKey(this._algName, rawSk, "private", KEM_USAGES);
            const pk = await this.derivePublicKey(sk);
            return { publicKey: pk, privateKey: sk };
        }
        catch (e) {
            throw new NotSupportedError(e);
        }
    }
    async deriveKeyPair(ikm) {
        try {
            const rawIkm = toArrayBuffer(ikm);
            const dkpPrk = await this._hkdf.labeledExtract(EMPTY.buffer, LABEL_DKP_PRK, new Uint8Array(rawIkm));
            const rawSk = await this._hkdf.labeledExpand(dkpPrk, LABEL_SK, EMPTY, this._nSk);
            const sk = new XCryptoKey(this._algName, new Uint8Array(rawSk), "private", KEM_USAGES);
            return {
                privateKey: sk,
                publicKey: await this.derivePublicKey(sk),
            };
        }
        catch (e) {
            throw new DeriveKeyPairError(e);
        }
    }
    derivePublicKey(key) {
        try {
            const pk = this._curve.getPublicKey(key.key);
            return Promise.resolve(new XCryptoKey(this._algName, pk, "public"));
        }
        catch (e) {
            return Promise.reject(new DeserializeError(e));
        }
    }
    dh(sk, pk) {
        try {
            return Promise.resolve(this._curve.getSharedSecret(sk.key, pk.key).buffer);
        }
        catch (e) {
            return Promise.reject(new SerializeError(e));
        }
    }
    _importRawKey(key, isPublic) {
        return new Promise((resolve, reject) => {
            if (isPublic && key.byteLength !== this._nPk) {
                reject(new Error("Invalid length of the key"));
            }
            if (!isPublic && key.byteLength !== this._nSk) {
                reject(new Error("Invalid length of the key"));
            }
            resolve(new XCryptoKey(this._algName, new Uint8Array(key), isPublic ? "public" : "private", isPublic ? [] : KEM_USAGES));
        });
    }
    _importJWK(key, isPublic) {
        return new Promise((resolve, reject) => {
            if (key.kty !== "OKP") {
                reject(new Error(`Invalid kty: ${key.kty}`));
            }
            if (key.crv !== this._algName) {
                reject(new Error(`Invalid crv: ${key.crv}`));
            }
            if (isPublic) {
                if (typeof key.d !== "undefined") {
                    reject(new Error("Invalid key: `d` should not be set"));
                }
                if (typeof key.x !== "string") {
                    reject(new Error("Invalid key: `x` not found"));
                }
                resolve(new XCryptoKey(this._algName, base64UrlToBytes(key.x), "public"));
            }
            else {
                if (typeof key.d !== "string") {
                    reject(new Error("Invalid key: `d` not found"));
                }
                resolve(new XCryptoKey(this._algName, base64UrlToBytes(key.d), "private", KEM_USAGES));
            }
        });
    }
}
