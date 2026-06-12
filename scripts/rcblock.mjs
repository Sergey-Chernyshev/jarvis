/**
 * Managed-блок Jarvis в rc-файлах шелла (~/.zshrc и т.п.).
 * Блок живёт между маркерами и заменяется целиком — merge, не overwrite.
 * Чистые функции без I/O, чтобы гоняться тестами против любых строк.
 */

export const BEGIN = '# >>> jarvis >>>';
export const END = '# <<< jarvis <<<';

export function blockBody(shimsDir) {
  return `${BEGIN}
# Управляется Jarvis (npm run setup/teardown) — не редактируй вручную
export PATH="${shimsDir}:$PATH"
${END}`;
}

export function hasBlock(content) {
  return content.includes(BEGIN) && content.includes(END);
}

/** Вставить или заменить блок. Идемпотентно: повторный вызов ничего не меняет. */
export function mergeBlock(content, shimsDir) {
  const block = blockBody(shimsDir);
  if (hasBlock(content)) {
    const re = new RegExp(
      `${escapeRe(BEGIN)}[\\s\\S]*?${escapeRe(END)}`,
      'g',
    );
    return content.replace(re, block);
  }
  const sep = content.length && !content.endsWith('\n') ? '\n' : '';
  return `${content}${sep}\n${block}\n`;
}

/** Убрать блок вместе с окружающими его пустыми строками. */
export function removeBlock(content) {
  if (!hasBlock(content)) return content;
  const re = new RegExp(
    `\\n*${escapeRe(BEGIN)}[\\s\\S]*?${escapeRe(END)}\\n?`,
    'g',
  );
  return content.replace(re, '\n').replace(/\n{3,}/g, '\n\n');
}

function escapeRe(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}
