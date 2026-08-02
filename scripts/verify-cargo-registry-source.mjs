import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdtempSync,
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
import { fileURLToPath } from "node:url";
import { readStableFile } from "./read-stable-file.mjs";

const MAX_CONFIG_BYTES = 1024 * 1024;
const MAX_ARCHIVE_BYTES = 16 * 1024 * 1024;
const MAX_SOURCE_FILE_BYTES = 32 * 1024 * 1024;
const MAX_TREE_MANIFEST_BYTES = 16 * 1024 * 1024;
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

function trustedPythonBinary() {
  for (const candidate of ["/usr/bin/python3"]) {
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
  throw new Error("no trusted system Python binary is available");
}

const treeInspector = readStableFile(
  fileURLToPath(new URL("./inspect-cargo-source-tree.py", import.meta.url)),
  "Cargo source tree inspector",
  1024 * 1024,
);
const treeInspectorSource = new TextDecoder("utf-8", { fatal: true }).decode(
  treeInspector.buffer,
);

function inspectSourceTree(root, label) {
  const before = lstatSync(root, { bigint: true });
  if (before.isSymbolicLink() || !before.isDirectory()) {
    throw new Error(`${label} must be a non-symlink directory`);
  }
  const result = spawnSync(
    trustedPythonBinary(),
    ["-I", "-S", "-c", treeInspectorSource, root],
    {
      encoding: "utf8",
      env: {
        LANG: "C",
        LC_ALL: "C",
        PATH: "/usr/bin:/bin",
      },
      maxBuffer: MAX_TREE_MANIFEST_BYTES,
      timeout: 60_000,
    },
  );
  if (result.error || result.status !== 0 || result.signal !== null) {
    const detail = result.stderr.trim();
    throw new Error(`${label} inspection failed${detail ? `: ${detail}` : ""}`);
  }
  const after = lstatSync(root, { bigint: true });
  const manifest = JSON.parse(result.stdout);
  if (
    !manifest ||
    !Array.isArray(manifest.records) ||
    typeof manifest.root !== "object" ||
    manifest.root.dev !== before.dev.toString() ||
    manifest.root.ino !== before.ino.toString() ||
    manifest.root.mode !== before.mode.toString() ||
    before.dev !== after.dev ||
    before.ino !== after.ino ||
    before.mode !== after.mode ||
    before.mtimeNs !== after.mtimeNs ||
    before.ctimeNs !== after.ctimeNs
  ) {
    throw new Error(`${label} changed while being inspected`);
  }
  return manifest.records;
}

function compareSourceTreeManifests(expected, physical) {
  const expectedByPath = new Map(expected.map((entry) => [entry.path, entry]));
  const physicalWithoutMarker = physical.filter(
    (entry) => entry.path !== ".cargo-ok" || expectedByPath.has(entry.path),
  );
  if (
    expected.length !== physicalWithoutMarker.length ||
    expected.some((entry, index) => {
      const actual = physicalWithoutMarker[index];
      return (
        actual?.path !== entry.path ||
        actual.kind !== entry.kind ||
        actual.size !== entry.size ||
        actual.sha256 !== entry.sha256
      );
    })
  ) {
    const differingFile = expected.find((entry) => {
      const actual = physicalWithoutMarker.find(
        (candidate) => candidate.path === entry.path,
      );
      return (
        entry.kind === "file" &&
        actual?.kind === "file" &&
        (actual.size !== entry.size || actual.sha256 !== entry.sha256)
      );
    });
    if (differingFile) {
      throw new Error(
        `audited Cargo source file differs from its archive: ${differingFile.path}`,
      );
    }
    throw new Error("audited Cargo source tree differs from its crate archive");
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
    const expectedPath = join(resolvedTempRoot, artifactName);
    const expectedRoot = realpathSync(expectedPath);
    if (!isInside(resolvedTempRoot, expectedRoot)) {
      throw new Error("audited Cargo archive escaped its extraction root");
    }
    const expectedManifest = inspectSourceTree(
      expectedPath,
      "audited Cargo archive source",
    );
    const physicalManifest = inspectSourceTree(
      physicalRoot,
      "audited Cargo physical source",
    );
    compareSourceTreeManifests(expectedManifest, physicalManifest);
  } finally {
    rmSync(resolvedTempRoot, { force: true, recursive: true });
  }
}
