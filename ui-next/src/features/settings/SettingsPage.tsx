import { useState } from 'react';
import {
  AlertTriangle,
  Bell,
  Bot,
  Box,
  Cable,
  Check,
  ChevronRight,
  Coffee,
  Cpu,
  Download,
  ExternalLink,
  Info,
  Keyboard,
  Mic2,
  Monitor,
  Moon,
  Palette,
  Play,
  RefreshCw,
  Search,
  Settings2,
  Shield,
  Sparkles,
  Sun,
  Terminal,
  Volume2,
  WandSparkles,
  X,
} from 'lucide-react';
import { AnimatePresence, motion } from 'motion/react';
import type { AppSettings, Paint, Theme } from '../../core/client/types';
import { useAppStore } from '../../app/app-store';
import {
  Button,
  IconButton,
  Segmented,
  SettingRow,
  Switch,
} from '../../shared/ui/controls';

const panes = [
  ['appearance', 'Внешний вид', Palette],
  ['general', 'Основное', Settings2],
  ['stt', 'Голосовой ввод', Mic2],
  ['voice', 'Голос', Volume2],
  ['wake', 'Пробуждение', WandSparkles],
  ['notify', 'Уведомления', Bell],
  ['awake', 'Бодрость', Coffee],
  ['keys', 'Горячие клавиши', Keyboard],
  ['launch', 'Запуск', Terminal],
  ['environments', 'Виртуальные машины', Box],
  ['service', 'Под капотом', Cpu],
  ['integration', 'Интеграция', Cable],
  ['about', 'О программе', Info],
] as const;

type Pane = (typeof panes)[number][0];

export function SettingsPage() {
  const [query, setQuery] = useState('');
  const route = useAppStore((state) => state.route);
  const navigate = useAppStore((state) => state.navigate);
  const visiblePanes = panes.filter(([, label]) =>
    label.toLocaleLowerCase('ru').includes(query.trim().toLocaleLowerCase('ru')),
  );
  const pane =
    route.section === 'settings' &&
    panes.some(([id]) => id === route.pane)
      ? (route.pane as Pane)
      : 'appearance';

  return (
    <section className="settings-shell">
      <aside className="settings-sidebar">
        <label className="settings-sidebar__search">
          <Search size={15} />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Поиск настроек…"
          />
        </label>
        <div className="settings-account">
          <span>J</span>
          <div>
            <strong>Jarvis</strong>
            <small>локально · v0.4.0-next</small>
          </div>
        </div>
        <nav>
          {visiblePanes.map(([id, label, Icon]) => (
            <button
              key={id}
              className={pane === id ? 'is-active' : ''}
              onClick={() =>
                navigate({ section: 'settings', view: 'settings', pane: id })
              }
            >
              <span className={`settings-icon settings-icon--${id}`}>
                <Icon size={15} />
              </span>
              {label}
              {id === 'environments' && <em>3</em>}
            </button>
          ))}
        </nav>
      </aside>
      <div className="settings-detail">
        <AnimatePresence mode="wait" initial={false}>
          <motion.div
            className="settings-pane"
            key={pane}
            initial={{ opacity: 0, y: 5 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -3 }}
            transition={{ duration: 0.16 }}
          >
            <SettingsPane pane={pane} />
          </motion.div>
        </AnimatePresence>
      </div>
    </section>
  );
}

function SettingsPane({ pane }: { pane: Pane }) {
  if (pane === 'appearance') return <AppearancePane />;
  if (pane === 'general') return <GeneralPane />;
  if (pane === 'stt') return <SttPane />;
  if (pane === 'voice') return <VoicePane />;
  if (pane === 'wake') return <WakePane />;
  if (pane === 'notify') return <NotifyPane />;
  if (pane === 'awake') return <AwakePane />;
  if (pane === 'keys') return <KeysPane />;
  if (pane === 'launch') return <LaunchPane />;
  if (pane === 'environments') return <EnvironmentPane />;
  if (pane === 'service') return <ServicePane />;
  if (pane === 'integration') return <IntegrationPane />;
  return <AboutPane />;
}

