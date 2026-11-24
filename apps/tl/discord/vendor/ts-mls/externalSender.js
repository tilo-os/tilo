// RFC 9420 12.1.8.1 makes the external_senders extension a vector of ExternalSender:
// `ExternalSender external_senders<V>;`. Upstream ts-mls stores one sender per extension
// and writes its fields bare; Discord's DAVE group context carries the RFC vector, whose
// only element is the voice gateway. The vendoring rewrites both directions to the RFC
// form; the rest of the library keeps its one-sender-per-extension model.
import { mapDecoders } from "./codec/tlsDecoder.js";
import { contramapBufferEncoders, encode } from "./codec/tlsEncoder.js";
import { decodeVarLenData, determineLength, lengthEncoder, varLenDataEncoder } from "./codec/variableLength.js";
import { decodeCredential, credentialEncoder } from "./credential.js";
const senderEncoder = contramapBufferEncoders([varLenDataEncoder, credentialEncoder], (e) => [e.signaturePublicKey, e.credential]);
// A one-element vector of ExternalSender.
export const externalSenderEncoder = (sender) => {
  const [length, writeSender] = senderEncoder(sender);
  const [header, writeHeader] = lengthEncoder(length);
  return [
    header + length,
    (offset, buffer) => {
      writeHeader(offset, buffer);
      writeSender(offset + header, buffer);
    },
  ];
};
/** @public */
export const encodeExternalSender = encode(externalSenderEncoder);
/** @public */
export const decodeExternalSender = (b, offset) => {
  const { length, lengthFieldSize } = determineLength(b, offset);
  const inner = mapDecoders([decodeVarLenData, decodeCredential], (signaturePublicKey, credential) => ({ signaturePublicKey, credential }))(b, offset + lengthFieldSize);
  if (inner === undefined) return undefined;
  return [inner[0], lengthFieldSize + length];
};
