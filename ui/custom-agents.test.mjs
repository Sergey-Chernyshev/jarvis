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
  // Список кнопок собирает одна точка (launchAgents) — она же обязана добавлять
  // своих агентов и обязана не добавлять их на узел, где шима нет.
  const at = renderer.indexOf('function launchAgents(');
  assert.ok(at > 0, 'общей точки списка агентов нет');
  const body = renderer.slice(at, renderer.indexOf('\n}', at));
  assert.ok(body.includes('customAgents'), 'свои агенты не попадают в кнопки запуска');
  assert.ok(/if \(!remote\)/.test(body), 'на узле шима нет — кнопка там была бы обманом');
  assert.ok(renderer.includes('for (const a of agents) form.append(btn('), 'форма «Нового проекта» рисует кнопки не из списка');
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