function PaneHeader({
  eyebrow,
  title,
  body,
}: {
  eyebrow?: string;
  title: string;
  body?: string;
}) {
  return (
    <header className="settings-pane-header">
      {eyebrow && <span className="eyebrow">{eyebrow}</span>}
      <h2>{title}</h2>
      {body && <p>{body}</p>}
    </header>
  );
}

function Group({
  title,
  children,
}: {
  title?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="settings-group-wrap">
      {title && <h3>{title}</h3>}
      <div className="settings-group">{children}</div>
    </section>
  );
}

function AppearancePane() {
  const settings = useAppStore((state) => state.snapshot!.settings);
  const setTheme = useAppStore((state) => state.setTheme);
  const setPaint = useAppStore((state) => state.setPaint);
  const showError = useAppStore((state) => state.showConfigError);
  const themes: Array<{ id: Theme; label: string; icon: typeof Moon }> = [
    { id: 'dark', label: 'Тёмная', icon: Moon },
    { id: 'light', label: 'Светлая', icon: Sun },
    { id: 'midnight', label: 'Полночь', icon: Sparkles },
  ];
  const paints: Array<{ id: Paint; label: string; color: string }> = [
    { id: 'clover', label: 'Клевер', color: '#65d196' },
    { id: 'raspberry', label: 'Малина', color: '#e6537b' },
    { id: 'coal', label: 'Уголь', color: '#5f615e' },
  ];
  return (
    <>
      <PaneHeader
        eyebrow="Персонализация"
        title="Внешний вид"
        body="Тема и краска применяются ко всему приложению без перезапуска."
      />
      <Group title="Тема">
        <div className="theme-grid">
          {themes.map(({ id, label, icon: Icon }) => (
            <button
              key={id}
              className={settings.theme === id ? 'is-active' : ''}
              onClick={() => void setTheme(id)}
            >
              <span className={`theme-preview theme-preview--${id}`}>
                <i /><i /><i />
                <b />
              </span>
              <span><Icon size={14} /> {label}</span>
              {settings.theme === id && <Check size={14} />}
            </button>
          ))}
        </div>
      </Group>
      <Group title="Краска">
        <div className="paint-grid">
          {paints.map((paint) => (
            <button
              key={paint.id}
              className={settings.paint === paint.id ? 'is-active' : ''}
              onClick={() => void setPaint(paint.id)}
            >
              <i style={{ background: paint.color }} />
              <span>{paint.label}</span>
              {settings.paint === paint.id && <Check size={14} />}
            </button>
          ))}
        </div>
      </Group>
      <Group title="Проверка состояний">
        <SettingRow
          title="Показать ошибку"
          description="Имитирует реальную проблему конфигурации: banner, объяснение и безопасное исправление."
          control={
            <Button variant="danger" onClick={showError}>
              <AlertTriangle size={14} /> Показать ошибку
            </Button>
          }
        />
      </Group>
      <div className="appearance-note">
        <Monitor size={16} />
        <span>
          <strong>Системная настройка движения учитывается</strong>
          При Reduced Motion пространственные переходы заменяются мягким fade.
        </span>
      </div>
    </>
  );
}

function GeneralPane() {
  const settings = useAppStore((state) => state.snapshot!.settings);
  const update = useAppStore((state) => state.updateSettings);
  return (
    <>
      <PaneHeader title="Основное" body="Поведение окна Jarvis и локальная диагностика." />
      <Group>
        <SettingRow title="Глобальный хоткей" description="Показать или скрыть Jarvis из любого места." control={<kbd className="hotkey-cap">⌘ J</kbd>} />
        <SettingRow title="Позиция панели" description="Где появляется окно." control={<Segmented label="Позиция" value={settings.position} options={[{ value: 'center', label: 'Центр' }, { value: 'corner', label: 'Угол' }]} onChange={(position) => void update({ position })} />} />
        <SettingRow title="Запускать при старте" description="Автозапуск при входе в macOS." control={<Switch label="Автозапуск" checked={settings.openAtLogin} onChange={(openAtLogin) => void update({ openAtLogin })} />} />
        <SettingRow title="Режим логов" description="Тайминги, RAM/CPU и типы событий. Текст чатов не записывается." control={<Switch label="Диагностика" checked={settings.diagnostics} onChange={(diagnostics) => void update({ diagnostics })} />} />
      </Group>
    </>
  );
}

