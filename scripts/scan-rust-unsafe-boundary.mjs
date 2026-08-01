#!/usr/bin/env node

import {
  existsSync,
  lstatSync,
  readFileSync,
  readdirSync,
  realpathSync,
  statSync,
} from 'node:fs';
import {
  basename,
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from 'node:path';

const MAX_DISCOVERY_ENTRIES = 200_000;
const MAX_RUST_FILES = 20_000;
const MAX_RUST_SOURCE_BYTES = 128 * 1024 * 1024;
const MAX_CARGO_TARGET_SOURCES = 20_000;
const MAX_CARGO_TARGET_SOURCE_BYTES = 8 * 1024 * 1024;
const MAX_DIAGNOSTIC_RECORDS = 1_024;
const MAX_DIAGNOSTIC_BYTES = 1024 * 1024;
const MAX_USE_ALIAS_EDGES = 4_096;
const MAX_USE_ALIAS_CLOSURE_WORK = 8_192;
const MAX_MACRO_DEPENDENCY_EDGES = 4_096;
const EXCLUDED_DIRECTORY_NAMES = new Set([
  '.git',
  '.worktrees',
  'node_modules',
]);

const scanArguments = process.argv.slice(2);
const trustRootsMode = scanArguments[0] === '--trust-roots';
const targetSourcesOption = scanArguments.indexOf('--target-sources-stdin0');
const rootArguments = trustRootsMode
  ? scanArguments.slice(
      1,
      targetSourcesOption === -1 ? scanArguments.length : targetSourcesOption,
    )
  : scanArguments.slice(0, targetSourcesOption === -1 ? 1 : targetSourcesOption);
const unexpectedArguments =
  (trustRootsMode
    ? targetSourcesOption !== -1 && targetSourcesOption !== scanArguments.length - 1
    : rootArguments.length !== 1 ||
      (targetSourcesOption === -1
        ? scanArguments.length !== 1
        : targetSourcesOption !== scanArguments.length - 1)) ||
  scanArguments.includes('--target-sources');
if (rootArguments.length === 0 || unexpectedArguments) {
  console.error(
    'usage: scan-rust-unsafe-boundary.mjs <package-root> [--target-sources-stdin0] | --trust-roots <root>... [--target-sources-stdin0]',
  );
  process.exit(2);
}

async function readTargetSourcesFromStdin() {
  const targetSources = [];
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let pending = Buffer.alloc(0);
  let transportBytes = 0;

  for await (const inputChunk of process.stdin) {
    const chunk = Buffer.isBuffer(inputChunk)
      ? inputChunk
      : Buffer.from(inputChunk);
    transportBytes += chunk.length;
    if (transportBytes > MAX_CARGO_TARGET_SOURCE_BYTES) {
      throw new Error(
        `Cargo target source transport exceeds ${MAX_CARGO_TARGET_SOURCE_BYTES} bytes`,
      );
    }
    pending =
      pending.length === 0
        ? chunk
        : Buffer.concat([pending, chunk], pending.length + chunk.length);
    let recordStart = 0;
    for (
      let terminator = pending.indexOf(0, recordStart);
      terminator !== -1;
      terminator = pending.indexOf(0, recordStart)
    ) {
      targetSources.push(decoder.decode(pending.subarray(recordStart, terminator)));
      if (targetSources.length > MAX_CARGO_TARGET_SOURCES) {
        throw new Error(
          `Cargo target source count exceeds ${MAX_CARGO_TARGET_SOURCES}`,
        );
      }
      recordStart = terminator + 1;
    }
    pending = Buffer.from(pending.subarray(recordStart));
  }
  if (pending.length !== 0) {
    throw new Error('Cargo target source transport must be NUL terminated');
  }
  return targetSources;
}

const explicitTargetSources =
  targetSourcesOption === -1 ? [] : await readTargetSourcesFromStdin();
const scanRoots = [
  ...new Set(rootArguments.map((root) => realpathSync(resolve(root)))),
];
const diagnosticBudget = { records: 0, bytes: 0 };

function diagnosticRecord(kind, path, line, reason) {
  const record = `${kind}\t${path}:${line}${reason ? `\t${reason}` : ''}\n`;
  const recordBytes = Buffer.byteLength(record);
  if (diagnosticBudget.records >= MAX_DIAGNOSTIC_RECORDS) {
    throw new Error(
      `Rust boundary diagnostics exceed ${MAX_DIAGNOSTIC_RECORDS} records`,
    );
  }
  if (diagnosticBudget.bytes + recordBytes > MAX_DIAGNOSTIC_BYTES) {
    throw new Error(
      `Rust boundary diagnostics exceed ${MAX_DIAGNOSTIC_BYTES} bytes`,
    );
  }
  diagnosticBudget.records += 1;
  diagnosticBudget.bytes += recordBytes;
  return record;
}

function rustFiles(root, discoveryBudget) {
  const files = new Set();
  const sourceEscapes = [];
  const rootBuildOutput = join(root, 'target');

  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true }).sort((a, b) =>
      a.name.localeCompare(b.name),
    )) {
      discoveryBudget.entries += 1;
      if (discoveryBudget.entries > MAX_DISCOVERY_ENTRIES) {
        throw new Error(`Rust source discovery exceeds ${MAX_DISCOVERY_ENTRIES} entries`);
      }
      if (EXCLUDED_DIRECTORY_NAMES.has(entry.name)) {
        continue;
      }
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) {
        sourceEscapes.push(
          diagnosticRecord('source', path, 1, 'symlink source entry'),
        );
      } else if (entry.isDirectory()) {
        if (
          path === rootBuildOutput ||
          (entry.name === 'target' && existsSync(join(directory, 'Cargo.toml')))
        ) {
          continue;
        }
        visit(path);
      } else if (entry.isFile() && entry.name.endsWith('.rs')) {
        files.add(path);
        if (files.size > MAX_RUST_FILES) {
          throw new Error(`Rust source discovery exceeds ${MAX_RUST_FILES} files`);
        }
      }
    }
  }

  visit(root);
  return { files: [...files].sort(), sourceEscapes };
}

