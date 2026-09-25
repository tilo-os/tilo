/**
 * Draft 21 OBJECT_DATAGRAM (section 11.2.1), local patch.
 *
 * Only the Normal-status payload form with an Object ID of 0 is produced: every datagram
 * is its own single-object group (the track's group and datagram sequence numbers share one
 * namespace), so the ZERO_OBJECT_ID, END_OF_GROUP and DEFAULT_PRIORITY bits are set and no
 * Properties are sent. Decode accepts every valid Type Flags combination; a datagram that
 * is malformed, carries a status, or names a non-zero object is dropped by the caller.
 *
 * @module
 */
import * as Varint from "../varint.ts";
import { type IetfVersion, Version } from "./version.ts";

const PROPERTIES = 0x01;
const END_OF_GROUP = 0x02;
const ZERO_OBJECT_ID = 0x04;
const DEFAULT_PRIORITY = 0x08;
const RESERVED = 0x10;
const STATUS = 0x20;
const KNOWN = PROPERTIES | END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY | STATUS;

/** The single-object datagram type sent here: END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY. */
export const SINGLE_OBJECT_TYPE = END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY;

function leadingOnes(version: IetfVersion): boolean {
	return version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16;
}

function encodeInt(v: number | bigint, version: IetfVersion): Uint8Array {
	return leadingOnes(version) ? Varint.encodeLeadingOnes(v) : Varint.encode(v);
}

function decodeInt(buf: Uint8Array, version: IetfVersion): [bigint, Uint8Array] {
	return leadingOnes(version) ? Varint.decodeLeadingOnes(buf) : Varint.decodeBigInt(buf);
}

/** One decoded object datagram. `payload` is undefined for a status datagram. */
export interface ObjectDatagram {
	trackAlias: bigint;
	groupId: bigint;
	objectId: bigint;
	endOfGroup: boolean;
	payload?: Uint8Array;
}

/** Encode a single-object group as one OBJECT_DATAGRAM. */
export function encodeObjectDatagram(
	trackAlias: bigint,
	groupId: bigint | number,
	payload: Uint8Array,
	version: IetfVersion,
): Uint8Array {
	const type = encodeInt(SINGLE_OBJECT_TYPE, version);
	const alias = encodeInt(trackAlias, version);
	const group = encodeInt(groupId, version);
	const out = new Uint8Array(type.byteLength + alias.byteLength + group.byteLength + payload.byteLength);
	let offset = 0;
	for (const part of [type, alias, group, payload]) {
		out.set(part, offset);
		offset += part.byteLength;
	}
	return out;
}

/** Decode one OBJECT_DATAGRAM, throwing on anything malformed or unsupported. */
export function decodeObjectDatagram(data: Uint8Array, version: IetfVersion): ObjectDatagram {
	let rest = data;
	let value: bigint;
	[value, rest] = decodeInt(rest, version);
	const type = Number(value);
	if (value > 0x7fn || type & RESERVED || type & ~KNOWN) throw new Error("invalid object datagram type");
	if (type & STATUS && type & END_OF_GROUP) throw new Error("invalid object datagram type");
	const [trackAlias, afterAlias] = decodeInt(rest, version);
	const [groupId, afterGroup] = decodeInt(afterAlias, version);
	rest = afterGroup;
	let objectId = 0n;
	if (!(type & ZERO_OBJECT_ID)) [objectId, rest] = decodeInt(rest, version);
	if (!(type & DEFAULT_PRIORITY)) {
		if (rest.byteLength < 1) throw new Error("short object datagram");
		rest = rest.subarray(1);
	}
	if (type & PROPERTIES) {
		const [length, afterLength] = decodeInt(rest, version);
		if (length === 0n || length > BigInt(afterLength.byteLength)) throw new Error("invalid object datagram properties");
		rest = afterLength.subarray(Number(length));
	}
	if (type & STATUS) {
		return { trackAlias, groupId, objectId, endOfGroup: false };
	}
	return { trackAlias, groupId, objectId, endOfGroup: (type & END_OF_GROUP) !== 0, payload: rest };
}