function SttPane() {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const update = useAppStore((state) => state.updateSettings);
  const patch = useAppStore((state) => state.patchSnapshot);
  const toast = useAppStore((state) => state.toast);
  const [testing, setTesting] = useState(false);
  return (
    <>
      <PaneHeader title="Голосовой ввод" body="Локальное распознавание речи и модели на диске." />
      <Group>
        <SettingRow title="Движок распознавания" description="Старая модель отвечает, пока загружается новая." control={<select value={snapshot.settings.sttEngine} onChange={(event) => void update({ sttEngine: event.target.value })}><option>whisper-turbo</option><option>qwen3-0.6b</option><option>qwen3-1.7b</option></select>} />
        <SettingRow title="Микрофон" description="С какого устройства записывать речь." control={<select value={snapshot.settings.microphone} onChange={(event) => void update({ microphone: event.target.value })}><option>MacBook Pro Microphone</option><option>AirPods Pro</option><option>Системный по умолчанию</option></select>} />
        <SettingRow title="Push-to-talk" description="Зажми и говори." control={<kbd className="hotkey-cap">⌥ Space</kbd>} />
        <SettingRow title="Шумодав (VAD) · альфа" description="Пропускает диктовку, если речи не слышно." control={<Switch label="VAD" checked={snapshot.settings.noiseGate} onChange={(noiseGate) => void update({ noiseGate })} />} />
        <SettingRow title="Проверить микрофон" description={testing ? 'Слушаю 4 секунды…' : 'Запишет короткий фрагмент и покажет результат.'} control={<Button onClick={() => { setTesting(true); window.setTimeout(() => { setTesting(false); toast({ kind: 'success', title: 'Микрофон работает', body: '«Проверка нового интерфейса Jarvis»' }); }, 1200); }} disabled={testing}>{testing ? <RefreshCw className="spin" size={14} /> : <Mic2 size={14} />} {testing ? 'Запись…' : 'Проверить · 4 с'}</Button>} />
      </Group>
      <Group title="Модели на диске">
        {snapshot.models.map((model) => (
          <SettingRow key={model.id} title={model.label} description={`${model.kind} · ${model.size}`} control={model.state === 'available' ? <Button onClick={() => { patch((state) => { const target = state.models.find((item) => item.id === model.id); if (target) target.state = 'downloading'; }); window.setTimeout(() => patch((state) => { const target = state.models.find((item) => item.id === model.id); if (target) target.state = 'installed'; }), 1600); }}><Download size={14} /> Скачать</Button> : model.state === 'downloading' ? <span className="download-state"><RefreshCw className="spin" size={14} /> скачиваю…</span> : <span className={`state-label ${model.state === 'active' ? 'is-active' : ''}`}>{model.state === 'active' ? 'активна' : 'на месте'}</span>} />
        ))}
      </Group>
    </>
  );
}

function VoicePane() {
  const settings = useAppStore((state) => state.snapshot!.settings);
  const update = useAppStore((state) => state.updateSettings);
  const toast = useAppStore((state) => state.toast);
  return (
    <>
      <PaneHeader title="Голос" body="Синтез речи работает локально через Silero." />
      <Group>
        <SettingRow title="Диктор" description="Голос синтеза." control={<select value={settings.speaker} onChange={(event) => void update({ speaker: event.target.value })}><option value="baya">Бая</option><option value="xenia">Ксения</option><option value="aidar">Айдар</option></select>} />
        <SettingRow title="Скорость" description="Темп речи." control={<Segmented label="Скорость" value={settings.voiceRate} options={[{ value: 'slow', label: 'Медленно' }, { value: 'medium', label: 'Обычно' }, { value: 'fast', label: 'Быстро' }, { value: 'x-fast', label: 'Очень быстро' }]} onChange={(voiceRate) => void update({ voiceRate })} />} />
        <SettingRow title="Проверить голос" description="Проиграть короткий образец." control={<Button onClick={() => toast({ kind: 'success', title: 'Jarvis говорит', body: 'Система готова к работе' })}><Play size={14} /> Тест</Button>} />
        <SettingRow title="Без звука" control={<Switch label="Mute" checked={settings.voiceMuted} onChange={(voiceMuted) => void update({ voiceMuted })} />} />
        <SettingRow title="Пауза чужого звука" description="Приглушать музыку и видео на время реплики." control={<Switch label="Ducking" checked={settings.voiceDuck} onChange={(voiceDuck) => void update({ voiceDuck })} />} />
        <SettingRow title="Только через Bluetooth" control={<Switch label="Bluetooth only" checked={settings.bluetoothOnly} onChange={(bluetoothOnly) => void update({ bluetoothOnly })} />} />
      </Group>
    </>
  );
}