function isInsideRoot(root, candidate) {
  const relativeCandidate = relative(root, candidate);
  return (
    relativeCandidate !== '..' &&
    !relativeCandidate.startsWith(`..${sep}`) &&
    !isAbsolute(relativeCandidate)
  );
}

function isInsideSkippedBuildOutput(candidate, roots) {
  for (const root of roots) {
    if (!isInsideRoot(root, candidate)) continue;
    const rootBuildOutput = join(root, 'target');
    if (
      candidate === rootBuildOutput ||
      candidate.startsWith(`${rootBuildOutput}${sep}`)
    ) {
      return true;
    }
    let directory = dirname(candidate);
    while (directory !== root && isInsideRoot(root, directory)) {
      if (
        basename(directory) === 'target' &&
        existsSync(join(dirname(directory), 'Cargo.toml'))
      ) {
        return true;
      }
      const parent = dirname(directory);
      if (parent === directory) break;
      directory = parent;
    }
  }
  return false;
}

function isInsideExcludedDirectory(candidate, roots) {
  for (const root of roots) {
    if (!isInsideRoot(root, candidate)) continue;
    let directory = dirname(candidate);
    while (directory !== root && isInsideRoot(root, directory)) {
      if (EXCLUDED_DIRECTORY_NAMES.has(basename(directory))) return true;
      const parent = dirname(directory);
      if (parent === directory) break;
      directory = parent;
    }
  }
  return false;
}

function cargoTargetFiles(targetSources, roots, discoveryBudget) {
  const files = new Set();
  const sourceEscapes = [];
  for (const targetSourceArgument of targetSources) {
    discoveryBudget.entries += 1;
    if (discoveryBudget.entries > MAX_DISCOVERY_ENTRIES) {
      throw new Error(`Rust source discovery exceeds ${MAX_DISCOVERY_ENTRIES} entries`);
    }
    const targetSource = resolve(targetSourceArgument);
    let sourceInfo;
    try {
      sourceInfo = lstatSync(targetSource);
    } catch {
      sourceEscapes.push(
        diagnosticRecord('source', targetSource, 1, 'missing Cargo target source'),
      );
      continue;
    }
    if (sourceInfo.isSymbolicLink()) {
      sourceEscapes.push(
        diagnosticRecord('source', targetSource, 1, 'symlink Cargo target source'),
      );
      continue;
    }
    const realSource = realpathSync(targetSource);
    if (!roots.some((root) => isInsideRoot(root, realSource))) {
      sourceEscapes.push(
        diagnosticRecord(
          'source',
          targetSource,
          1,
          trustRootsMode
            ? 'Cargo target source outside trust roots'
            : 'Cargo target source outside package root',
        ),
      );
      continue;
    }
    if (isInsideSkippedBuildOutput(realSource, roots)) {
      sourceEscapes.push(
        diagnosticRecord(
          'source',
          targetSource,
          1,
          'Cargo target source inside build output',
        ),
      );
      continue;
    }
    if (isInsideExcludedDirectory(realSource, roots)) {
      sourceEscapes.push(
        diagnosticRecord(
          'source',
          targetSource,
          1,
          'Cargo target source inside excluded directory',
        ),
      );
      continue;
    }
    if (!sourceInfo.isFile()) {
      sourceEscapes.push(
        diagnosticRecord(
          'source',
          targetSource,
          1,
          'Cargo target source is not a regular file',
        ),
      );
      continue;
    }
    files.add(realSource);
  }
  return { files: [...files].sort(), sourceEscapes };
}

function isIdentifierStart(character) {
  return /[A-Za-z_]/.test(character) || character.codePointAt(0) > 0x7f;
}

function isIdentifierContinue(character) {
  return /[A-Za-z0-9_]/.test(character) || character.codePointAt(0) > 0x7f;
}

function rawStringStart(source, offset) {
  let cursor = offset;
  if ((source[cursor] === 'b' || source[cursor] === 'c') && source[cursor + 1] === 'r') {
    cursor += 1;
  }
  if (source[cursor] !== 'r') return null;
  cursor += 1;
  let hashes = 0;
  while (source[cursor] === '#') {
    hashes += 1;
    cursor += 1;
  }
  return source[cursor] === '"' ? { contentStart: cursor + 1, hashes } : null;
}

function skipRawString(source, offset, line) {
  const start = rawStringStart(source, offset);
  if (!start) return null;
  const terminator = `"${'#'.repeat(start.hashes)}`;
  const end = source.indexOf(terminator, start.contentStart);
  const limit = end === -1 ? source.length : end + terminator.length;
  for (let cursor = offset; cursor < limit; cursor += 1) {
    if (source[cursor] === '\n') line += 1;
  }
  return { offset: limit, line };
}

function skipQuoted(source, offset, quote, line) {
  let cursor = offset + 1;
  while (cursor < source.length) {
    if (source[cursor] === '\n') line += 1;
    if (source[cursor] === '\\') {
      cursor += 2;
      continue;
    }
    if (source[cursor] === quote) return { offset: cursor + 1, line };
    cursor += 1;
  }
  return { offset: cursor, line };
}

function looksLikeCharacterLiteral(source, offset) {
  let cursor = offset + 1;
  if (cursor >= source.length || source[cursor] === '\n') return false;
  if (source[cursor] === '\\') cursor += 2;
  else cursor += 1;
  return source[cursor] === "'";
}

