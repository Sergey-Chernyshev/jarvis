import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdtempSync,
  opendirSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import {
  basename,
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { readStableFile } from "./read-stable-file.mjs";

const MAX_CONFIG_BYTES = 1024 * 1024;
const MAX_ARCHIVE_BYTES = 16 * 1024 * 1024;
const MAX_SOURCE_FILE_BYTES = 32 * 1024 * 1024;
const MAX_SOURCE_ENTRIES = 20_000;
const MAX_SOURCE_BYTES = 256 * 1024 * 1024;
const MAX_SOURCE_DEPTH = 64;
const ARTIFACT_SEGMENT = /^[A-Za-z0-9][A-Za-z0-9._+-]*$/;
const CARGO_OK = Buffer.from('{"v":1}', "utf8");
const OFFICIAL_REGISTRY_INDEXES = new Set([
  "github.com-1ecc6299db9ec823",
  "index.crates.io-1949cf8c6b5b557f",
]);

function isInside(root, candidate) {
  const nested = relative(root, candidate);
  return (
    nested !== ".." && !nested.startsWith(`..${sep}`) && !isAbsolute(nested)
  );
}

function ancestorDirectories(start) {
  const directories = [];
  let directory = realpathSync(resolve(start));
  while (true) {
    directories.push(directory);
    const parent = dirname(directory);
    if (parent === directory) return directories;
    directory = parent;
  }
}

function uncommentedToml(source) {
  const output = [];
  for (const line of source.split(/\r?\n/)) {
    let singleQuoted = false;
    let doubleQuoted = false;
    let escaped = false;
    let end = line.length;
    for (let index = 0; index < line.length; index += 1) {
      const character = line[index];
      if (doubleQuoted && escaped) {
        escaped = false;
        continue;
      }
      if (doubleQuoted && character === "\\") {
        escaped = true;
        continue;
      }
      if (!doubleQuoted && character === "'") {
        singleQuoted = !singleQuoted;
        continue;
      }
      if (!singleQuoted && character === '"') {
        doubleQuoted = !doubleQuoted;
        continue;
      }
      if (!singleQuoted && !doubleQuoted && character === "#") {
        end = index;
        break;
      }
    }
    output.push(line.slice(0, end));
  }
  return output.join("\n");
}

function configHasSourceOverride(source) {
  const content = uncommentedToml(source);
  if (/\\u[0-9a-fA-F]{4}|\\U[0-9a-fA-F]{8}/.test(content)) {
    return true;
  }
  return (
    /(?:^|\n)\s*\[\s*["']?source["']?(?:\s*[.\]])/m.test(content) ||
    /(?:^|\n)\s*\[\s*["']?registries["']?\s*\.\s*["']?crates-io["']?\s*\]/m.test(
      content,
    ) ||
    /(?:^|\n)\s*["']?source["']?\s*(?:[.=])/m.test(content) ||
    /(?:^|\n)\s*["']?paths["']?\s*=/m.test(content) ||
    /(?:^|\n)\s*["']?include["']?\s*=/m.test(content) ||
    /(?:^|\n)\s*["']?registries["']?\s*\.\s*["']?crates-io["']?\s*\.\s*["']?index["']?\s*=/m.test(
      content,
    ) ||
    /["']?(?:replace-with|replace_with|local-registry|local_registry)["']?\s*=/m.test(
      content,
    )
  );
}

export function assertNoCargoSourceOverrides(workspaceRoot) {
  for (const key of Object.keys(process.env)) {
    if (
      /^CARGO_SOURCE_/.test(key) ||
      key === "CARGO_CONFIG" ||
      key === "CARGO_PATHS" ||
      key === "CARGO_REGISTRY_INDEX" ||
      /^CARGO_REGISTRIES_[A-Z0-9_]+_(?:INDEX|REPLACE_WITH|DIRECTORY|LOCAL_REGISTRY)$/.test(
        key,
      )
    ) {
      throw new Error(`Cargo source override environment is forbidden: ${key}`);
    }
  }

  const cargoHome = resolve(
    process.env.CARGO_HOME ?? join(homedir(), ".cargo"),
  );
  const directories = new Set([
    cargoHome,
    ...ancestorDirectories(process.cwd()).map((directory) =>
      join(directory, ".cargo"),
    ),
    ...ancestorDirectories(workspaceRoot).map((directory) =>
      join(directory, ".cargo"),
    ),
  ]);
  const checked = new Set();
  for (const directory of directories) {
    for (const name of ["config", "config.toml"]) {
      const candidate = resolve(directory, name);
      if (checked.has(candidate) || !existsSync(candidate)) continue;
      checked.add(candidate);
      const config = readStableFile(
        candidate,
        "Cargo configuration",
        MAX_CONFIG_BYTES,
      );
      const source = new TextDecoder("utf-8", { fatal: true }).decode(
        config.buffer,
      );
      if (configHasSourceOverride(source)) {
        throw new Error(
          `Cargo source replacement or paths override is forbidden: ${config.path}`,
        );
      }
    }
  }
}

function directoryEntries(path, label) {
  const before = lstatSync(path, { bigint: true });
  if (before.isSymbolicLink() || !before.isDirectory()) {
    throw new Error(`${label} must be a non-symlink directory`);
  }
  const entries = [];
  const directory = opendirSync(path);
  try {
    let entry;
    while ((entry = directory.readSync()) !== null) {
      entries.push(entry);
      if (entries.length > MAX_SOURCE_ENTRIES + 1) {
        throw new Error(`${label} exceeds ${MAX_SOURCE_ENTRIES} entries`);
      }
    }
  } finally {
    directory.closeSync();
  }
  const after = lstatSync(path, { bigint: true });
  for (const field of ["dev", "ino", "mode", "size", "mtimeNs", "ctimeNs"]) {
    if (before[field] !== after[field]) {
      throw new Error(`${label} changed while being read`);
    }
  }
  return entries.sort((left, right) => left.name.localeCompare(right.name));
}

function compareSourceTrees(
  expectedRoot,
  physicalRoot,
  budget,
  relativePath = "",
  depth = 0,
) {
  if (depth > MAX_SOURCE_DEPTH) {
    throw new Error(`audited Cargo source exceeds depth ${MAX_SOURCE_DEPTH}`);
  }
  const expectedDirectory = join(expectedRoot, relativePath);
  const physicalDirectory = join(physicalRoot, relativePath);
  const expectedEntries = directoryEntries(
    expectedDirectory,
    "audited Cargo archive directory",
  );
  const expectedNames = new Set(expectedEntries.map((entry) => entry.name));
  const physicalEntries = directoryEntries(
    physicalDirectory,
    "audited Cargo source directory",
  ).filter(
    (entry) =>
      relativePath !== "" ||
      entry.name !== ".cargo-ok" ||
      expectedNames.has(entry.name),
  );
  if (
    expectedEntries.length !== physicalEntries.length ||
    expectedEntries.some(
      (entry, index) => entry.name !== physicalEntries[index]?.name,
    )
  ) {
    throw new Error("audited Cargo source tree differs from its crate archive");
  }

  for (const [index, expectedEntry] of expectedEntries.entries()) {
    budget.entries += 1;
    if (budget.entries > MAX_SOURCE_ENTRIES) {
      throw new Error(
        `audited Cargo source exceeds ${MAX_SOURCE_ENTRIES} entries`,
      );
    }
    const physicalEntry = physicalEntries[index];
    const child = relativePath
      ? join(relativePath, expectedEntry.name)
      : expectedEntry.name;
    if (
      expectedEntry.isSymbolicLink() ||
      physicalEntry.isSymbolicLink() ||
      expectedEntry.isDirectory() !== physicalEntry.isDirectory() ||
      expectedEntry.isFile() !== physicalEntry.isFile()
    ) {
      throw new Error(
        "audited Cargo source entry type differs from its archive",
      );
    }
    if (expectedEntry.isDirectory()) {
      compareSourceTrees(expectedRoot, physicalRoot, budget, child, depth + 1);
      continue;
    }
    if (!expectedEntry.isFile()) {
      throw new Error("audited Cargo source contains an unsupported entry");
    }
    const expected = readStableFile(
      join(expectedRoot, child),
      "audited Cargo archive source",
      MAX_SOURCE_FILE_BYTES,
    );
    const physical = readStableFile(
      join(physicalRoot, child),
      "audited Cargo physical source",
      MAX_SOURCE_FILE_BYTES,
    );
    budget.bytes += expected.buffer.length;
    if (budget.bytes > MAX_SOURCE_BYTES) {
      throw new Error(`audited Cargo source exceeds ${MAX_SOURCE_BYTES} bytes`);
    }
    if (!expected.buffer.equals(physical.buffer)) {
      throw new Error(
        `audited Cargo source file differs from its archive: ${child}`,
      );
    }
  }
}

function trustedTarBinary() {
  for (const candidate of ["/usr/bin/bsdtar", "/usr/bin/tar", "/bin/tar"]) {
    try {
      const canonical = realpathSync(candidate);
      const info = lstatSync(canonical);
      if (
        info.isFile() &&
        !info.isSymbolicLink() &&
        info.uid === 0 &&
        (info.mode & 0o022) === 0
      ) {
        return canonical;
      }
    } catch {
      // Try the next fixed system path.
    }
  }
  throw new Error("no trusted system tar binary is available");
}

function requireDirectory(path, label) {
  const info = lstatSync(path);
  if (info.isSymbolicLink() || !info.isDirectory()) {
    throw new Error(`${label} must be a non-symlink directory`);
  }
}

export function verifyCargoRegistrySource(packageRecord, checksum) {
  if (
    !packageRecord ||
    typeof packageRecord.name !== "string" ||
    typeof packageRecord.version !== "string" ||
    typeof packageRecord.manifest_path !== "string" ||
    !ARTIFACT_SEGMENT.test(packageRecord.name) ||
    !ARTIFACT_SEGMENT.test(packageRecord.version)
  ) {
    throw new Error("audited Cargo package has malformed physical identity");
  }
  const manifest = readStableFile(
    packageRecord.manifest_path,
    `audited Cargo package ${packageRecord.name} manifest`,
    MAX_SOURCE_FILE_BYTES,
  );
  const physicalRoot = dirname(manifest.path);
  const artifactName = `${packageRecord.name}-${packageRecord.version}`;
  if (
    basename(physicalRoot) !== artifactName ||
    manifest.path !== join(physicalRoot, "Cargo.toml")
  ) {
    throw new Error(
      `audited Cargo package ${packageRecord.name} has a non-registry physical source`,
    );
  }
  const registryIndexRoot = dirname(physicalRoot);
  const registrySourceRoot = dirname(registryIndexRoot);
  const registryRoot = dirname(registrySourceRoot);
  const registryIndex = basename(registryIndexRoot);
  if (
    basename(registrySourceRoot) !== "src" ||
    basename(registryRoot) !== "registry" ||
    !OFFICIAL_REGISTRY_INDEXES.has(registryIndex)
  ) {
    throw new Error(
      `audited Cargo package ${packageRecord.name} has a non-registry physical source`,
    );
  }
  requireDirectory(registryRoot, "Cargo registry root");
  requireDirectory(registrySourceRoot, "Cargo registry source root");
  requireDirectory(registryIndexRoot, "Cargo registry index root");
  requireDirectory(physicalRoot, "audited Cargo package source root");
  const cargoOk = readStableFile(
    join(physicalRoot, ".cargo-ok"),
    `audited Cargo package ${packageRecord.name} extraction marker`,
    CARGO_OK.length,
  );
  if (!cargoOk.buffer.equals(CARGO_OK)) {
    throw new Error(
      `audited Cargo package ${packageRecord.name} has an invalid extraction marker`,
    );
  }
  const archivePath = join(
    registryRoot,
    "cache",
    registryIndex,
    `${artifactName}.crate`,
  );
  const archive = readStableFile(
    archivePath,
    `audited Cargo package ${packageRecord.name} archive`,
    MAX_ARCHIVE_BYTES,
  );
  const archiveChecksum = createHash("sha256")
    .update(archive.buffer)
    .digest("hex");
  if (archiveChecksum !== checksum) {
    throw new Error(
      `audited Cargo package ${packageRecord.name} archive checksum differs from Cargo.lock`,
    );
  }

  const extractionRoot = mkdtempSync(
    join(tmpdir(), "jarvis-cargo-source-audit."),
  );
  const resolvedTempRoot = realpathSync(extractionRoot);
  const resolvedSystemTemp = realpathSync(tmpdir());
  if (
    dirname(resolvedTempRoot) !== resolvedSystemTemp ||
    !basename(resolvedTempRoot).startsWith("jarvis-cargo-source-audit.")
  ) {
    throw new Error("refusing unexpected Cargo source audit cleanup path");
  }
  try {
    const extraction = spawnSync(
      trustedTarBinary(),
      ["-xzf", "-", "-C", extractionRoot],
      {
        encoding: "utf8",
        env: {
          LANG: "C",
          LC_ALL: "C",
          PATH: "/usr/bin:/bin",
        },
        input: archive.buffer,
        maxBuffer: 1024 * 1024,
        timeout: 30_000,
      },
    );
    if (
      extraction.error ||
      extraction.status !== 0 ||
      extraction.signal !== null
    ) {
      throw new Error(
        `failed to extract audited Cargo package ${packageRecord.name}`,
      );
    }
    const extractedEntries = directoryEntries(
      resolvedTempRoot,
      "audited Cargo extraction root",
    );
    if (
      extractedEntries.length !== 1 ||
      extractedEntries[0].name !== artifactName ||
      !extractedEntries[0].isDirectory() ||
      extractedEntries[0].isSymbolicLink()
    ) {
      throw new Error("audited Cargo archive has an unexpected root layout");
    }
    const expectedRoot = realpathSync(join(extractionRoot, artifactName));
    if (!isInside(resolvedTempRoot, expectedRoot)) {
      throw new Error("audited Cargo archive escaped its extraction root");
    }
    compareSourceTrees(expectedRoot, physicalRoot, {
      bytes: 0,
      entries: 0,
    });
  } finally {
    rmSync(resolvedTempRoot, { force: true, recursive: true });
  }
}
