import { NotSupportedError } from "../../../common/mod.js";
export function emitNotSupported() {
    return new Promise((_resolve, reject) => {
        reject(new NotSupportedError("Not supported"));
    });
}