function lexRust(source) {
  const tokens = [];
  let offset = 0;
  let line = 1;

  while (offset < source.length) {
    const character = source[offset];
    const next = source[offset + 1];

    if (character === '\n') {
      line += 1;
      offset += 1;
      continue;
    }
    if (/\s/.test(character)) {
      offset += 1;
      continue;
    }
    if (character === '/' && next === '/') {
      offset += 2;
      while (offset < source.length && source[offset] !== '\n') offset += 1;
      continue;
    }
    if (character === '/' && next === '*') {
      let depth = 1;
      offset += 2;
      while (offset < source.length && depth > 0) {
        if (source[offset] === '\n') line += 1;
        if (source[offset] === '/' && source[offset + 1] === '*') {
          depth += 1;
          offset += 2;
        } else if (source[offset] === '*' && source[offset + 1] === '/') {
          depth -= 1;
          offset += 2;
        } else {
          offset += 1;
        }
      }
      continue;
    }

    const raw = rawStringStart(source, offset);
    if (raw) {
      ({ offset, line } = skipRawString(source, offset, line));
      continue;
    }
    if (
      character === '"' ||
      ((character === 'b' || character === 'c') && next === '"')
    ) {
      const tokenLine = line;
      const quoteOffset = character === '"' ? offset : offset + 1;
      const skipped = skipQuoted(source, quoteOffset, '"', line);
      if (
        character === '"' &&
        skipped.offset > quoteOffset &&
        source[skipped.offset - 1] === '"'
      ) {
        tokens.push({
          kind: 'string',
          value: source.slice(quoteOffset + 1, skipped.offset - 1),
          raw: false,
          line: tokenLine,
        });
      }
      offset = skipped.offset;
      line = skipped.line;
      continue;
    }
    if (
      character === "'" &&
      looksLikeCharacterLiteral(source, offset)
    ) {
      ({ offset, line } = skipQuoted(source, offset, "'", line));
      continue;
    }
    if (
      character === 'b' &&
      next === "'" &&
      looksLikeCharacterLiteral(source, offset + 1)
    ) {
      const skipped = skipQuoted(source, offset + 1, "'", line);
      offset = skipped.offset;
      line = skipped.line;
      continue;
    }

    if (
      character === 'r' &&
      next === '#' &&
      isIdentifierStart(source[offset + 2] ?? '')
    ) {
      const tokenLine = line;
      offset += 2;
      const start = offset;
      while (offset < source.length && isIdentifierContinue(source[offset])) offset += 1;
      tokens.push({ kind: 'identifier', value: source.slice(start, offset), raw: true, line: tokenLine });
      continue;
    }
    if (isIdentifierStart(character)) {
      const tokenLine = line;
      const start = offset;
      offset += 1;
      while (offset < source.length && isIdentifierContinue(source[offset])) offset += 1;
      tokens.push({
        kind: 'identifier',
        value: source.slice(start, offset),
        raw: false,
        line: tokenLine,
      });
      continue;
    }

    tokens.push({ kind: 'punctuation', value: character, raw: false, line });
    offset += 1;
  }

  return tokens;
}

function matchingToken(tokens, start, open, close, limit = tokens.length) {
  let depth = 0;
  for (let index = start; index < limit; index += 1) {
    if (tokens[index].value === open) depth += 1;
    if (tokens[index].value === close) {
      depth -= 1;
      if (depth === 0) return index;
    }
  }
  return -1;
}

function attributes(tokens) {
  const ranges = [];
  for (let index = 0; index < tokens.length; index += 1) {
    if (tokens[index].value !== '#') continue;
    let bracket = index + 1;
    if (tokens[bracket]?.value === '!') bracket += 1;
    if (tokens[bracket]?.value !== '[') continue;
    const bracketEnd = matchingToken(tokens, bracket, '[', ']');
    if (bracketEnd === -1) continue;
    ranges.push({ start: bracket + 1, end: bracketEnd });
    index = bracketEnd;
  }
  return ranges;
}

function unsafeLintAttributes(tokens) {
  const lines = [];
  for (const attribute of attributes(tokens)) {
    for (let cursor = attribute.start; cursor < attribute.end; cursor += 1) {
      const lintLevel = tokens[cursor];
      if (
        lintLevel.kind !== 'identifier' ||
        !['allow', 'expect', 'warn'].includes(lintLevel.value) ||
        tokens[cursor + 1]?.value !== '('
      ) {
        continue;
      }
      const argumentsEnd = matchingToken(tokens, cursor + 1, '(', ')', attribute.end);
      if (argumentsEnd === -1) continue;
      if (
        tokens
          .slice(cursor + 2, argumentsEnd)
          .some((token) => token.kind === 'identifier' && token.value === 'unsafe_code')
      ) {
        lines.push(lintLevel.line);
      }
      cursor = argumentsEnd;
    }
  }
  return lines;
}

function isCoveredTrustSource(file, literal, discoveredFiles) {
  if (
    !trustRootsMode ||
    literal?.kind !== 'string' ||
    literal.value.includes('\\')
  ) {
    return false;
  }
  const candidate = resolve(dirname(file), literal.value);
  let candidateInfo;
  try {
    candidateInfo = lstatSync(candidate);
  } catch {
    return false;
  }
  return (
    !candidateInfo.isSymbolicLink() &&
    candidateInfo.isFile() &&
    discoveredFiles.has(realpathSync(candidate))
  );
}

