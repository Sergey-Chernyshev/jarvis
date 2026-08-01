#!/usr/bin/env node

import {
  existsSync,
  lstatSync,
  readFileSync,
  readdirSync,
  realpathSync,
  statSync,
} from 'node:fs';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';

const MAX_DISCOVERY_ENTRIES = 200_000;
const MAX_RUST_FILES = 20_000;
const MAX_RUST_SOURCE_BYTES = 128 * 1024 * 1024;

const scanArguments = process.argv.slice(2);
const trustRootsMode = scanArguments[0] === '--trust-roots';
const rootArguments = trustRootsMode ? scanArguments.slice(1) : scanArguments.slice(0, 1);
if (rootArguments.length === 0) {
  console.error(
    'usage: scan-rust-unsafe-boundary.mjs <package-root> [target-sources...] | --trust-roots <root>...',
  );
  process.exit(2);
}
const scanRoots = [...new Set(rootArguments.map((root) => realpathSync(resolve(root))))];
const explicitTargetSources = trustRootsMode ? [] : scanArguments.slice(1);

function rustFiles(root, targetSources, discoveryBudget) {
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
      if (
        entry.name === '.git' ||
        entry.name === '.worktrees' ||
        entry.name === 'node_modules'
      ) {
        continue;
      }
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) {
        sourceEscapes.push({ path, line: 1, reason: 'symlink source entry' });
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
  for (const targetSource of targetSources) {
    let sourceInfo;
    try {
      sourceInfo = lstatSync(targetSource);
    } catch {
      sourceEscapes.push({
        path: targetSource,
        line: 1,
        reason: 'missing Cargo target source',
      });
      continue;
    }
    if (sourceInfo.isSymbolicLink()) {
      sourceEscapes.push({
        path: targetSource,
        line: 1,
        reason: 'symlink Cargo target source',
      });
      continue;
    }
    const realSource = realpathSync(targetSource);
    const relativeSource = relative(root, realSource);
    if (
      relativeSource === '..' ||
      relativeSource.startsWith(`..${sep}`) ||
      isAbsolute(relativeSource)
    ) {
      sourceEscapes.push({
        path: targetSource,
        line: 1,
        reason: 'Cargo target source outside package root',
      });
      continue;
    }
    if (
      realSource === rootBuildOutput ||
      realSource.startsWith(`${rootBuildOutput}${sep}`)
    ) {
      sourceEscapes.push({
        path: targetSource,
        line: 1,
        reason: 'Cargo target source inside build output',
      });
      continue;
    }
    if (!sourceInfo.isFile()) {
      sourceEscapes.push({
        path: targetSource,
        line: 1,
        reason: 'Cargo target source is not a regular file',
      });
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
      const quoteOffset = character === '"' ? offset : offset + 1;
      const skipped = skipQuoted(source, quoteOffset, '"', line);
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

function sourceDiscoveryViolations(tokens) {
  const violations = [];
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (
      token.kind === 'identifier' &&
      !token.raw &&
      token.value === 'include' &&
      tokens[index + 1]?.value === '!'
    ) {
      violations.push({ line: token.line, reason: 'include! source expansion' });
    }
  }
  for (const attribute of attributes(tokens)) {
    for (let index = attribute.start; index < attribute.end; index += 1) {
      const token = tokens[index];
      if (
        token.kind === 'identifier' &&
        !token.raw &&
        token.value === 'path' &&
        tokens[index + 1]?.value === '='
      ) {
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

function addUseAliases(tokenizedFiles, names) {
  let added = false;
  let changed = true;
  while (changed) {
    changed = false;
    for (const { tokens } of tokenizedFiles) {
      for (let index = 0; index < tokens.length; index += 1) {
        if (
          tokens[index].kind !== 'identifier' ||
          tokens[index].raw ||
          tokens[index].value !== 'as' ||
          tokens[index + 1]?.kind !== 'identifier' ||
          tokens[index + 1]?.raw
        ) {
          continue;
        }
        let statementStart = index - 1;
        while (
          statementStart >= 0 &&
          tokens[statementStart].value !== ';'
        ) {
          statementStart -= 1;
        }
        const useIndex = tokens
          .slice(statementStart + 1, index)
          .findIndex(
            (token) =>
              token.kind === 'identifier' &&
              !token.raw &&
              token.value === 'use',
          );
        if (useIndex === -1) continue;

        let branchStart = index - 1;
        while (
          branchStart > statementStart &&
          tokens[branchStart].value !== ',' &&
          tokens[branchStart].value !== '{'
        ) {
          branchStart -= 1;
        }
        if (
          !tokens
            .slice(branchStart + 1, index)
            .some(
              (token) =>
                token.kind === 'identifier' &&
                !token.raw &&
                names.has(token.value),
            )
        ) {
          continue;
        }
        if (!names.has(tokens[index + 1].value)) {
          names.add(tokens[index + 1].value);
          changed = true;
          added = true;
        }
      }
    }
  }
  return added;
}

function packageTrustVerifierNames(tokenizedFiles) {
  const names = new Set(['PackageTrustVerifier']);
  addUseAliases(tokenizedFiles, names);
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
      if (
        token.kind === 'identifier' &&
        !token.raw &&
        verifierNames.has(token.value)
      ) {
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

function packageTrustVerifierMacroNames(tokenizedFiles, verifierNames) {
  const macroNames = new Set();
  let changed = true;
  while (changed) {
    changed = addUseAliases(tokenizedFiles, macroNames);
    for (const { tokens } of tokenizedFiles) {
      for (let index = 0; index < tokens.length - 3; index += 1) {
        if (
          tokens[index].kind !== 'identifier' ||
          tokens[index].raw ||
          tokens[index].value !== 'macro_rules' ||
          tokens[index + 1]?.value !== '!' ||
          tokens[index + 2]?.kind !== 'identifier' ||
          tokens[index + 2]?.raw ||
          !['(', '[', '{'].includes(tokens[index + 3]?.value)
        ) {
          continue;
        }
        const opener = tokens[index + 3].value;
        const closer = opener === '(' ? ')' : opener === '[' ? ']' : '}';
        const bodyEnd = matchingToken(tokens, index + 3, opener, closer);
        if (bodyEnd === -1) continue;
        const body = tokens.slice(index + 4, bodyEnd);
        const namesVerifier =
          body.some(
            (token) =>
              token.kind === 'identifier' &&
              !token.raw &&
              verifierNames.has(token.value),
          ) ||
          body.some(
            (token, bodyIndex) =>
              token.kind === 'identifier' &&
              !token.raw &&
              macroNames.has(token.value) &&
              body[bodyIndex + 1]?.value === '!',
          );
        if (namesVerifier && !macroNames.has(tokens[index + 2].value)) {
          macroNames.add(tokens[index + 2].value);
          changed = true;
        }
        index = bodyEnd;
      }
    }
  }
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
      tokens[index].raw ||
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
      verifierMacroNames.has(tokens[index].value) ||
      tokens
        .slice(index + 3, argumentsEnd)
        .some(
          (token) =>
            token.kind === 'identifier' &&
            !token.raw &&
            verifierNames.has(token.value),
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
  const discovery = rustFiles(root, explicitTargetSources, discoveryBudget);
  for (const file of discovery.files) {
    discoveredFiles.add(file);
    if (discoveredFiles.size > MAX_RUST_FILES) {
      throw new Error(`Rust source discovery exceeds ${MAX_RUST_FILES} files`);
    }
  }
  sourceEscapes.push(...discovery.sourceEscapes);
}
for (const escape of sourceEscapes) {
  process.stdout.write(`source\t${escape.path}:${escape.line}\t${escape.reason}\n`);
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
const verifierNames = packageTrustVerifierNames(tokenizedFiles);
const verifierMacroNames = packageTrustVerifierMacroNames(
  tokenizedFiles,
  verifierNames,
);
for (const { file, tokens } of tokenizedFiles) {
  for (const violation of sourceDiscoveryViolations(tokens)) {
    process.stdout.write(`source\t${file}:${violation.line}\t${violation.reason}\n`);
  }
  for (const line of unsafeLintAttributes(tokens)) {
    process.stdout.write(`allow\t${file}:${line}\n`);
  }
  for (const token of tokens) {
    if (token.kind === 'identifier' && !token.raw && token.value === 'unsafe') {
      process.stdout.write(`unsafe\t${file}:${token.line}\n`);
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
      `${implementation.testOnly ? 'trust-test' : 'trust'}\t${file}:${implementation.line}\n`,
    );
  }
}