function WakePane() {
  const settings = useAppStore((state) => state.snapshot!.settings);
  const update = useAppStore((state) => state.updateSettings);
  return (
    <>
      <PaneHeader title="Пробуждение" body="Фраза Hey Jarvis обрабатывается локально." />
      <Group>
        <SettingRow title="Активация по фразе" control={<Switch label="Wake word" checked={settings.wakeEnabled} onChange={(wakeEnabled) => void update({ wakeEnabled })} />} />
        <SettingRow title="Заглушить микрофон" description="Полностью отключить микрофон у источника." control={<Switch label="Mic mute" checked={settings.microphoneMuted} onChange={(microphoneMuted) => void update({ microphoneMuted })} />} />
        <SettingRow title="Порог срабатывания" description={settings.wakeThreshold.toFixed(2)} control={<input type="range" min=".2" max=".95" step=".01" value={settings.wakeThreshold} onChange={(event) => void update({ wakeThreshold: Number(event.target.value) })} />} />
        <SettingRow title="Модели openWakeWord" description="ONNX-модели Hey Jarvis." control={<span className="state-label is-active">на месте</span>} />
      </Group>
    </>
  );
}

function NotifyPane() {
  const settings = useAppStore((state) => state.snapshot!.settings);
  const update = useAppStore((state) => state.updateSettings);
  const [preview, setPreview] = useState(false);
  return (
    <>
      <PaneHeader title="Уведомления" body="Карточки завершения, вопросов и лимитов." />
      <div className="notification-preview">
        <span><Check size={14} /></span><div><strong>jarvis · агент закончил</strong><small>⎇ feat/ui-next · opus · high · сейчас</small><p>React MVP собран, проверки прошли.</p></div><Button onClick={() => setPreview((value) => !value)}>Preview</Button>
      </div>
      <Group title="Уведомлять о">
        <SettingRow title="Когда агент закончил" control={<Switch label="Done" checked={settings.notifyDone} onChange={(notifyDone) => void update({ notifyDone })} />} />
        <SettingRow title="Когда ждёт тебя" control={<Switch label="Waiting" checked={settings.notifyWaiting} onChange={(notifyWaiting) => void update({ notifyWaiting })} />} />
        <SettingRow title="Продолжать после лимита" description="Авто-«продолжай» при сбросе лимита." control={<Switch label="Auto resume" checked={settings.autoResume} onChange={(autoResume) => void update({ autoResume })} />} />
      </Group>
      <Group title="Вид и поведение">
        <SettingRow title="Позиция" control={<Segmented label="Позиция уведомлений" value={settings.notificationPosition} options={[{ value: 'center', label: 'Центр' }, { value: 'corner', label: 'Угол' }]} onChange={(notificationPosition) => void update({ notificationPosition })} />} />
        <SettingRow title="Автоскрытие" control={<Segmented label="TTL" value={settings.notificationTtl} options={[{ value: 5, label: '5с' }, { value: 8, label: '8с' }, { value: 0, label: 'Не прятать' }]} onChange={(notificationTtl) => void update({ notificationTtl })} />} />
      </Group>
      <AnimatePresence>{preview && <VoiceHudPreview onClose={() => setPreview(false)} />}</AnimatePresence>
    </>
  );
}