function sourceDiscoveryViolations(
  tokens,
  file,
  discoveredFiles,
  sourceMacroNames,
  macroBoundary,
) {
  const violations = [
    ...(macroBoundary.definitionViolations.get(file) ?? []),
  ];
  const imports = macroBoundary.importsByFile.get(file) ?? new Map();
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (
      isNamedSymbol(token, sourceMacroNames) &&
      tokens[index + 1]?.value === '!'
    ) {
      const opener = tokens[index + 2]?.value;
      const literal = ['(', '[', '{'].includes(opener)
        ? tokens[index + 3]
        : undefined;
      if (isCoveredTrustSource(file, literal, discoveredFiles)) continue;
      violations.push({ line: token.line, reason: 'include! source expansion' });
      continue;
    }
    if (
      token.kind !== 'identifier' ||
      tokens[index + 1]?.value !== '!' ||
      !['(', '[', '{'].includes(tokens[index + 2]?.value) ||
      (!token.raw && token.value === 'macro_rules') ||
      (!token.raw && RUST_KEYWORDS.has(token.value)) ||
      tokens[index - 1]?.value === '$'
    ) {
      continue;
    }
    const opener = tokens[index + 2].value;
    const closer = opener === '(' ? ')' : opener === '[' ? ']' : '}';
    const argumentsEnd = matchingToken(tokens, index + 2, opener, closer);
    const macroName = canonicalSymbolName(token.value);
    if (
      argumentsEnd === -1 ||
      !macroInvocationIsAudited(
        tokens,
        index,
        imports,
        macroBoundary.safeLocalMacroNames,
      )
    ) {
      violations.push({
        line: token.line,
        reason: 'unaudited macro invocation may expand Rust source',
      });
      continue;
    }
    const usesLocalDefinition = macroInvocationUsesLocalDefinition(
      tokens,
      index,
      imports,
      macroBoundary.safeLocalMacroNames,
    );
    const argumentsContainBang = tokens
      .slice(index + 3, argumentsEnd)
      .some((argument) => argument.value === '!');
    if (
      usesLocalDefinition &&
      !localMacroArgumentsAreAudited(
        tokens,
        index + 3,
        argumentsEnd,
        imports,
        sourceMacroNames,
        macroBoundary.safeLocalMacroNames,
        macroBoundary.dynamicLocalMacroNames.has(macroName) ||
          argumentsContainBang,
      )
    ) {
      violations.push({
        line: token.line,
        reason: 'local macro invocation may expand Rust source',
      });
    }
  }
  for (const attribute of attributes(tokens)) {
    for (let index = attribute.start; index < attribute.end; index += 1) {
      const token = tokens[index];
      if (
        token.kind === 'identifier' &&
        !token.raw &&
        token.value === 'macro_use'
      ) {
        violations.push({
          line: token.line,
          reason: 'unaudited macro import may expand Rust source',
        });
      }
      if (
        token.kind === 'identifier' &&
        token.value === 'path' &&
        tokens[index + 1]?.value === '='
      ) {
        if (
          isCoveredTrustSource(file, tokens[index + 2], discoveredFiles)
        ) {
          continue;
        }
        violations.push({ line: token.line, reason: 'custom #[path] source' });
      }
    }
  }
  return violations;
}

function cfgTestModuleRanges(tokens) {
  const ranges = [];
  for (const attribute of attributes(tokens)) {
    const attributeTokens = tokens.slice(attribute.start, attribute.end);
    const isExactCfgTest =
      attributeTokens.length === 4 &&
      attributeTokens[0].kind === 'identifier' &&
      attributeTokens[0].value === 'cfg' &&
      attributeTokens[1].value === '(' &&
      attributeTokens[2].kind === 'identifier' &&
      attributeTokens[2].value === 'test' &&
      attributeTokens[3].value === ')';
    if (!isExactCfgTest) {
      continue;
    }
    let module = -1;
    let body = -1;
    for (let cursor = attribute.end + 1; cursor < tokens.length; cursor += 1) {
      if (tokens[cursor].value === ';') break;
      if (
        tokens[cursor].kind === 'identifier' &&
        tokens[cursor].value === 'mod'
      ) {
        module = cursor;
      }
      if (tokens[cursor].value === '{') {
        body = cursor;
        break;
      }
    }
    if (module === -1 || body === -1 || module > body) continue;
    const bodyEnd = matchingToken(tokens, body, '{', '}');
    if (bodyEnd !== -1) ranges.push({ start: body + 1, end: bodyEnd });
  }
  return ranges;
}

function canonicalSymbolName(value) {
  return value.normalize('NFC');
}

function isNamedSymbol(token, names) {
  // Rust raw identifiers, ordinary identifiers, and canonically equivalent
  // Unicode spellings resolve to the same symbol. Keyword recognition must
  // continue to inspect token.raw and the source text stays unmodified.
  return (
    token?.kind === 'identifier' &&
    names.has(canonicalSymbolName(token.value))
  );
}

const SOURCE_INERT_BUILTIN_MACROS = new Set([
  'assert',
  'assert_eq',
  'assert_ne',
  'cfg',
  'concat',
  'debug_assert',
  'debug_assert_eq',
  'env',
  'eprint',
  'eprintln',
  'format',
  'format_args',
  'include_bytes',
  'include_str',
  'matches',
  'module_path',
  'option_env',
  'panic',
  'print',
  'println',
  'stringify',
  'thread_local',
  'todo',
  'unreachable',
  'vec',
  'write',
  'writeln',
].map(canonicalSymbolName));
const AUDITED_SOURCE_INERT_MACRO_PATHS = new Set([
  'objc2::class',
  'objc2::msg_send',
  'ort::inputs',
  'schemars::schema_for',
  'serde_json::json',
  'tauri::generate_context',
  'tauri::generate_handler',
  'tokio::join',
  'tokio::pin',
  'tokio::select',
]);
const LOCAL_MACRO_PATH_PREFIXES = new Set(['crate', 'self', 'super']);
const RUST_KEYWORDS = new Set([
  'as',
  'async',
  'await',
  'break',
  'const',
  'continue',
  'crate',
  'dyn',
  'else',
  'enum',
  'extern',
  'false',
  'fn',
  'for',
  'if',
  'impl',
  'in',
  'let',
  'loop',
  'match',
  'mod',
  'move',
  'mut',
  'pub',
  'ref',
  'return',
  'self',
  'Self',
  'static',
  'struct',
  'super',
  'trait',
  'true',
  'type',
  'union',
  'unsafe',
  'use',
  'where',
  'while',
]);

