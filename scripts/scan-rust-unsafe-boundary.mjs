#!/usr/bin/env node

import { readFileSync, readdirSync, realpathSync } from 'node:fs';
import { join, resolve } from 'node:path';

const packageRoot = process.argv[2] ? realpathSync(resolve(process.argv[2])) : '';
if (!packageRoot) {
  console.error('usage: scan-rust-unsafe-boundary.mjs <package-root>');
  process.exit(2);
}

function rustFiles(root) {
  const files = [];

  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true }).sort((a, b) =>
      a.name.localeCompare(b.name),
    )) {
      if (entry.name === 'target' || entry.name === '.git') continue;
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        visit(path);
      } else if ((entry.isFile() || entry.isSymbolicLink()) && entry.name.endsWith('.rs')) {
        files.push(path);
      }
    }
  }

  visit(root);
  return files;
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

function unsafeLintAttributes(tokens) {
  const lines = [];
  for (let index = 0; index < tokens.length; index += 1) {
    if (tokens[index].value !== '#') continue;
    let bracket = index + 1;
    if (tokens[bracket]?.value === '!') bracket += 1;
    if (tokens[bracket]?.value !== '[') continue;
    const bracketEnd = matchingToken(tokens, bracket, '[', ']');
    if (bracketEnd === -1) continue;

    for (let cursor = bracket + 1; cursor < bracketEnd; cursor += 1) {
      const lintLevel = tokens[cursor];
      if (
        lintLevel.kind !== 'identifier' ||
        !['allow', 'expect', 'warn'].includes(lintLevel.value) ||
        tokens[cursor + 1]?.value !== '('
      ) {
        continue;
      }
      const argumentsEnd = matchingToken(tokens, cursor + 1, '(', ')', bracketEnd);
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
    index = bracketEnd;
  }
  return lines;
}

for (const file of rustFiles(packageRoot)) {
  const tokens = lexRust(readFileSync(file, 'utf8'));
  for (const line of unsafeLintAttributes(tokens)) {
    process.stdout.write(`allow\t${file}:${line}\n`);
  }
  for (const token of tokens) {
    if (token.kind === 'identifier' && !token.raw && token.value === 'unsafe') {
      process.stdout.write(`unsafe\t${file}:${token.line}\n`);
    }
  }
}
