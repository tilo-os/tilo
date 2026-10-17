#!/usr/bin/env python3
"""Bounded, read-only FAT32 file readback from one reviewed raw ESP extent."""

import hashlib
import struct


class FatError(ValueError):
    pass


def require(ok, reason):
    if not ok:
        raise FatError(reason)


class Fat32:
    def __init__(self, stream, offset, size):
        self.stream = stream
        self.offset = offset
        self.size = size
        require(offset >= 0 and size >= 512 and offset + size <= stream.seek(0, 2),
                "FAT32 partition is outside the raw image")
        boot = self.read(0, 512)
        require(boot[510:512] == b"\x55\xaa", "FAT32 boot sector has no signature")
        sector, spc, reserved, fats = struct.unpack_from("<HBHB", boot, 11)
        fat_sectors = struct.unpack_from("<I", boot, 36)[0]
        total = struct.unpack_from("<I", boot, 32)[0]
        self.root = struct.unpack_from("<I", boot, 44)[0]
        require(sector == 512 and spc in (1, 2, 4, 8, 16, 32, 64, 128)
                and reserved >= 1 and fats in (1, 2) and fat_sectors > 0
                and total > reserved + fats * fat_sectors and total * sector <= size,
                "unsupported or malformed FAT32 geometry")
        flags = struct.unpack_from("<H", boot, 40)[0]
        active_fat = flags & 0x0f if flags & 0x80 else 0
        require(active_fat < fats, "FAT32 active FAT index is outside the allocation tables")
        self.cluster_bytes = sector * spc
        self.fat_offset = (reserved + active_fat * fat_sectors) * sector
        self.data_offset = (reserved + fats * fat_sectors) * sector
        self.max_cluster = 2 + (total * sector - self.data_offset) // self.cluster_bytes
        require(2 <= self.root < self.max_cluster
                and self.max_cluster <= fat_sectors * sector // 4,
                "FAT32 root or allocation table is outside the partition")

    def read(self, offset, size):
        require(0 <= offset <= self.size and 0 <= size <= self.size - offset,
                "FAT32 read leaves reviewed ESP extent")
        self.stream.seek(self.offset + offset)
        value = self.stream.read(size)
        require(len(value) == size, "short FAT32 read")
        return value

    def cluster(self, number):
        require(2 <= number < self.max_cluster, "FAT32 cluster outside data area")
        return self.read(self.data_offset + (number - 2) * self.cluster_bytes,
                         self.cluster_bytes)

    def next(self, number):
        value = struct.unpack("<I", self.read(self.fat_offset + 4 * number, 4))[0] & 0x0fffffff
        if value >= 0x0ffffff8:
            return None
        require(2 <= value < self.max_cluster, "FAT32 chain has a bad cluster")
        return value

    def chain(self, start, limit):
        require(2 <= start < self.max_cluster, "FAT32 chain starts outside data area")
        seen = set()
        current = start
        while current is not None:
            require(current not in seen and len(seen) < limit,
                    "FAT32 chain loops or exceeds its bound")
            seen.add(current)
            yield current
            current = self.next(current)

    @staticmethod
    def short_name(entry):
        base = entry[:8].decode("ascii", "strict").rstrip()
        ext = entry[8:11].decode("ascii", "strict").rstrip()
        return base + ("." + ext if ext else "")

    @staticmethod
    def long_name(entries, short):
        if not entries:
            return None
        count = len(entries)
        require(count <= 20 and entries[0][0] == count | 0x40,
                "malformed FAT32 long-name sequence")
        checksum = 0
        for value in short[:11]:
            checksum = ((checksum & 1) << 7) + (checksum >> 1) + value
            checksum &= 0xff
        chars = []
        for index, entry in enumerate(reversed(entries), 1):
            require(entry[0] & 0x1f == index and entry[11] == 0x0f
                    and entry[13] == checksum,
                    "FAT32 long-name checksum or order differs")
            chars.extend(struct.unpack("<5H", entry[1:11]))
            chars.extend(struct.unpack("<6H", entry[14:26]))
            chars.extend(struct.unpack("<2H", entry[28:32]))
        stop = next((i for i, char in enumerate(chars) if char in (0, 0xffff)), len(chars))
        require(all(char == 0xffff for char in chars[stop + 1:]),
                "FAT32 long-name padding differs")
        name = b"".join(struct.pack("<H", char) for char in chars[:stop]).decode("utf-16-le", "strict")
        require(name and "/" not in name and "\\" not in name,
                "FAT32 long name is unsafe")
        return name

    def directory(self, start):
        pending = []
        for cluster in self.chain(start, min(self.max_cluster, 4096)):
            data = self.cluster(cluster)
            for at in range(0, len(data), 32):
                entry = data[at:at + 32]
                if entry[0] == 0:
                    return
                if entry[0] == 0xe5:
                    pending = []
                    continue
                if entry[11] == 0x0f:
                    pending.append(entry)
                    continue
                if entry[11] & 0x08:
                    pending = []
                    continue
                name = self.long_name(pending, entry) or self.short_name(entry)
                pending = []
                first = (struct.unpack_from("<H", entry, 20)[0] << 16
                         | struct.unpack_from("<H", entry, 26)[0])
                size = struct.unpack_from("<I", entry, 28)[0]
                yield name, bool(entry[11] & 0x10), first, size

    def find(self, path):
        parts = path.split("/")
        require(all(part and part not in (".", "..") for part in parts),
                "unsafe FAT32 path")
        current = self.root
        for index, part in enumerate(parts):
            matches = [row for row in self.directory(current)
                       if row[0].casefold() == part.casefold()]
            require(matches, "FAT32 path is missing")
            require(len(matches) == 1, "FAT32 path is ambiguous")
            _, directory, first, size = matches[0]
            if index != len(parts) - 1:
                require(directory, "FAT32 path component is not a directory")
                current = first
            else:
                require(not directory and 0 < size <= 512 * 1024 * 1024,
                        "FAT32 target UKI size is invalid")
                return first, size

    def file_sha256(self, path):
        first, size = self.find(path)
        remaining = size
        digest = hashlib.sha256()
        expected = (size + self.cluster_bytes - 1) // self.cluster_bytes
        clusters = list(self.chain(first, expected + 1))
        require(len(clusters) == expected, "FAT32 file chain length differs")
        for number in clusters:
            data = self.cluster(number)
            digest.update(data[:remaining])
            remaining -= min(remaining, len(data))
        require(remaining == 0, "FAT32 file read is incomplete")
        return digest.hexdigest()