function identifierPath(tokens, start, end) {
  const parts = [];
  let cursor = start;
  while (
    cursor + 1 < end &&
    tokens[cursor].value === ':' &&
    tokens[cursor + 1].value === ':'
  ) {
    cursor += 2;
  }
  let expectsIdentifier = true;
  while (cursor < end) {
    if (expectsIdentifier) {
      if (tokens[cursor].kind !== 'identifier') return null;
      parts.push(canonicalSymbolName(tokens[cursor].value));
      cursor += 1;
      expectsIdentifier = false;
      continue;
    }
    if (
      cursor + 1 >= end ||
      tokens[cursor].value !== ':' ||
      tokens[cursor + 1].value !== ':'
    ) {
      return null;
    }
    cursor += 2;
    expectsIdentifier = true;
  }
  return parts.length > 0 && !expectsIdentifier ? parts : null;
}

function topLevelToken(tokens, start, end, wanted) {
  const closing = new Map([
    ['(', ')'],
    ['[', ']'],
    ['{', '}'],
  ]);
  const stack = [];
  for (let index = start; index < end; index += 1) {
    const value = tokens[index].value;
    if (stack.length === 0 && value === wanted) return index;
    if (closing.has(value)) {
      stack.push(closing.get(value));
    } else if (stack.at(-1) === value) {
      stack.pop();
    }
  }
  return -1;
}

function addUseTreeImports(tokens, start, end, prefix, imports) {
  while (start < end && tokens[start].value === ',') start += 1;
  while (end > start && tokens[end - 1].value === ',') end -= 1;
  if (start >= end) return;

  const groupStart = topLevelToken(tokens, start, end, '{');
  if (groupStart !== -1) {
    const groupEnd = matchingToken(tokens, groupStart, '{', '}', end);
    if (groupEnd === -1) return;
    let prefixEnd = groupStart;
    while (prefixEnd > start && tokens[prefixEnd - 1].value === ':') {
      prefixEnd -= 1;
    }
    const branchPrefix =
      prefixEnd === start
        ? prefix
        : identifierPath(tokens, start, prefixEnd);
    if (!branchPrefix) return;
    const nestedPrefix =
      prefixEnd === start ? prefix : [...prefix, ...branchPrefix];
    let branchStart = groupStart + 1;
    const closing = new Map([
      ['(', ')'],
      ['[', ']'],
      ['{', '}'],
    ]);
    const stack = [];
    for (let index = branchStart; index < groupEnd; index += 1) {
      const value = tokens[index].value;
      if (stack.length === 0 && value === ',') {
        addUseTreeImports(tokens, branchStart, index, nestedPrefix, imports);
        branchStart = index + 1;
        continue;
      }
      if (closing.has(value)) {
        stack.push(closing.get(value));
      } else if (stack.at(-1) === value) {
        stack.pop();
      }
    }
    addUseTreeImports(tokens, branchStart, groupEnd, nestedPrefix, imports);
    return;
  }

  const aliasIndex = topLevelToken(tokens, start, end, 'as');
  const sourceEnd = aliasIndex === -1 ? end : aliasIndex;
  const suffix = identifierPath(tokens, start, sourceEnd);
  if (!suffix) return;
  let sourceParts = [...prefix, ...suffix];
  let localName;
  if (aliasIndex !== -1) {
    const alias = tokens[aliasIndex + 1];
    if (alias?.kind !== 'identifier' || aliasIndex + 2 !== end) return;
    localName = canonicalSymbolName(alias.value);
  } else if (sourceParts.at(-1) === 'self') {
    // A `self` leaf imports its containing module, not a macro item.
    return;
  } else {
    localName = sourceParts.at(-1);
  }
  if (!localName || sourceParts.length === 0) return;
  let paths = imports.get(localName);
  if (!paths) {
    paths = new Set();
    imports.set(localName, paths);
  }
  paths.add(sourceParts.join('::'));
}

function useImports(tokens) {
  const imports = new Map();
  for (let index = 0; index < tokens.length; index += 1) {
    if (
      tokens[index].kind !== 'identifier' ||
      tokens[index].raw ||
      tokens[index].value !== 'use'
    ) {
      continue;
    }
    let statementEnd = index + 1;
    while (
      statementEnd < tokens.length &&
      tokens[statementEnd].value !== ';'
    ) {
      statementEnd += 1;
    }
    if (statementEnd >= tokens.length) continue;
    addUseTreeImports(tokens, index + 1, statementEnd, [], imports);
    index = statementEnd;
  }
  return imports;
}

function macroInvocationPath(tokens, macroIndex) {
  const parts = [canonicalSymbolName(tokens[macroIndex].value)];
  let start = macroIndex;
  while (
    start >= 3 &&
    tokens[start - 1].value === ':' &&
    tokens[start - 2].value === ':' &&
    tokens[start - 3].kind === 'identifier'
  ) {
    parts.unshift(canonicalSymbolName(tokens[start - 3].value));
    start -= 3;
  }
  return { parts, start };
}

function isAuditedLocalMacroPath(parts, safeLocalMacroNames) {
  return (
    parts.length >= 2 &&
    LOCAL_MACRO_PATH_PREFIXES.has(parts[0]) &&
    safeLocalMacroNames.has(parts.at(-1))
  );
}

