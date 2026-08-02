import {
  closeSync,
  constants,
  fstatSync,
  openSync,
  readSync,
  realpathSync,
  statSync,
} from "node:fs";
import { resolve } from "node:path";

const STABLE_FIELDS = [
  "dev",
  "ino",
  "mode",
  "nlink",
  "size",
  "mtimeNs",
  "ctimeNs",
];

function sameIdentity(left, right) {
  return STABLE_FIELDS.every((field) => left[field] === right[field]);
}

function checkedSize(stats, label, maximumBytes) {
  if (!stats.isFile()) {
    throw new Error(`${label} must be a regular file`);
  }
  if (
    !Number.isSafeInteger(maximumBytes) ||
    maximumBytes < 0 ||
    stats.size > BigInt(maximumBytes)
  ) {
    throw new Error(`${label} exceeds ${maximumBytes} bytes`);
  }
  return Number(stats.size);
}

function readExact(fd, size, label) {
  const buffer = Buffer.allocUnsafe(size);
  let offset = 0;
  while (offset < size) {
    const bytesRead = readSync(fd, buffer, offset, size - offset, offset);
    if (bytesRead === 0) {
      throw new Error(`${label} changed while being read`);
    }
    offset += bytesRead;
  }
  return buffer;
}

export function readStableFd(fd, label, maximumBytes) {
  if (!Number.isInteger(fd) || fd < 0) {
    throw new Error(`${label} descriptor is invalid`);
  }
  const before = fstatSync(fd, { bigint: true });
  const size = checkedSize(before, label, maximumBytes);
  const first = readExact(fd, size, label);
  const middle = fstatSync(fd, { bigint: true });
  const second = readExact(fd, size, label);
  const after = fstatSync(fd, { bigint: true });
  if (
    !sameIdentity(before, middle) ||
    !sameIdentity(middle, after) ||
    !first.equals(second)
  ) {
    throw new Error(`${label} changed while being read`);
  }
  return { buffer: first, stats: after };
}

export function readStableFile(path, label, maximumBytes) {
  if (
    typeof constants.O_NOFOLLOW !== "number" ||
    typeof constants.O_NONBLOCK !== "number"
  ) {
    throw new Error(`${label} cannot enforce safe open flags`);
  }
  const absolute = resolve(path);
  let fd;
  try {
    fd = openSync(
      absolute,
      constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
    );
  } catch {
    throw new Error(`${label} must be a regular non-symlink file`);
  }
  try {
    const snapshot = readStableFd(fd, label, maximumBytes);
    let canonicalPath;
    let pathStats;
    try {
      canonicalPath = realpathSync(absolute);
      pathStats = statSync(canonicalPath, { bigint: true });
    } catch {
      throw new Error(`${label} changed while being read`);
    }
    if (!sameIdentity(snapshot.stats, pathStats)) {
      throw new Error(`${label} changed while being read`);
    }
    return { ...snapshot, path: canonicalPath };
  } finally {
    closeSync(fd);
  }
}
