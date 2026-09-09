/* Проверить выгруженный .bpmn ЧУЖИМ парсером — тем самым, на котором стоят
 * bpmn-js и Camunda Modeler.
 *
 * Зачем отдельно от тестов: наши тесты проверяют, что мы читаем то, что сами
 * написали, — и на этом вопросе сходятся всегда. А открывается файл не нами.
 * Ровно так пропущена была кириллица в идентификаторах: XML её разрешает,
 * moddle отвечает «illegal ID», и узел не разбирается ЦЕЛИКОМ — полотно
 * открывается пустым, без единого слова о причине.
 *
 *   npm i --no-save bpmn-moddle
 *   node scripts/check-bpmn.mjs ~/.jarvis/pipelines/ночной-loop-1.bpmn
 *
 * `--rewrite` дополнительно просит moddle переписать файл своими руками:
 * то, что мы потом прочитаем обратно, и есть настоящий круг через модельер.
 */
import fs from 'node:fs';

const [path, ...flags] = process.argv.slice(2);
if (!path) {
  console.error('нужен путь к .bpmn');
  process.exit(2);
}

let BpmnModdle;
try {
  const m = await import('bpmn-moddle');
  BpmnModdle = m.BpmnModdle || m.default || m;
} catch {
  console.error('нет bpmn-moddle — поставь: npm i --no-save bpmn-moddle');
  process.exit(2);
}

const moddle = new BpmnModdle();
let parsed;
try {
  parsed = await moddle.fromXML(fs.readFileSync(path, 'utf8'));
} catch (e) {
  console.error('ОТКАЗ ПАРСЕРА:', e.message);
  process.exit(1);
}

const warnings = parsed.warnings || [];
for (const w of warnings) console.error('⚠', w.message.split('\n').join(' · '));
if (warnings.length) {
  console.error(`\n${warnings.length} предупреждений — Camunda Modeler покажет этот файл неполным`);
  process.exit(1);
}

// Ссылки переходов обязаны разрешиться в объекты, а не остаться строками:
// иначе стрелки на схеме есть, а вести им некуда.
const proc = (parsed.rootElement.rootElements || []).find((r) => r.$type === 'bpmn:Process');
const flows = (proc?.flowElements || []).filter((e) => e.$type === 'bpmn:SequenceFlow');
const broken = flows.filter((f) => typeof f.sourceRef !== 'object' || typeof f.targetRef !== 'object');
if (broken.length) {
  console.error('неразрешённые ссылки переходов:', broken.map((f) => f.id).join(', '));
  process.exit(1);
}

if (flags.includes('--rewrite')) {
  const out = await moddle.toXML(parsed.rootElement, { format: true });
  const to = path.replace(/\.bpmn$/, '') + '.moddle.bpmn';
  fs.writeFileSync(to, out.xml);
  console.log(`переписан модельером → ${to} (прочитай его обратно в панели)`);
}

console.log(`ок: узлов ${proc?.flowElements?.length ?? 0}, переходов ${flows.length}, предупреждений 0`);