function importedMacroPathsAreAudited(paths, safeLocalMacroNames) {
  return [...paths].every((path) => {
    if (AUDITED_SOURCE_INERT_MACRO_PATHS.has(path)) return true;
    const parts = path.split('::');
    return isAuditedLocalMacroPath(parts, safeLocalMacroNames);
  });
}

function macroInvocationUsesLocalDefinition(
  tokens,
  macroIndex,
  imports,
  safeLocalMacroNames,
) {
  const { parts } = macroInvocationPath(tokens, macroIndex);
  if (parts.length > 1) {
    return isAuditedLocalMacroPath(parts, safeLocalMacroNames);
  }
  if (safeLocalMacroNames.has(parts[0])) return true;
  return [...(imports.get(parts[0]) ?? [])].some((path) =>
    isAuditedLocalMacroPath(path.split('::'), safeLocalMacroNames),
  );
}

function macroBoundaryModel(tokenizedFiles, sourceMacroNames) {
  const macroDefinitionSafety = new Map();
  const dynamicLocalMacroNames = new Set();
  const definitionViolations = new Map();
  const importsByFile = new Map();

  for (const { file, tokens } of tokenizedFiles) {
    importsByFile.set(file, useImports(tokens));
    const fileViolations = [];
    for (let index = 0; index < tokens.length - 3; index += 1) {
      if (
        tokens[index].kind !== 'identifier' ||
        tokens[index].raw ||
        tokens[index].value !== 'macro_rules' ||
        tokens[index + 1]?.value !== '!' ||
        tokens[index + 2]?.kind !== 'identifier' ||
        !['(', '[', '{'].includes(tokens[index + 3]?.value)
      ) {
        continue;
      }
      const opener = tokens[index + 3].value;
      const closer = opener === '(' ? ')' : opener === '[' ? ']' : '}';
      const bodyEnd = matchingToken(tokens, index + 3, opener, closer);
      if (bodyEnd === -1) continue;
      const body = tokens.slice(index + 4, bodyEnd);
      const macroName = canonicalSymbolName(tokens[index + 2].value);
      // macro_rules! cannot invent identifier or keyword tokens: every
      // source-discovery token must occur in this definition or in one of its
      // invocation token trees. Keep that provenance boundary lexical and
      // reject unknown external/procedural macro calls instead of guessing at
      // their expansion.
      const sourceSensitive =
        body.some(
          (token) =>
            token.kind === 'identifier' &&
            !token.raw &&
            token.value === 'use',
        ) ||
        body.some((token) => isNamedSymbol(token, sourceMacroNames));
      if (
        body.some(
          (token, bodyIndex) =>
            token.value === '$' &&
            body[bodyIndex + 1]?.kind === 'identifier' &&
            body[bodyIndex + 2]?.value === '!',
        )
      ) {
        dynamicLocalMacroNames.add(macroName);
      }
      macroDefinitionSafety.set(
        macroName,
        (macroDefinitionSafety.get(macroName) ?? true) && !sourceSensitive,
      );
      if (sourceSensitive) {
        fileViolations.push({
          line: tokens[index].line,
          reason: 'source-sensitive declarative macro definition',
        });
      }
      index = bodyEnd;
    }
    definitionViolations.set(file, fileViolations);
  }

  return {
    definitionViolations,
    dynamicLocalMacroNames,
    importsByFile,
    safeLocalMacroNames: new Set(
      [...macroDefinitionSafety]
        .filter(([, safe]) => safe)
        .map(([name]) => name),
    ),
  };
}

function localMacroArgumentsAreAudited(
  tokens,
  start,
  end,
  imports,
  sourceMacroNames,
  safeLocalMacroNames,
  auditQualifiedMacroPaths,
) {
  for (let index = start; index < end; index += 1) {
    const token = tokens[index];
    if (
      token.kind === 'identifier' &&
      ((!token.raw && token.value === 'use') ||
        isNamedSymbol(token, sourceMacroNames))
    ) {
      return false;
    }
    if (token.kind !== 'identifier') continue;
    const name = canonicalSymbolName(token.value);
    const importedPaths = imports.get(name);
    if (
      importedPaths &&
      !importedMacroPathsAreAudited(importedPaths, safeLocalMacroNames)
    ) {
      return false;
    }
    if (
      auditQualifiedMacroPaths &&
      tokens[index + 1]?.value === ':' &&
      tokens[index + 2]?.value === ':' &&
      tokens[index + 3]?.kind === 'identifier'
    ) {
      let pathEnd = index + 3;
      while (
        tokens[pathEnd + 1]?.value === ':' &&
        tokens[pathEnd + 2]?.value === ':' &&
        tokens[pathEnd + 3]?.kind === 'identifier'
      ) {
        pathEnd += 3;
      }
      const parts = [];
      for (let cursor = index; cursor <= pathEnd; cursor += 3) {
        parts.push(canonicalSymbolName(tokens[cursor].value));
      }
      const path = parts.join('::');
      if (
        !AUDITED_SOURCE_INERT_MACRO_PATHS.has(path) &&
        !isAuditedLocalMacroPath(parts, safeLocalMacroNames)
      ) {
        return false;
      }
      index = pathEnd;
    }
  }
  return true;
}

function macroInvocationIsAudited(
  tokens,
  macroIndex,
  imports,
  safeLocalMacroNames,
) {
  const { parts } = macroInvocationPath(tokens, macroIndex);
  if (parts.length > 1) {
    return (
      AUDITED_SOURCE_INERT_MACRO_PATHS.has(parts.join('::')) ||
      isAuditedLocalMacroPath(parts, safeLocalMacroNames)
    );
  }
  const name = parts[0];
  const importedPaths = imports.get(name);
  if (importedPaths) {
    return importedMacroPathsAreAudited(importedPaths, safeLocalMacroNames);
  }
  return (
    SOURCE_INERT_BUILTIN_MACROS.has(name) ||
    safeLocalMacroNames.has(name)
  );
}