function VoiceHudPreview({ onClose }: { onClose: () => void }) {
  const [phase, setPhase] = useState<'listening' | 'thinking' | 'confirm'>('listening');
  return (
    <motion.div className="hud-preview" initial={{ opacity: 0, y: 10 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0, y: 8 }}>
      <span className="hud-wave"><i /><i /><i /><i /></span>
      <div><strong>{phase === 'listening' ? 'Слушаю' : phase === 'thinking' ? 'Думаю' : 'Отправить задачу?'}</strong><small>Voice HUD · интерактивный preview</small></div>
      {phase === 'listening' && <Button onClick={() => setPhase('thinking')}>Готово</Button>}
      {phase === 'thinking' && <Button onClick={() => setPhase('confirm')}>Продолжить</Button>}
      {phase === 'confirm' && <><Button variant="primary" onClick={onClose}>Да</Button><Button onClick={onClose}>Отмена</Button></>}
      <IconButton label="Закрыть" onClick={onClose}><X size={14} /></IconButton>
    </motion.div>
  );
}

function AwakePane() {
  const settings = useAppStore((state) => state.snapshot!.settings);
  const update = useAppStore((state) => state.updateSettings);
  return (
    <>
      <PaneHeader title="Бодрость" body="Управление сном Mac во время работы агентов." />
      <Group>
        <SettingRow title="Не спать" control={<Segmented label="Keep awake" value={settings.keepAwake} options={[{ value: 'off', label: 'Выкл' }, { value: '15m', label: '15м' }, { value: '1h', label: '1ч' }, { value: '4h', label: '4ч' }, { value: 'inf', label: '∞' }]} onChange={(keepAwake) => void update({ keepAwake })} />} />
        <SettingRow title="Держать, пока работают агенты" control={<Switch label="Agent keep awake" checked={settings.keepWhileAgentsRun} onChange={(keepWhileAgentsRun) => void update({ keepWhileAgentsRun })} />} />
        <SettingRow title="Не гасить экран" control={<Switch label="Display awake" checked={settings.keepDisplayOn} onChange={(keepDisplayOn) => void update({ keepDisplayOn })} />} />
        <SettingRow title="С закрытой крышкой" description="Требует питания от сети." control={<Segmented label="Clamshell" value={settings.clamshell} options={[{ value: 'sleep', label: 'Спать' }, { value: 'keep', label: 'Не спать' }]} onChange={(clamshell) => void update({ clamshell })} />} />
      </Group>
    </>
  );
}

function KeysPane() {
  const hotkeys = useAppStore((state) => state.snapshot!.hotkeys);
  const patch = useAppStore((state) => state.patchSnapshot);
  const [recording, setRecording] = useState<string | null>(null);
  return (
    <>
      <PaneHeader title="Горячие клавиши" body="Кликни сочетание и нажми новые клавиши." />
      {(['panel', 'sound', 'voice'] as const).map((group) => (
        <Group key={group} title={group === 'panel' ? 'Панель и сессии' : group === 'sound' ? 'Звук и уведомления' : 'Голос'}>
          {hotkeys.filter((item) => item.group === group).map((binding) => (
            <SettingRow key={binding.id} title={binding.label} control={<div className="hotkey-control"><button className={`hotkey-cap ${recording === binding.id ? 'is-recording' : ''}`} onClick={() => setRecording(binding.id)}>{recording === binding.id ? 'нажми сочетание…' : binding.accel}</button><IconButton label="Сбросить" onClick={() => patch((state) => { const target = state.hotkeys.find((item) => item.id === binding.id); if (target) target.accel = target.defaultAccel; })}><RefreshCw size={13} /></IconButton></div>} />
          ))}
        </Group>
      ))}
      {recording && <div className="hotkey-conflict"><AlertTriangle size={14} /><span>Демо записи: сочетание ⌘J уже занято панелью.</span><Button onClick={() => { patch((state) => { const target = state.hotkeys.find((item) => item.id === recording); if (target) target.accel = '⌘ ⇧ J'; }); setRecording(null); }}>Перехватить ⌘⇧J</Button></div>}
    </>
  );
}

