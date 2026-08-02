#!/usr/bin/env node

import { lstatSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const MAX_METADATA_BYTES = 32 * 1024 * 1024;
const MAX_LOCK_BYTES = 16 * 1024 * 1024;
const MAX_METADATA_PACKAGES = 20_000;
const MAX_RESOLVE_NODES = 20_000;
const MAX_DIRECT_DEPENDENCIES = 4_096;
const MAX_LOCK_PACKAGES = 20_000;
const MAX_PROVIDERS = 64;
const RUST_CRATE_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;
const CHECKSUM = /^[a-f0-9]{64}$/;

if (process.argv.length !== 3) {
  console.error("usage: resolve-cargo-macro-provenance.mjs <Cargo.toml>");
  process.exit(2);
}

function regularFile(path, label, maximumBytes) {
  const absolute = resolve(path);
  const info = lstatSync(absolute);
  if (info.isSymbolicLink() || !info.isFile()) {
    throw new Error(`${label} must be a regular non-symlink file`);
  }
  if (info.size > maximumBytes) {
    throw new Error(`${label} exceeds ${maximumBytes} bytes`);
  }
  return realpathSync(absolute);
}

function exactObject(value, keys, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  if (
    actual.length !== expected.length ||
    actual.some((key, index) => key !== expected[index])
  ) {
    throw new Error(`${label} has unexpected fields`);
  }
}

function validateVersions(versions, label) {
  if (
    !versions ||
    typeof versions !== "object" ||
    Array.isArray(versions) ||
    Object.keys(versions).length === 0
  ) {
    throw new Error(`${label} versions are malformed`);
  }
  for (const [version, checksum] of Object.entries(versions)) {
    if (version.length === 0 || !CHECKSUM.test(checksum)) {
      throw new Error(`${label} identity is malformed`);
    }
  }
}

const auditPath = regularFile(
  fileURLToPath(new URL("./audited-cargo-macros.json", import.meta.url)),
  "Cargo macro audit",
  1024 * 1024,
);
const audit = JSON.parse(readFileSync(auditPath, "utf8"));
exactObject(
  audit,
  ["packages", "providers", "registrySource"],
  "Cargo macro audit",
);
if (
  typeof audit.registrySource !== "string" ||
  !audit.packages ||
  typeof audit.packages !== "object" ||
  Array.isArray(audit.packages) ||
  !audit.providers ||
  typeof audit.providers !== "object" ||
  Array.isArray(audit.providers)
) {
  throw new Error("Cargo macro audit is malformed");
}
if (Object.keys(audit.providers).length > MAX_PROVIDERS) {
  throw new Error(`Cargo macro audit exceeds ${MAX_PROVIDERS} providers`);
}
for (const [providerName, providerPolicy] of Object.entries(audit.providers)) {
  if (!RUST_CRATE_NAME.test(providerName.replaceAll("-", "_"))) {
    throw new Error(`Cargo macro provider ${providerName} is malformed`);
  }
  exactObject(
    providerPolicy,
    ["versions"],
    `Cargo macro provider ${providerName}`,
  );
  validateVersions(
    providerPolicy.versions,
    `Cargo macro provider ${providerName}`,
  );
}
for (const [packageName, policy] of Object.entries(audit.packages)) {
  if (!RUST_CRATE_NAME.test(packageName.replaceAll("-", "_"))) {
    throw new Error(`Cargo macro audit package ${packageName} is malformed`);
  }
  exactObject(
    policy,
    ["macros", "versions"],
    `Cargo macro audit ${packageName}`,
  );
  validateVersions(policy.versions, `Cargo macro audit ${packageName}`);
  if (
    !policy.macros ||
    typeof policy.macros !== "object" ||
    Array.isArray(policy.macros) ||
    Object.keys(policy.macros).length === 0
  ) {
    throw new Error(`Cargo macro audit ${packageName} macros are malformed`);
  }
  for (const [macroName, providerName] of Object.entries(policy.macros)) {
    if (
      !RUST_CRATE_NAME.test(macroName) ||
      (providerName !== null &&
        (typeof providerName !== "string" ||
          !Object.hasOwn(audit.providers, providerName)))
    ) {
      throw new Error(
        `Cargo macro audit ${packageName} macro ${macroName} is malformed`,
      );
    }
  }
}

const manifestPath = regularFile(
  process.argv[2],
  "Cargo manifest",
  1024 * 1024,
);

let metadataBytes = 0;
const metadataChunks = [];
for await (const chunk of process.stdin) {
  const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
  metadataBytes += buffer.length;
  if (metadataBytes > MAX_METADATA_BYTES) {
    throw new Error(`Cargo metadata exceeds ${MAX_METADATA_BYTES} bytes`);
  }
  metadataChunks.push(buffer);
}
const metadataBuffer = Buffer.concat(metadataChunks, metadataBytes);
if (metadataBuffer.includes(0)) {
  throw new Error("Cargo metadata contains a NUL byte");
}
const metadata = JSON.parse(
  new TextDecoder("utf-8", { fatal: true }).decode(metadataBuffer),
);
if (typeof metadata.workspace_root !== "string") {
  throw new Error("Cargo metadata workspace root is missing");
}
const workspaceRoot = realpathSync(resolve(metadata.workspace_root));
const relativeManifest = relative(workspaceRoot, manifestPath);
if (relativeManifest === ".." || relativeManifest.startsWith(`..${sep}`)) {
  throw new Error("Cargo manifest is outside its metadata workspace");
}
const lockPath = regularFile(
  join(workspaceRoot, "Cargo.lock"),
  "Cargo lockfile",
  MAX_LOCK_BYTES,
);

function parseLockPackages(source) {
  const packages = [];
  let current = null;
  const finish = () => {
    if (!current) return;
    if (
      typeof current.name !== "string" ||
      typeof current.version !== "string"
    ) {
      throw new Error("Cargo lockfile contains an incomplete package record");
    }
    packages.push(current);
    if (packages.length > MAX_LOCK_PACKAGES) {
      throw new Error(`Cargo lockfile exceeds ${MAX_LOCK_PACKAGES} packages`);
    }
  };
  for (const line of source.split(/\r?\n/)) {
    if (line === "[[package]]") {
      finish();
      current = Object.create(null);
      continue;
    }
    if (!current) continue;
    const relevant = line.match(/^(name|version|source|checksum)\s*=/);
    if (!relevant) continue;
    const field = line.match(
      /^(name|version|source|checksum)\s*=\s*("(?:[^"\\]|\\.)*")\s*$/,
    );
    if (!field) {
      throw new Error(`Cargo lockfile has unsupported ${relevant[1]} syntax`);
    }
    if (Object.hasOwn(current, field[1])) {
      throw new Error(`Cargo lockfile repeats package field ${field[1]}`);
    }
    current[field[1]] = JSON.parse(field[2]);
  }
  finish();
  return packages;
}