function buildUseAliasGraph(tokenizedFiles) {
  const aliases = new Map();
  let edgeCount = 0;

  for (const { tokens } of tokenizedFiles) {
    let inUseStatement = false;
    let branchStart = 0;
    for (let index = 0; index < tokens.length; index += 1) {
      const token = tokens[index];
      if (token.value === ';') {
        inUseStatement = false;
        branchStart = index + 1;
        continue;
      }
      if (
        token.kind === 'identifier' &&
        !token.raw &&
        token.value === 'use'
      ) {
        inUseStatement = true;
        branchStart = index + 1;
        continue;
      }
      if (!inUseStatement) continue;
      if (token.value === '{' || token.value === ',') {
        branchStart = index + 1;
        continue;
      }
      if (
        token.kind !== 'identifier' ||
        token.raw ||
        token.value !== 'as' ||
        tokens[index + 1]?.kind !== 'identifier'
      ) {
        continue;
      }

      let sourceIndex = index - 1;
      while (
        sourceIndex >= branchStart &&
        tokens[sourceIndex].kind !== 'identifier'
      ) {
        sourceIndex -= 1;
      }
      if (sourceIndex < branchStart) continue;

      edgeCount += 1;
      if (edgeCount > MAX_USE_ALIAS_EDGES) {
        throw new Error(
          `Rust use-alias graph exceeds ${MAX_USE_ALIAS_EDGES} edges`,
        );
      }
      const sourceName = canonicalSymbolName(tokens[sourceIndex].value);
      const aliasName = canonicalSymbolName(tokens[index + 1].value);
      let targets = aliases.get(sourceName);
      if (!targets) {
        targets = new Set();
        aliases.set(sourceName, targets);
      }
      targets.add(aliasName);
    }
  }

  return aliases;
}

function expandNameGraph(graphs, names, budgetReason) {
  const queue = [...names];
  let cursor = 0;
  let work = 0;
  let added = false;
  const chargeWork = () => {
    work += 1;
    if (work > MAX_USE_ALIAS_CLOSURE_WORK) {
      throw new Error(
        `${budgetReason} exceeds ${MAX_USE_ALIAS_CLOSURE_WORK} work units`,
      );
    }
  };

  while (cursor < queue.length) {
    chargeWork();
    const sourceName = queue[cursor];
    cursor += 1;
    for (const graph of graphs) {
      for (const aliasName of graph.get(sourceName) ?? []) {
        chargeWork();
        if (names.has(aliasName)) continue;
        names.add(aliasName);
        queue.push(aliasName);
        added = true;
      }
    }
  }
  return added;
}

function addUseAliases(useAliasGraph, names) {
  return expandNameGraph(
    [useAliasGraph],
    names,
    'Rust use-alias closure',
  );
}

function packageTrustVerifierNames(useAliasGraph) {
  const names = new Set([canonicalSymbolName('PackageTrustVerifier')]);
  addUseAliases(useAliasGraph, names);
  return names;
}

function sourceExpandingMacroNames(useAliasGraph) {
  const names = new Set([canonicalSymbolName('include')]);
  addUseAliases(useAliasGraph, names);
  return names;
}

function packageTrustVerifierImplementations(tokens, verifierNames) {
  const implementations = [];
  const testModules = cfgTestModuleRanges(tokens);
  for (let index = 0; index < tokens.length; index += 1) {
    if (
      tokens[index].kind !== 'identifier' ||
      tokens[index].raw ||
      tokens[index].value !== 'impl'
    ) {
      continue;
    }
    let cursor = index + 1;
    if (tokens[cursor]?.value === '<') {
      const genericsEnd = matchingToken(tokens, cursor, '<', '>');
      if (genericsEnd === -1) continue;
      cursor = genericsEnd + 1;
    }
    let verifier = -1;
    let forToken = -1;
    for (; cursor < tokens.length; cursor += 1) {
      const token = tokens[cursor];
      if (token.value === '{' || token.value === ';') break;
      if (isNamedSymbol(token, verifierNames)) {
        verifier = cursor;
      }
      if (
        token.kind === 'identifier' &&
        !token.raw &&
        token.value === 'for'
      ) {
        forToken = cursor;
        break;
      }
    }
    if (verifier === -1 || forToken === -1 || verifier > forToken) continue;
    implementations.push({
      line: tokens[index].line,
      testOnly: testModules.some(
        (range) => index >= range.start && index < range.end,
      ),
    });
  }
  return implementations;
}

