/**
 * This file is based on noble-curves (https://github.com/paulmillr/noble-curves).
 *
 * noble-curves - MIT License (c) 2022 Paul Miller (paulmillr.com)
 *
 * The original file is located at:
 * https://github.com/paulmillr/noble-curves/blob/b9d49d2b41d550571a0c5be443ecb62109fa3373/src/utils.ts
 */
/**
 * Hash utilities and type definitions extracted from noble.ts
 * @module
 */
import { anumber } from "../utils/noble.js";
/** Asserts something is hash */
export function ahash(h) {
    if (typeof h !== "function" || typeof h.create !== "function") {
        throw new Error("Hash must wrapped by utils.createHasher");
    }
    anumber(h.outputLen);
    anumber(h.blockLen);
}
export function createHasher(hashCons, info = {}) {
    const hashFn = (msg, opts) => hashCons(opts).update(msg).digest();
    const tmp = hashCons(undefined);
    const hashC = Object.assign(hashFn, {
        outputLen: tmp.outputLen,
        blockLen: tmp.blockLen,
        create: (opts) => hashCons(opts),
        ...info,
    });
    return Object.freeze(hashC);
}