const lockPackages = parseLockPackages(readFileSync(lockPath, "utf8"));
if (
  !Array.isArray(metadata.packages) ||
  metadata.packages.length > MAX_METADATA_PACKAGES
) {
  throw new Error(
    `Cargo metadata packages must not exceed ${MAX_METADATA_PACKAGES}`,
  );
}
if (
  !metadata.resolve ||
  typeof metadata.resolve !== "object" ||
  !Array.isArray(metadata.resolve.nodes)
) {
  throw new Error("Cargo metadata resolve graph is missing");
}
if (metadata.resolve.nodes.length > MAX_RESOLVE_NODES) {
  throw new Error(
    `Cargo metadata resolve graph exceeds ${MAX_RESOLVE_NODES} nodes`,
  );
}

const packageMatches = metadata.packages.filter((candidate) => {
  if (typeof candidate?.manifest_path !== "string") return false;
  try {
    return realpathSync(candidate.manifest_path) === manifestPath;
  } catch {
    return false;
  }
});
if (packageMatches.length !== 1) {
  throw new Error(
    "Cargo metadata does not uniquely contain the requested package",
  );
}
const packageRecord = packageMatches[0];
if (
  typeof packageRecord.id !== "string" ||
  !Array.isArray(packageRecord.dependencies) ||
  packageRecord.dependencies.length > MAX_DIRECT_DEPENDENCIES
) {
  throw new Error("Cargo metadata requested package is malformed");
}