function packageTrustVerifierMacroNames(
  tokenizedFiles,
  verifierNames,
  useAliasGraph,
) {
  const macroNames = new Set();
  const macroDependencies = new Map();
  let dependencyEdges = 0;

  for (const { tokens } of tokenizedFiles) {
    for (let index = 0; index < tokens.length - 3; index += 1) {
      if (
        tokens[index].kind !== 'identifier' ||
        tokens[index].raw ||
        tokens[index].value !== 'macro_rules' ||
        tokens[index + 1]?.value !== '!' ||
        tokens[index + 2]?.kind !== 'identifier' ||
        !['(', '[', '{'].includes(tokens[index + 3]?.value)
      ) {
        continue;
      }
      const opener = tokens[index + 3].value;
      const closer = opener === '(' ? ')' : opener === '[' ? ']' : '}';
      const bodyEnd = matchingToken(tokens, index + 3, opener, closer);
      if (bodyEnd === -1) continue;
      const body = tokens.slice(index + 4, bodyEnd);
      const macroName = canonicalSymbolName(tokens[index + 2].value);
      if (body.some((token) => isNamedSymbol(token, verifierNames))) {
        macroNames.add(macroName);
      }
      for (let bodyIndex = 0; bodyIndex < body.length - 1; bodyIndex += 1) {
        if (
          body[bodyIndex].kind !== 'identifier' ||
          body[bodyIndex + 1]?.value !== '!'
        ) {
          continue;
        }
        dependencyEdges += 1;
        if (dependencyEdges > MAX_MACRO_DEPENDENCY_EDGES) {
          throw new Error(
            `Rust macro dependency graph exceeds ${MAX_MACRO_DEPENDENCY_EDGES} edges`,
          );
        }
        const dependencyName = canonicalSymbolName(body[bodyIndex].value);
        let dependents = macroDependencies.get(dependencyName);
        if (!dependents) {
          dependents = new Set();
          macroDependencies.set(dependencyName, dependents);
        }
        dependents.add(macroName);
      }
      index = bodyEnd;
    }
  }
  expandNameGraph(
    [useAliasGraph, macroDependencies],
    macroNames,
    'Rust verifier-macro closure',
  );
  return macroNames;
}

function packageTrustVerifierMacroInvocations(
  tokens,
  verifierNames,
  verifierMacroNames,
) {
  const invocations = [];
  const testModules = cfgTestModuleRanges(tokens);
  for (let index = 0; index < tokens.length - 2; index += 1) {
    if (
      tokens[index].kind !== 'identifier' ||
      tokens[index + 1]?.value !== '!' ||
      !['(', '[', '{'].includes(tokens[index + 2]?.value)
    ) {
      continue;
    }
    const opener = tokens[index + 2].value;
    const closer = opener === '(' ? ')' : opener === '[' ? ']' : '}';
    const argumentsEnd = matchingToken(tokens, index + 2, opener, closer);
    if (argumentsEnd === -1) continue;
    const carriesVerifier =
      isNamedSymbol(tokens[index], verifierMacroNames) ||
      tokens
        .slice(index + 3, argumentsEnd)
        .some(
          (token) =>
            isNamedSymbol(token, verifierNames) ||
            isNamedSymbol(token, verifierMacroNames),
        );
    if (!carriesVerifier) {
      index = argumentsEnd;
      continue;
    }
    invocations.push({
      line: tokens[index].line,
      testOnly: testModules.some(
        (range) => index >= range.start && index < range.end,
      ),
    });
    index = argumentsEnd;
  }
  return invocations;
}

const discoveryBudget = { entries: 0 };
const discoveredFiles = new Set();
const sourceEscapes = [];
for (const root of scanRoots) {
  const discovery = rustFiles(root, discoveryBudget);
  for (const file of discovery.files) {
    discoveredFiles.add(file);
    if (discoveredFiles.size > MAX_RUST_FILES) {
      throw new Error(`Rust source discovery exceeds ${MAX_RUST_FILES} files`);
    }
  }
  sourceEscapes.push(...discovery.sourceEscapes);
}
const cargoTargets = cargoTargetFiles(
  explicitTargetSources,
  scanRoots,
  discoveryBudget,
);
for (const file of cargoTargets.files) {
  discoveredFiles.add(file);
  if (discoveredFiles.size > MAX_RUST_FILES) {
    throw new Error(`Rust source discovery exceeds ${MAX_RUST_FILES} files`);
  }
}
sourceEscapes.push(...cargoTargets.sourceEscapes);
for (const record of sourceEscapes) {
  process.stdout.write(record);
}
let rustSourceBytes = 0;
const tokenizedFiles = [...discoveredFiles].sort().map((file) => {
  rustSourceBytes += statSync(file).size;
  if (rustSourceBytes > MAX_RUST_SOURCE_BYTES) {
    throw new Error(`Rust sources exceed ${MAX_RUST_SOURCE_BYTES} bytes`);
  }
  return {
    file,
    tokens: lexRust(readFileSync(file, 'utf8')),
  };
});
const useAliasGraph = buildUseAliasGraph(tokenizedFiles);
const sourceMacroNames = sourceExpandingMacroNames(useAliasGraph);
const macroBoundary = macroBoundaryModel(tokenizedFiles, sourceMacroNames);
const verifierNames = packageTrustVerifierNames(useAliasGraph);
const verifierMacroNames = packageTrustVerifierMacroNames(
  tokenizedFiles,
  verifierNames,
  useAliasGraph,
);
for (const { file, tokens } of tokenizedFiles) {
  for (const violation of sourceDiscoveryViolations(
    tokens,
    file,
    discoveredFiles,
    sourceMacroNames,
    macroBoundary,
  )) {
    process.stdout.write(
      diagnosticRecord('source', file, violation.line, violation.reason),
    );
  }
  for (const line of unsafeLintAttributes(tokens)) {
    process.stdout.write(diagnosticRecord('allow', file, line));
  }
  for (const token of tokens) {
    if (token.kind === 'identifier' && !token.raw && token.value === 'unsafe') {
      process.stdout.write(diagnosticRecord('unsafe', file, token.line));
    }
  }
  const verifierSites = [
    ...packageTrustVerifierImplementations(tokens, verifierNames),
    ...packageTrustVerifierMacroInvocations(
      tokens,
      verifierNames,
      verifierMacroNames,
    ),
  ];
  const emittedVerifierSites = new Set();
  for (const implementation of verifierSites) {
    const site = `${implementation.line}:${implementation.testOnly}`;
    if (emittedVerifierSites.has(site)) continue;
    emittedVerifierSites.add(site);
    process.stdout.write(
      diagnosticRecord(
        implementation.testOnly ? 'trust-test' : 'trust',
        file,
        implementation.line,
      ),
    );
  }
}