function LaunchPane() {
  const settings = useAppStore((state) => state.snapshot!.settings);
  const update = useAppStore((state) => state.updateSettings);
  return (
    <>
      <PaneHeader title="Запуск" body="Как и где открывать новые сессии." />
      <Group>
        <SettingRow title="Терминал" description="Кастомная команда использует плейсхолдер {cmd}." control={<select value={settings.launchTerminal} onChange={(event) => void update({ launchTerminal: event.target.value as AppSettings['launchTerminal'] })}><option value="terminal-app">Terminal.app</option><option value="iterm2">iTerm2</option><option value="custom">Кастомная команда</option></select>} />
        {settings.launchTerminal === 'custom' && <SettingRow title="Шаблон команды" control={<input value={settings.launchCustomCommand} onChange={(event) => void update({ launchCustomCommand: event.target.value })} placeholder="ghostty -e bash -lc {cmd}" />} />}
        <SettingRow title="Команда прокси" description="Выполняется перед запуском агента, не связана с egress proxy." control={<input value={settings.launchProxyCommand} onChange={(event) => void update({ launchProxyCommand: event.target.value })} placeholder="export HTTPS_PROXY=http://…" />} />
        <SettingRow danger title="Опасный режим" description="Claude skip permissions и Codex YOLO. Глобально для обоих агентов." control={<Switch label="Dangerous mode" checked={settings.launchDangerous} onChange={(launchDangerous) => void update({ launchDangerous })} />} />
      </Group>
    </>
  );
}

function EnvironmentPane() {
  const settings = useAppStore((state) => state.snapshot!.settings);
  const navigate = useAppStore((state) => state.navigate);
  const update = useAppStore((state) => state.updateSettings);
  return (
    <>
      <PaneHeader title="Виртуальные машины" body="Память, ресурсы и поведение новых машин." />
      <Group title="Память">
        <SettingRow title="Память проектов Claude" description="Каждая VM видит память только своего проекта." control={<Segmented label="Memory scope" value={settings.memoryScope} options={[{ value: 'project', label: 'Только свой' }, { value: 'all', label: 'Все' }]} onChange={(memoryScope) => void update({ memoryScope })} />} />
        <SettingRow title="Память Codex" description="MEMORY.md, записи и выжимки сессий · 768 КБ." control={<span className="state-label is-active">целиком</span>} />
      </Group>
      <Group title="Новые машины">
        <SettingRow title="Процессоры" control={<Segmented label="CPU" value={settings.vmCpus} options={[{ value: 2, label: '2' }, { value: 4, label: '4' }, { value: 8, label: '8' }]} onChange={(vmCpus) => void update({ vmCpus })} />} />
        <SettingRow title="Память" control={<Segmented label="RAM" value={settings.vmMemory} options={[{ value: 4, label: '4 ГиБ' }, { value: 8, label: '8 ГиБ' }, { value: 16, label: '16 ГиБ' }]} onChange={(vmMemory) => void update({ vmMemory })} />} />
        <SettingRow title="Диск" description="Занимает место по мере роста." control={<span className="state-label">{settings.vmDisk} ГиБ</span>} />
      </Group>
      <Group title="Поведение">
        <SettingRow title="Поднимать закреплённые VM при старте" control={<Switch label="VM autostart" checked={settings.vmAutostart} onChange={(vmAutostart) => void update({ vmAutostart })} />} />
        <SettingRow title="Останавливать простаивающие" description="Остановка убивает dev-серверы внутри VM." control={<Segmented label="Idle stop" value={settings.vmIdleStop} options={[{ value: 'off', label: 'Никогда' }, { value: '30', label: '30 мин' }, { value: '120', label: '2 ч' }]} onChange={(vmIdleStop) => void update({ vmIdleStop })} />} />
        <SettingRow title="Все виртуальные машины" description="Статусы, файлы, mounts и кэш." control={<Button variant="primary" onClick={() => navigate({ section: 'projects', view: 'environments' })}>Открыть список <ChevronRight size={14} /></Button>} />
      </Group>
    </>
  );
}