function uniqueById(records, label) {
  const byId = new Map();
  for (const record of records) {
    if (!record || typeof record.id !== "string") {
      throw new Error(`${label} contains a malformed identity`);
    }
    if (byId.has(record.id)) {
      throw new Error(`${label} repeats identity ${record.id}`);
    }
    byId.set(record.id, record);
  }
  return byId;
}

const packagesById = uniqueById(metadata.packages, "Cargo metadata packages");
const nodesById = uniqueById(
  metadata.resolve.nodes,
  "Cargo metadata resolve nodes",
);
const rootNode = nodesById.get(packageRecord.id);
if (!rootNode || !Array.isArray(rootNode.deps)) {
  throw new Error(
    "Cargo metadata resolve node is missing for requested package",
  );
}
if (rootNode.deps.length > MAX_DIRECT_DEPENDENCIES) {
  throw new Error(
    `Cargo resolve exceeds ${MAX_DIRECT_DEPENDENCIES} direct dependencies`,
  );
}

function normalizedAlias(value) {
  if (typeof value !== "string") return null;
  const normalized = value.replaceAll("-", "_");
  return RUST_CRATE_NAME.test(normalized) ? normalized : null;
}

function groupResolvedDependencies(node, label) {
  if (!node || !Array.isArray(node.deps)) {
    throw new Error(`${label} resolve node is missing`);
  }
  if (node.deps.length > MAX_DIRECT_DEPENDENCIES) {
    throw new Error(
      `${label} exceeds ${MAX_DIRECT_DEPENDENCIES} resolved dependencies`,
    );
  }
  const groups = new Map();
  for (const dependency of node.deps) {
    const alias = normalizedAlias(dependency?.name);
    if (!alias || typeof dependency.pkg !== "string") {
      throw new Error(`${label} contains a malformed resolved dependency`);
    }
    let packageIds = groups.get(alias);
    if (!packageIds) {
      packageIds = new Set();
      groups.set(alias, packageIds);
    }
    packageIds.add(dependency.pkg);
  }
  return groups;
}

const resolvedAliases = groupResolvedDependencies(rootNode, "Cargo package");
const declaredAuditedAliases = new Map();
for (const dependency of packageRecord.dependencies) {
  if (!dependency || typeof dependency !== "object") {
    throw new Error("Cargo metadata contains a malformed dependency");
  }
  const localAlias = normalizedAlias(dependency.rename ?? dependency.name);
  if (!localAlias || typeof dependency.name !== "string") {
    throw new Error("Cargo metadata dependency has no valid local alias");
  }
  if (
    Object.hasOwn(audit.packages, localAlias) &&
    dependency.name !== localAlias
  ) {
    throw new Error(
      `audited Cargo macro alias ${localAlias} resolves to unaudited package ${dependency.name}`,
    );
  }
  if (!Object.hasOwn(audit.packages, dependency.name)) continue;
  if (dependency.source !== audit.registrySource || dependency.path != null) {
    throw new Error(
      `audited Cargo macro package ${dependency.name} must use the crates.io registry`,
    );
  }
  const previous = declaredAuditedAliases.get(localAlias);
  if (previous && previous !== dependency.name) {
    throw new Error(`Cargo macro alias ${localAlias} is ambiguous`);
  }
  declaredAuditedAliases.set(localAlias, dependency.name);
}

function lockIdentity(name, version, source, versions, label) {
  if (source !== audit.registrySource) {
    throw new Error(`${label} must use the crates.io registry`);
  }
  const checksum = versions[version];
  if (typeof checksum !== "string") {
    throw new Error(`${label} ${version} is not audited`);
  }
  const locked = lockPackages.filter(
    (candidate) =>
      candidate.name === name &&
      candidate.version === version &&
      candidate.source === source,
  );
  if (locked.length !== 1) {
    throw new Error(
      `Cargo lock identity is missing or ambiguous for ${name} ${version}`,
    );
  }
  if (!CHECKSUM.test(locked[0].checksum ?? "")) {
    throw new Error(`${name} ${version} has no valid locked checksum`);
  }
  if (locked[0].checksum !== checksum) {
    throw new Error(`${name} ${version} does not match an audited checksum`);
  }
  return {
    package: name,
    version,
    source,
    checksum,
  };
}

