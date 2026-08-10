import { useMemo, useState } from 'react';
import {
  BarChart3,
  BookOpenText,
  Check,
  Clipboard,
  FilePenLine,
  Flame,
  History,
  Plus,
  RefreshCw,
  Search,
  Sparkles,
  Trash2,
  WandSparkles,
  X,
} from 'lucide-react';
import { motion } from 'motion/react';
import { useAppStore } from '../../app/app-store';
import { Button, Switch } from '../../shared/ui/controls';

type VoicePane = 'history' | 'insights' | 'dictionary' | 'transforms' | 'scratch';

const nav = [
  ['history', 'История', History],
  ['insights', 'Статистика', BarChart3],
  ['dictionary', 'Словарь', BookOpenText],
  ['transforms', 'Преобразования', WandSparkles],
  ['scratch', 'Черновик', FilePenLine],
] as const;

export function VoicePage() {
  const [pane, setPane] = useState<VoicePane>('history');
  return (
    <section className="voice-shell">
      <aside className="voice-sidebar">
        <div><span className="eyebrow">Jisper</span><h1>Голос</h1><p>Локальная история диктовки</p></div>
        <nav>
          {nav.map(([id, label, Icon]) => (
            <button key={id} className={pane === id ? 'is-active' : ''} onClick={() => setPane(id)}>
              <Icon size={15} /><span>{label}</span>
            </button>
          ))}
        </nav>
        <VoiceRail />
      </aside>
      <div className="voice-content">
        {pane === 'history' && <HistoryPane />}
        {pane === 'insights' && <InsightsPane />}
        {pane === 'dictionary' && <DictionaryPane />}
        {pane === 'transforms' && <TransformsPane />}
        {pane === 'scratch' && <ScratchPane />}
      </div>
    </section>
  );
}

function VoiceRail() {
  const transcripts = useAppStore((state) => state.snapshot!.transcripts);
  const words = transcripts.reduce((sum, item) => sum + item.words, 0);
  return <div className="voice-rail"><span><strong>{words}</strong>слов</span><span><strong>{transcripts.length}</strong>записи</span><span><i />локально</span></div>;
}

function HistoryPane() {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const patch = useAppStore((state) => state.patchSnapshot);
  const toast = useAppStore((state) => state.toast);
  const search = useAppStore((state) => state.search).toLowerCase();
  const [transformFor, setTransformFor] = useState<string | null>(null);
  const items = snapshot.transcripts.filter((item) => item.text.toLowerCase().includes(search));
  return (
    <div className="voice-pane">
      <header><span className="eyebrow">Последние записи</span><h2>История диктовки</h2><p>Исходный текст сохраняется вместе с преобразованным.</p></header>
      <div className="voice-history-list">
        {items.map((item) => (
          <article className="voice-entry" key={item.id}>
            <header><time>{new Date(item.createdAt).toLocaleTimeString('ru', { hour: '2-digit', minute: '2-digit' })}</time><span>{item.words} слов</span><button onClick={() => patch((state) => { state.transcripts = state.transcripts.filter((current) => current.id !== item.id); })}><Trash2 size={13} /></button></header>
            <p>{item.text}</p>
            {item.enhanced && <div className="enhanced-text"><span><Sparkles size={12} /> {item.appliedStyle}</span><p>{item.enhanced}</p></div>}
            <footer>
              <Button onClick={() => setTransformFor(transformFor === item.id ? null : item.id)}><WandSparkles size={13} /> Преобразовать</Button>
              <Button onClick={() => toast({ kind: 'success', title: 'Текст скопирован' })}><Clipboard size={13} /> Копировать</Button>
              {item.enhanced && <Button onClick={() => toast({ kind: 'info', title: 'Перегенерирую текст' })}><RefreshCw size={13} /></Button>}
            </footer>
            {transformFor === item.id && <div className="transform-menu">{snapshot.transforms.filter((transform) => transform.enabled).map((transform) => <button key={transform.id} onClick={() => { patch((state) => { const target = state.transcripts.find((current) => current.id === item.id); if (target) { target.enhanced = `${transform.name}: ${target.text.replace('крч', 'короче')}`; target.appliedStyle = transform.name; } }); setTransformFor(null); }}>{transform.name}<small>{transform.description}</small></button>)}</div>}
          </article>
        ))}
      </div>
    </div>
  );
}

