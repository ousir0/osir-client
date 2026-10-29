// Portable PE header verification. Reads artifacts only; never executes them.
import { openSync, closeSync, readSync, fstatSync, constants } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export function inspectPe(path, { expectedMachine } = {}) {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size < 128) throw new Error("PE must be a nonempty regular executable");
    function read(offset, size) {
      if (offset < 0 || offset + size > stat.size) throw new Error("PE header points outside the file");
      const buffer = Buffer.alloc(size);
      if (readSync(fd, buffer, 0, size, offset) !== size) throw new Error("PE header is truncated");
      return buffer;
    }
    const dos = read(0, 64);
    if (dos.readUInt16LE(0) !== 0x5a4d) throw new Error("Missing MZ executable header");
    const offset = dos.readUInt32LE(60);
    const coff = read(offset, 24);
    if (coff.readUInt32LE(0) !== 0x4550) throw new Error("Missing PE signature");
    const machine = coff.readUInt16LE(4);
    const architecture = { 0x14c: "x86", 0x8664: "x86_64", 0xaa64: "aarch64" }[machine];
    if (!architecture || (expectedMachine && architecture !== expectedMachine))
      throw new Error("PE architecture does not match the requested target");
    const optionalSize = coff.readUInt16LE(20);
    if (optionalSize < 2) throw new Error("Invalid PE optional header");
    const optional = read(offset + 24, optionalSize);
    const magic = optional.readUInt16LE(0);
    const directories = magic === 0x20b ? 112 : magic === 0x10b ? 96 : null;
    if (!directories || optionalSize < directories + 40) throw new Error("Invalid PE optional header");
    const certificateOffset = optional.readUInt32LE(directories + 32);
    const certificateBytes = optional.readUInt32LE(directories + 36);
    if ((certificateOffset === 0) !== (certificateBytes === 0) ||
      (certificateBytes && (certificateOffset < offset + 24 + optionalSize || certificateOffset + certificateBytes > stat.size)))
      throw new Error("Invalid Authenticode certificate directory");
    return { architecture, machine: "0x" + machine.toString(16), bytes: stat.size,
      authenticodeCertificatePresent: certificateOffset !== 0 || certificateBytes !== 0 };
  } finally { closeSync(fd); }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try { console.log(JSON.stringify(inspectPe(resolve(process.argv[2]), { expectedMachine: process.argv[3] }), null, 2)); }
  catch (error) { console.error("PE verification: " + error.message); process.exitCode = 1; }
}
