/**
 * Minimal ZIP writer (STORE, no compression) for the mock adapter: "Download
 * folder" has to produce an archive a desktop tool can actually open, and the
 * console ships no zip library. The controller builds real deflated archives in
 * Rust; this file only keeps the demo honest.
 */

export interface ZipEntry {
  /** Path inside the archive. A directory name ends with `/`. */
  name: string;
  bytes: Uint8Array;
  directory?: boolean;
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let index = 0; index < 256; index += 1) {
    let value = index;
    for (let bit = 0; bit < 8; bit += 1) {
      value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
    }
    table[index] = value >>> 0;
  }
  return table;
})();

export function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (let index = 0; index < bytes.length; index += 1) {
    crc = (CRC_TABLE[(crc ^ bytes[index]) & 0xff] ^ (crc >>> 8)) >>> 0;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

/** DOS date and time: 2-second resolution, no time zone. */
function dosStamp(when: Date): { time: number; date: number } {
  const year = Math.max(1980, when.getFullYear());
  return {
    time:
      (when.getHours() << 11) | (when.getMinutes() << 5) | Math.floor(when.getSeconds() / 2),
    date: ((year - 1980) << 9) | ((when.getMonth() + 1) << 5) | when.getDate(),
  };
}

const LOCAL_SIGNATURE = 0x04034b50;
const CENTRAL_SIGNATURE = 0x02014b50;
const END_SIGNATURE = 0x06054b50;
/** Names are UTF-8 (bit 11), never encrypted, STORE. */
const FLAGS = 0x0800;
const METHOD_STORE = 0;

export function zipStore(entries: ZipEntry[], modified = new Date()): Uint8Array<ArrayBuffer> {
  const encoder = new TextEncoder();
  const { time, date } = dosStamp(modified);
  const plan = entries.map((entry) => {
    const directory = entry.directory ?? entry.name.endsWith("/");
    return {
      name: encoder.encode(entry.name),
      bytes: entry.bytes,
      crc: directory ? 0 : crc32(entry.bytes),
      directory,
    };
  });

  const localSize = plan.reduce(
    (total, entry) => total + 30 + entry.name.length + entry.bytes.length,
    0,
  );
  const centralSize = plan.reduce((total, entry) => total + 46 + entry.name.length, 0);
  const buffer = new Uint8Array(localSize + centralSize + 22);
  const view = new DataView(buffer.buffer);
  let offset = 0;

  const u16 = (value: number) => {
    view.setUint16(offset, value, true);
    offset += 2;
  };
  const u32 = (value: number) => {
    view.setUint32(offset, value >>> 0, true);
    offset += 4;
  };
  const raw = (bytes: Uint8Array) => {
    buffer.set(bytes, offset);
    offset += bytes.length;
  };

  const localOffsets: number[] = [];
  for (const entry of plan) {
    localOffsets.push(offset);
    u32(LOCAL_SIGNATURE);
    u16(20); // version needed
    u16(FLAGS);
    u16(METHOD_STORE);
    u16(time);
    u16(date);
    u32(entry.crc);
    u32(entry.bytes.length);
    u32(entry.bytes.length);
    u16(entry.name.length);
    u16(0); // extra field
    raw(entry.name);
    raw(entry.bytes);
  }

  const centralStart = offset;
  plan.forEach((entry, index) => {
    u32(CENTRAL_SIGNATURE);
    u16((3 << 8) | 20); // made by: unix, version 2.0
    u16(20); // version needed
    u16(FLAGS);
    u16(METHOD_STORE);
    u16(time);
    u16(date);
    u32(entry.crc);
    u32(entry.bytes.length);
    u32(entry.bytes.length);
    u16(entry.name.length);
    u16(0); // extra field
    u16(0); // comment
    u16(0); // disk number
    u16(0); // internal attributes
    // External attributes: unix mode in the high 16 bits, so `unzip` restores it.
    u32((entry.directory ? 0o040755 : 0o100644) * 0x10000);
    u32(localOffsets[index]);
    raw(entry.name);
  });

  const centralLength = offset - centralStart;
  u32(END_SIGNATURE);
  u16(0); // this disk
  u16(0); // disk holding the central directory
  u16(plan.length);
  u16(plan.length);
  u32(centralLength);
  u32(centralStart);
  u16(0); // comment

  return buffer;
}