function resolvedPackage(packageId, label) {
  const record = packagesById.get(packageId);
  if (!record) {
    throw new Error(`Cargo resolve package identity is missing for ${label}`);
  }
  if (
    typeof record.name !== "string" ||
    typeof record.version !== "string" ||
    !Object.hasOwn(record, "source")
  ) {
    throw new Error(`Cargo resolve package identity is malformed for ${label}`);
  }
  return record;
}

function providerIdentities(packageIdentity, policy) {
  const expectedProviders = new Set(
    Object.values(policy.macros).filter(
      (providerName) => providerName !== null,
    ),
  );
  const providers = Object.create(null);
  if (expectedProviders.size === 0) return providers;

  const providerNode = nodesById.get(packageIdentity.id);
  const providerEdges = groupResolvedDependencies(
    providerNode,
    `Cargo macro package ${packageIdentity.name}`,
  );
  for (const providerName of expectedProviders) {
    const matches = new Set();
    for (const packageIds of providerEdges.values()) {
      for (const packageId of packageIds) {
        const candidate = resolvedPackage(
          packageId,
          `macro provider ${providerName}`,
        );
        if (candidate.name === providerName) matches.add(packageId);
      }
    }
    if (matches.size === 0) {
      throw new Error(
        `Cargo macro provider ${providerName} is missing for ${packageIdentity.name}`,
      );
    }
    if (matches.size !== 1) {
      throw new Error(`Cargo macro provider ${providerName} is ambiguous`);
    }
    const [providerId] = matches;
    const provider = resolvedPackage(
      providerId,
      `macro provider ${providerName}`,
    );
    providers[providerName] = lockIdentity(
      provider.name,
      provider.version,
      provider.source,
      audit.providers[providerName].versions,
      `Cargo macro provider ${providerName}`,
    );
  }
  return providers;
}

const aliases = Object.create(null);
for (const [localAlias, packageIds] of resolvedAliases) {
  const relevant =
    Object.hasOwn(audit.packages, localAlias) ||
    [...packageIds].some((packageId) => {
      const candidate = packagesById.get(packageId);
      return candidate && Object.hasOwn(audit.packages, candidate.name);
    });
  if (!relevant) continue;
  if (packageIds.size !== 1) {
    throw new Error(`Cargo macro alias ${localAlias} is ambiguous`);
  }
  const [packageId] = packageIds;
  const resolved = resolvedPackage(packageId, `alias ${localAlias}`);
  if (
    Object.hasOwn(audit.packages, localAlias) &&
    resolved.name !== localAlias
  ) {
    throw new Error(
      `audited Cargo macro alias ${localAlias} resolves to unaudited package ${resolved.name}`,
    );
  }
  const policy = audit.packages[resolved.name];
  if (!policy) continue;
  const declaredName = declaredAuditedAliases.get(localAlias);
  if (declaredName && declaredName !== resolved.name) {
    throw new Error(`Cargo macro alias ${localAlias} is ambiguous`);
  }
  const identity = lockIdentity(
    resolved.name,
    resolved.version,
    resolved.source,
    policy.versions,
    `audited Cargo macro package ${resolved.name}`,
  );
  aliases[localAlias] = {
    ...identity,
    providers: providerIdentities(resolved, policy),
  };
}

for (const [localAlias, packageName] of declaredAuditedAliases) {
  if (!Object.hasOwn(aliases, localAlias)) {
    const packageIds = resolvedAliases.get(localAlias);
    if (!packageIds || packageIds.size === 0) {
      throw new Error(`Cargo resolve is missing audited alias ${localAlias}`);
    }
    if (packageIds.size !== 1) {
      throw new Error(`Cargo macro alias ${localAlias} is ambiguous`);
    }
    throw new Error(
      `Cargo resolve alias ${localAlias} does not match audited package ${packageName}`,
    );
  }
}

process.stdout.write(
  `${JSON.stringify({
    packageRoot: realpathSync(dirname(manifestPath)),
    manifestPath,
    aliases,
  })}\n`,
);