function ServicePane() {
  const settings = useAppStore((state) => state.snapshot!.settings);
  const update = useAppStore((state) => state.updateSettings);
  const toast = useAppStore((state) => state.toast);
  const [testing, setTesting] = useState(false);
  return (
    <>
      <PaneHeader title="Под капотом" body="Служебный LLM для сводок, заголовков и голосового плана." />
      <Group>
        <SettingRow title="Бэкенд служебного LLM" description="Авто: Claude Haiku → Codex с fallback." control={<Segmented label="Service backend" value={settings.serviceBackend} options={[{ value: 'auto', label: 'Авто' }, { value: 'claude', label: 'Claude' }, { value: 'codex', label: 'Codex' }]} onChange={(serviceBackend) => void update({ serviceBackend })} />} />
        <SettingRow title="Доступно" description="claude ✓ · codex ✓ · Codex-SDK ✓" control={<span className="state-label is-active">online</span>} />
        <SettingRow title="Проверить ответ" description="Короткий запрос через выбранный backend." control={<Button onClick={() => { setTesting(true); window.setTimeout(() => { setTesting(false); toast({ kind: 'success', title: 'Backend ответил', body: 'Claude Haiku · 0.8 с' }); }, 900); }} disabled={testing}>{testing ? <RefreshCw className="spin" size={14} /> : <Sparkles size={14} />} {testing ? 'Тестирую…' : 'Протестировать'}</Button>} />
      </Group>
      <Group title="Сеть">
        <SettingRow title="Egress-прокси" description="Применяется к служебным вызовам Claude и Codex." control={<input value={settings.serviceProxy} onChange={(event) => void update({ serviceProxy: event.target.value })} placeholder="из окружения процесса" />} />
      </Group>
      <Group title="Аккаунт Claude">
        <SettingRow title="Подписка" description="Служебные вызовы используют setup-token." control={<span className="state-label is-active">подключено</span>} />
        <SettingRow title="Управление" control={<Button onClick={() => toast({ kind: 'warning', title: 'Claude отключён в mock-режиме', body: 'В приложении здесь будет отзыв setup-token.' })}>Отключить</Button>} />
      </Group>
      <Group title="Codex (Python SDK)">
        <SettingRow title="Модель Codex" control={<select value={settings.codexModel} onChange={(event) => void update({ codexModel: event.target.value })}><option>gpt-5.3-codex</option><option>gpt-5.3-codex-spark</option></select>} />
        <SettingRow title="Глубина рассуждений" control={<Segmented label="Codex effort" value={settings.codexEffort} options={[{ value: 'low', label: 'low' }, { value: 'medium', label: 'medium' }, { value: 'high', label: 'high' }]} onChange={(codexEffort) => void update({ codexEffort })} />} />
        <SettingRow title="Codex-SDK сайдкар" description="Авторизация через существующий codex login." control={<span className="state-label is-active">на месте</span>} />
      </Group>
    </>
  );
}

function IntegrationPane() {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const update = useAppStore((state) => state.updateSettings);
  const patch = useAppStore((state) => state.patchSnapshot);
  const [onboarding, setOnboarding] = useState(false);
  const [armed, setArmed] = useState(false);
  return (
    <>
      <PaneHeader title="Интеграция" body="Claude Code hooks, shim и локальный транспорт." />
      <Group title="Claude Code · подключено">
        {[
          ['Хуки событий', snapshot.integration.hooks],
          ['Шим запуска claude', snapshot.integration.shim],
          ['tmux-транспорт', snapshot.integration.tmux],
          ['PATH-блок в shell', snapshot.integration.path],
        ].map(([label, ok]) => <SettingRow key={String(label)} title={String(label)} control={<span className={`state-label ${ok ? 'is-active' : ''}`}>{ok ? 'есть' : '—'}</span>} />)}
      </Group>
      <Group title="Разработчик">
        <SettingRow title="Тихий режим" description="Копить статистику без тостов, голоса и показа панели." control={<Switch label="Quiet" checked={snapshot.settings.quietMode} onChange={(quietMode) => void update({ quietMode })} />} />
      </Group>
      <Group title="Управление и диск">
        <SettingRow title="Переустановить интеграцию" description="Обновить хуки, шим и транспорт." control={<Button onClick={() => setOnboarding(true)}>Переустановить</Button>} />
        <SettingRow danger title="Удалить интеграцию" description={`${snapshot.integration.foreignHooks} чужих хука будут сохранены.`} control={<Button variant="danger" onClick={() => { if (!armed) { setArmed(true); return; } patch((state) => { state.integration.hooks = false; state.integration.shim = false; state.integration.tmux = false; state.integration.path = false; }); setArmed(false); }}>{armed ? 'Точно удалить?' : 'Удалить'}</Button>} />
      </Group>
      <AnimatePresence>{onboarding && <OnboardingPreview onClose={() => setOnboarding(false)} />}</AnimatePresence>
    </>
  );
}