function InsightsPane() {
  const transcripts = useAppStore((state) => state.snapshot!.transcripts);
  const words = transcripts.reduce((sum, item) => sum + item.words, 0);
  return (
    <div className="voice-pane">
      <header><span className="eyebrow">Речь в цифрах</span><h2>Статистика диктовки</h2><p>Активность и объём только на этом Mac.</p></header>
      <div className="voice-big-stats"><div><strong>{words}</strong><span>всего слов</span></div><div><strong>{transcripts.length}</strong><span>записей</span></div><div><strong>3</strong><span>активных дня</span></div></div>
      <section className="heatmap-card"><header><Flame size={16} /><strong>Ритм за 12 недель</strong></header><div className="heatmap">{Array.from({ length: 84 }, (_, index) => <motion.i key={index} initial={{ opacity: 0 }} animate={{ opacity: 0.18 + ((index * 7) % 10) / 12 }} transition={{ delay: index * 0.004 }} />)}</div></section>
    </div>
  );
}

function DictionaryPane() {
  const words = useAppStore((state) => state.snapshot!.dictionary);
  const patch = useAppStore((state) => state.patchSnapshot);
  const [value, setValue] = useState('');
  const add = () => { if (!value.trim()) return; patch((state) => state.dictionary.push({ id: crypto.randomUUID(), word: value.trim(), note: 'добавлено вручную' })); setValue(''); };
  return (
    <div className="voice-pane">
      <header><span className="eyebrow">Свои слова</span><h2>Словарь</h2><p>Названия, технологии и имена, которые нужно распознавать точно.</p></header>
      <div className="inline-add"><input value={value} onChange={(event) => setValue(event.target.value)} onKeyDown={(event) => event.key === 'Enter' && add()} placeholder="Например: Tauri, Haiku, openWakeWord…" /><Button variant="primary" onClick={add}><Plus size={14} /> Добавить</Button></div>
      <div className="dictionary-list">{words.map((word) => <div key={word.id}><span><strong>{word.word}</strong><small>{word.note}</small></span><button onClick={() => patch((state) => { state.dictionary = state.dictionary.filter((item) => item.id !== word.id); })}><X size={14} /></button></div>)}</div>
    </div>
  );
}

function TransformsPane() {
  const transforms = useAppStore((state) => state.snapshot!.transforms);
  const patch = useAppStore((state) => state.patchSnapshot);
  const [smart, setSmart] = useState(true);
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  return (
    <div className="voice-pane">
      <header><span className="eyebrow">После распознавания</span><h2>Преобразования</h2><p>Jarvis может выбрать стиль автоматически или по команде.</p></header>
      <div className="smart-transform"><span><Sparkles size={16} /><div><strong>Умный выбор</strong><small>Подбирает преобразование по контексту</small></div></span><Switch label="Умный выбор" checked={smart} onChange={setSmart} /></div>
      <div className="transform-list">{transforms.map((transform) => <div key={transform.id}><WandSparkles size={15} /><span><strong>{transform.name}</strong><small>{transform.description}</small></span><Switch label={transform.name} checked={transform.enabled} onChange={(enabled) => patch((state) => { const target = state.transforms.find((item) => item.id === transform.id); if (target) target.enabled = enabled; })} /></div>)}</div>
      <div className="new-transform"><h3>Новое преобразование</h3><input value={name} onChange={(event) => setName(event.target.value)} placeholder="Название" /><input value={description} onChange={(event) => setDescription(event.target.value)} placeholder="Короткое описание" /><Button onClick={() => { if (!name.trim()) return; patch((state) => state.transforms.push({ id: crypto.randomUUID(), name, description, enabled: true })); setName(''); setDescription(''); }}><Plus size={14} /> Добавить</Button></div>
    </div>
  );
}

function ScratchPane() {
  const value = useAppStore((state) => state.snapshot!.scratchpad);
  const patch = useAppStore((state) => state.patchSnapshot);
  const toast = useAppStore((state) => state.toast);
  return (
    <div className="voice-pane scratch-pane">
      <header><span className="eyebrow">Автосохранение</span><h2>Черновик</h2><p>Собирай надиктованные куски до отправки агенту.</p></header>
      <textarea value={value} onChange={(event) => patch((state) => { state.scratchpad = event.target.value; })} />
      <footer><span><Check size={13} /> сохранено локально</span><span>{value.length} символов</span><Button onClick={() => toast({ kind: 'success', title: 'Черновик скопирован' })}><Clipboard size={13} /> Копировать</Button></footer>
    </div>
  );
}
