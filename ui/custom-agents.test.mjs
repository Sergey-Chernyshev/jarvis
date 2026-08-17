/* Свои агенты в панели.
 *
 * Реестр делает агентом любую команду, и панель обязана это отражать в трёх
 * местах: кнопки «Нового проекта», команда возобновления, вкладка настроек.
 * Тесты источниковые: держат провода, чтобы кнопка не потерялась молча. */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const renderer = readFileSync(new URL('./renderer.js', import.meta.url), 'utf8');
const settings = readFileSync(new URL('./settings2.js', import.meta.url), 'utf8');
const bridge = readFileSync(new URL('./bridge.js', import.meta.url), 'utf8');

test('мост знает команды реестра', () => {
  assert.ok(bridge.includes("invoke('agents_list')"), 'agents_list не проброшен');
  assert.ok(bridge.includes("invoke('agents_save'"), 'agents_save не проброшен');
});

test('кнопки своих агентов есть в «Новом проекте» и только локально', () => {
  const at = renderer.indexOf('customAgents) form.append(btn(');
  assert.ok(at > 0, 'кнопок своих агентов нет в форме');
  const line = renderer.slice(renderer.lastIndexOf('\n', at), at);
  assert.ok(line.includes('!remote'), 'на узле шима нет — кнопка там была бы обманом');
});

test('команда возобновления не выдумывает флаги чужому CLI', () => {
  assert.ok(renderer.includes('function resumeBase('), 'общей точки резюме нет');
  // Шаблон человека — как есть; без шаблона — просто имя шима, а не выдуманный флаг.
  assert.ok(/a && a\.resume/.test(renderer), 'шаблон resume не используется');
  assert.ok(!/`\$\{agent\} --resume/.test(renderer), 'флаг --resume приписан чужому агенту');
});

test('вкладка настроек подключена и честна про границы', () => {
  assert.ok(settings.includes("pane: 'agents'"), 'вкладки нет в сайдбаре');
  assert.ok(settings.includes('agents: renderAgents'), 'рендерер вкладки не зарегистрирован');
  assert.ok(settings.includes('не выдумыва'), 'обещание границ возможностей пропало');
});