function OnboardingPreview({ onClose }: { onClose: () => void }) {
  const [step, setStep] = useState(0);
  const steps = [
    ['Старт', 'Проверю конфигурацию и фактическую готовность системы.'],
    ['Связь', 'Hooks, shim и tmux-транспорт готовы к установке.'],
    ['Модули', 'Whisper, Silero и wake-word можно загрузить сейчас или позже.'],
    ['Online', 'Jarvis готов. Открой панель сочетанием ⌘J.'],
  ];
  return (
    <motion.div className="modal-scrim" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} onClick={onClose}>
      <motion.div className="onboarding-preview" initial={{ scale: .97, y: 12 }} animate={{ scale: 1, y: 0 }} onClick={(event) => event.stopPropagation()}>
        <aside>{steps.map(([title], index) => <button key={title} className={step === index ? 'is-active' : index < step ? 'is-done' : ''} onClick={() => setStep(index)}><span>{index < step ? <Check size={12} /> : index + 1}</span>{title}</button>)}</aside>
        <main><span className="brand-orbit">{step === 3 ? <Check size={24} /> : 'J'}</span><span className="eyebrow">Private · local</span><h2>{steps[step]?.[0]}</h2><p>{steps[step]?.[1]}</p><div className="onboarding-readiness"><span><Shield size={14} /> конфигурация</span><span><Cable size={14} /> транспорт</span><span><Bot size={14} /> агенты</span></div><footer>{step > 0 && <Button onClick={() => setStep(step - 1)}>Назад</Button>}<Button variant="primary" onClick={() => step === steps.length - 1 ? onClose() : setStep(step + 1)}>{step === steps.length - 1 ? 'Открыть Jarvis' : 'Продолжить'}</Button></footer></main>
        <IconButton label="Закрыть" onClick={onClose}><X size={15} /></IconButton>
      </motion.div>
    </motion.div>
  );
}

function AboutPane() {
  const toast = useAppStore((state) => state.toast);
  return (
    <>
      <PaneHeader title="О программе" body="Jarvis · локальный ассистент для Claude Code и Codex." />
      <Group>
        <SettingRow title="Версия" description="UI Next prototype · browser mock" control={<span className="state-label is-active">v0.4.0-next</span>} />
        <SettingRow title="Обновления" description="Проверить GitHub Releases и перезапустить приложение." control={<Button onClick={() => toast({ kind: 'success', title: 'Установлена свежая версия' })}><RefreshCw size={14} /> Проверить</Button>} />
        <SettingRow title="Уровни усилия" description="minimal · low · medium · high · max" control={<span className="state-label">5 режимов</span>} />
        <SettingRow title="Лицензии" description="Код MIT; веса локальных моделей имеют отдельные лицензии." control={<Button onClick={() => toast({ kind: 'info', title: 'Лицензии откроются в системном браузере', body: 'Для browser MVP внешнее действие заменено уведомлением.' })}><ExternalLink size={13} /> Открыть</Button>} />
      </Group>
      <div className="about-mark"><span>J</span><strong>Данные остаются на этом Mac</strong><small>Typed UI prototype · August 2026</small></div>
    </>
  );
}
