import { useEffect, useMemo, useRef, useState } from 'react';
import {
  ArrowLeft,
  Bot,
  Box,
  Check,
  ChevronDown,
  ChevronRight,
  CircleStop,
  Clock3,
  Copy,
  Cpu,
  File,
  Folder,
  FolderOpen,
  HardDrive,
  MemoryStick,
  MoreHorizontal,
  Paperclip,
  Play,
  Plus,
  RefreshCw,
  Search,
  Send,
  Settings2,
  ShieldCheck,
  Terminal,
  Trash2,
  X,
} from 'lucide-react';
import { AnimatePresence, motion } from 'motion/react';
import type {
  Agent,
  FileNode,
  Project,
  VirtualMachine,
  VmState,
} from '../../core/client/types';
import { useAppStore } from '../../app/app-store';
import {
  Button,
  EmptyState,
  IconButton,
  Segmented,
  SelectionMark,
  StatusDot,
  Switch,
} from '../../shared/ui/controls';

const vmLabels: Record<VmState, string> = {
  running: 'VM работает',
  booting: 'VM поднимается',
  stopped: 'VM спит',
  error: 'Ошибка VM',
  none: 'Без VM',
};

function ProjectHeader({
  project,
  kind,
  onBack,
  children,
}: {
  project: Project;
  kind: string;
  onBack: () => void;
  children?: React.ReactNode;
}) {
  return (
    <header className="page-header page-header--compact">
      <IconButton label="Назад · Esc" onClick={onBack}>
        <ArrowLeft size={17} />
      </IconButton>
      <div className="crumb-title">
        <span>{kind}</span>
        <h1>{project.name}</h1>
      </div>
      <div className="page-header__spacer" />
      {children}
    </header>
  );
}

export function ProjectsFeature() {
  const route = useAppStore((state) => state.route);
  if (route.section !== 'projects') return null;
  if (route.view === 'history') return <ProjectHistory projectId={route.projectId} />;
  if (route.view === 'workspace') return <VmWorkspace projectId={route.projectId} />;
  if (route.view === 'environments') return <EnvironmentList />;
  if (route.view === 'environment-files')
    return <EnvironmentFiles vmId={route.vmId} />;
  return <ProjectCatalog />;
}

function ProjectCatalog() {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const search = useAppStore((state) => state.search).toLowerCase();
  const navigate = useAppStore((state) => state.navigate);
  const projects = snapshot.projects.filter((project) =>
    `${project.name} ${project.path}`.toLowerCase().includes(search),
  );

  return (
    <section className="page">
      <header className="page-header">
        <div>
          <span className="eyebrow">Рабочий контекст</span>
          <h1>Проекты</h1>
        </div>
        <div className="page-header__spacer" />
        <Button
          onClick={() =>
            navigate({ section: 'projects', view: 'environments' })
          }
        >
          <Box size={15} />
          Управление VM
        </Button>
      </header>
      <div className="entity-list line-free-list">
        {projects.map((project, index) => (
          <motion.button
            type="button"
            className="project-row line-free-row"
            key={project.id}
            onClick={() =>
              navigate({
                section: 'projects',
                view: 'history',
                projectId: project.id,
              })
            }
            initial={{ opacity: 0, y: 6 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ delay: index * 0.025 }}
          >
            <span className="project-mark">
              {project.name.slice(0, 1).toUpperCase()}
            </span>
            <span className="project-copy">
              <strong>{project.name}</strong>
              <small>{project.path}</small>
            </span>
            <span className={`vm-badge vm-badge--${project.vmState}`}>
              <StatusDot
                status={
                  project.vmState === 'running'
                    ? 'active'
                    : project.vmState === 'error'
                      ? 'error'
                      : 'idle'
                }
                pulse={project.vmState === 'running'}
              />
              {vmLabels[project.vmState]}
            </span>
            <span className="row-meta">{project.chats.length} чатов</span>
            <ChevronRight size={16} />
          </motion.button>
        ))}
      </div>
    </section>
  );
}

function ProjectHistory({ projectId }: { projectId: string }) {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const navigate = useAppStore((state) => state.navigate);
  const toast = useAppStore((state) => state.toast);
  const client = useAppStore((state) => state.client);
  const refresh = useAppStore((state) => state.refresh);
  const project = snapshot.projects.find((item) => item.id === projectId);
  const [menuOpen, setMenuOpen] = useState(false);
  const [agent, setAgent] = useState<Agent>('claude');
  const [place, setPlace] = useState<'mac' | 'vm'>('mac');
  if (!project) return null;

  const launch = async () => {
    await client.launchSession(project.id, agent, place === 'vm');
    await refresh();
    setMenuOpen(false);
    if (place === 'vm') {
      navigate({ section: 'projects', view: 'workspace', projectId });
    } else {
      toast({
        kind: 'success',
        title: `Новый чат · ${agent === 'claude' ? 'Claude' : 'Codex'}`,
        body: 'Команда запуска подготовлена для Terminal.app',
      });
    }
  };

  const days = [...new Set(project.chats.map((chat) => chat.day))];

  return (
    <section className="page">
      <ProjectHeader
        project={project}
        kind="проект"
        onBack={() => navigate({ section: 'projects', view: 'catalog' })}
      >
        <div className="split-action">
          <Button variant="primary" onClick={() => void launch()}>
            <Plus size={15} /> Новый чат
          </Button>
          <IconButton
            label="Выбрать агента и место"
            aria-expanded={menuOpen}
            onClick={() => setMenuOpen((value) => !value)}
          >
            <ChevronDown size={15} />
          </IconButton>
          <AnimatePresence>
            {menuOpen && (
              <motion.div
                className="launch-menu popover"
                initial={{ opacity: 0, y: -6, scale: 0.98 }}
                animate={{ opacity: 1, y: 0, scale: 1 }}
                exit={{ opacity: 0, y: -4, scale: 0.98 }}
              >
                <span className="popover-label">Агент</span>
                {(['claude', 'codex'] as const).map((item) => (
                  <button key={item} onClick={() => setAgent(item)}>
                    <Bot size={15} />
                    <span className="launch-menu__copy">
                      <strong>{item === 'claude' ? 'Claude' : 'Codex'}</strong>
                      <small>{item === agent ? 'в прошлый раз' : 'доступен'}</small>
                    </span>
                    <SelectionMark selected={item === agent} />
                  </button>
                ))}
                <span className="popover-label">Где выполнять</span>
                {(['mac', 'vm'] as const).map((item) => (
                  <button key={item} onClick={() => setPlace(item)}>
                    {item === 'mac' ? <Terminal size={15} /> : <Box size={15} />}
                    <span className="launch-menu__copy">
                      <strong>
                        {item === 'mac' ? 'На этом Mac' : 'В Agent VM'}
                      </strong>
                      <small>
                        {item === 'mac'
                          ? 'видит твои файлы'
                          : project.vmState === 'running'
                            ? 'VM уже работает'
                            : 'поднимется сама'}
                      </small>
                    </span>
                    <SelectionMark selected={item === place} />
                  </button>
                ))}
                <button
                  onClick={() =>
                    navigate({ section: 'projects', view: 'workspace', projectId })
                  }
                >
                  <Box size={15} />
                  <span className="launch-menu__copy">
                    <strong>Открыть рабочее место VM</strong>
                    <small>терминал, файлы и управление</small>
                  </span>
                  <ChevronRight size={14} />
                </button>
              </motion.div>
            )}
          </AnimatePresence>
        </div>
        <button
          className={`vm-status-button vm-status-button--${project.vmState}`}
          onClick={() =>
            navigate({ section: 'projects', view: 'workspace', projectId })
          }
        >
          <StatusDot
            status={project.vmState === 'running' ? 'active' : 'idle'}
            pulse={project.vmState === 'running'}
          />
          {vmLabels[project.vmState]}
          <ChevronRight size={14} />
        </button>
      </ProjectHeader>

      <div className="dated-list line-free-list">
        {days.map((day) => (
          <div className="date-group" key={day}>
            <span className="date-separator line-free-separator">{day}</span>
            {project.chats
              .filter((chat) => chat.day === day)
              .map((chat) => (
                <button
                  className="chat-history-row line-free-row"
                  key={chat.id}
                  onClick={() => {
                    if (chat.run.startsWith('vm')) {
                      navigate({
                        section: 'projects',
                        view: 'workspace',
                        projectId,
                      });
                    } else {
                      toast({
                        kind: 'info',
                        title: 'Продолжаю чат в терминале',
                        body: `${chat.agent} · ${project.path}`,
                      });
                    }
                  }}
                >
                  <span className={`history-icon history-icon--${chat.run}`}>
                    {chat.run === 'terminal' ? (
                      <Terminal size={14} />
                    ) : chat.run === 'vm-working' ? (
                      <motion.span
                        animate={{ rotate: 360 }}
                        transition={{ repeat: Infinity, duration: 4, ease: 'linear' }}
                      >
                        <RefreshCw size={14} />
                      </motion.span>
                    ) : (
                      <Box size={14} />
                    )}
                  </span>
                  <span>
                    <strong>{chat.title}</strong>
                    {chat.detail && <small>{chat.detail}</small>}
                  </span>
                  <span className="agent-label">{chat.agent}</span>
                  <time>{chat.when}</time>
                </button>
              ))}
          </div>
        ))}
      </div>
    </section>
  );
}

function VmWorkspace({ projectId }: { projectId: string }) {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const navigate = useAppStore((state) => state.navigate);
  const client = useAppStore((state) => state.client);
  const refresh = useAppStore((state) => state.refresh);
  const toast = useAppStore((state) => state.toast);
  const project = snapshot.projects.find((item) => item.id === projectId);
  const vm = snapshot.vms.find((item) => item.projectId === projectId);
  const [panelOpen, setPanelOpen] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);
  const [confirmStop, setConfirmStop] = useState(false);
  const [text, setText] = useState('');
  const [attachments, setAttachments] = useState<string[]>([]);
  const [bootStage, setBootStage] = useState(0);
  const bootTimer = useRef<number | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const bootLines = [
    'Собираю виртуальную машину',
    'Раскладываю образ системы',
    'Прокладываю доступ к файлам проекта',
    'Переношу настройки и память',
  ];

  useEffect(() => {
    if (project?.vmState !== 'booting' || !vm) return;
    bootTimer.current = window.setInterval(
      () => setBootStage((stage) => (stage + 1) % bootLines.length),
      1500,
    );
    const ready = window.setTimeout(async () => {
      await client.setVmState(vm.id, 'running');
      await refresh();
      toast({ kind: 'success', title: 'Машина готова', body: vm.name });
    }, 6200);
    return () => {
      if (bootTimer.current) window.clearInterval(bootTimer.current);
      window.clearTimeout(ready);
    };
  }, [project?.vmState, vm?.id]);

  if (!project) return null;

  const start = async () => {
    if (!vm) return;
    await client.setVmState(vm.id, 'booting');
    await refresh();
  };
  const stop = async () => {
    if (!vm) return;
    await client.setVmState(vm.id, 'stopped');
    await refresh();
    setConfirmStop(false);
    toast({ kind: 'info', title: 'VM выключена', body: 'Диск и файлы сохранены' });
  };
  const submit = async () => {
    if (!text.trim() && !attachments.length) return;
    if (project.vmState !== 'running') await start();
    setText('');
    setAttachments([]);
    toast({
      kind: 'success',
      title: 'Задача поставлена',
      body:
        project.vmState === 'running'
          ? 'Агент получил сообщение'
          : 'Отправится после запуска машины',
    });
  };

  return (
    <section className="page vm-workspace">
      <ProjectHeader
        project={project}
        kind="рабочее место"
        onBack={() =>
          navigate({ section: 'projects', view: 'history', projectId })
        }
      >
        <button
          className={`vm-status-button vm-status-button--${project.vmState}`}
          onClick={() => setPanelOpen((value) => !value)}
          aria-expanded={panelOpen}
        >
          <StatusDot
            status={
              project.vmState === 'running'
                ? 'active'
                : project.vmState === 'error'
                  ? 'error'
                  : 'idle'
            }
            pulse={
              project.vmState === 'running' || project.vmState === 'booting'
            }
          />
          {vmLabels[project.vmState]}
          <ChevronDown size={14} />
        </button>
      </ProjectHeader>

      <AnimatePresence>
        {panelOpen && vm && (
          <VmControlPanel
            vm={vm}
            moreOpen={moreOpen}
            onMore={() => setMoreOpen((value) => !value)}
            onStart={() => void start()}
            onStop={() => setConfirmStop(true)}
            onFiles={() =>
              navigate({
                section: 'projects',
                view: 'environment-files',
                vmId: vm.id,
              })
            }
          />
        )}
      </AnimatePresence>

      <div className="workspace-body" style={{ gridRow: 3 }}>
        {project.vmState === 'running' && vm ? (
          <AgentWorkspaceFeed vm={vm} />
        ) : project.vmState === 'booting' ? (
          <VmStateScene
            state="booting"
            title="Готовлю машину проекта"
            body={
              bootLines[bootStage] ??
              bootLines[0] ??
              'Подготавливаю виртуальную машину'
            }
            detail={`${(bootStage + 1) * 14} с · этап ${bootStage + 1} из 4`}
          />
        ) : project.vmState === 'error' ? (
          <VmStateScene
            state="error"
            title="Машина не поднялась"
            body="Достигнут безопасный предел: одновременно может работать не больше четырёх машин."
            action={
              <>
                <Button variant="primary" onClick={() => void start()}>
                  Попробовать снова
                </Button>
                <Button
                  onClick={() =>
                    navigate({ section: 'projects', view: 'environments' })
                  }
                >
                  Показать машины
                </Button>
              </>
            }
            detail="попытка заняла 2 мин 07 с"
          />
        ) : (
          <VmStateScene
            state="stopped"
            title="Машина спит"
            body="Она поднимется сама, когда отправишь первое сообщение. Диск и файлы проекта уже на месте."
            action={
              <Button variant="primary" onClick={() => void start()}>
                <Play size={14} /> Поднять машину
              </Button>
            }
            detail="обычно это занимает около двух минут"
          />
        )}
      </div>

      <div className="workspace-composer" style={{ gridRow: 4 }}>
        {!!attachments.length && (
          <div className="attachment-shelf">
            {attachments.map((name) => (
              <span key={name}>
                <File size={12} />
                {name}
                <button
                  onClick={() =>
                    setAttachments((items) => items.filter((item) => item !== name))
                  }
                >
                  <X size={11} />
                </button>
              </span>
            ))}
          </div>
        )}
        <div className="composer-line">
          <IconButton label="Приложить файл" onClick={() => fileInput.current?.click()}>
            <Paperclip size={16} />
          </IconButton>
          <input
            ref={fileInput}
            type="file"
            multiple
            hidden
            onChange={(event) =>
              setAttachments(
                Array.from(event.target.files ?? []).map((file) => file.name),
              )
            }
          />
          <textarea
            value={text}
            rows={1}
            onChange={(event) => setText(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && !event.shiftKey) {
                event.preventDefault();
                void submit();
              }
            }}
            placeholder={
              project.vmState === 'running'
                ? 'Напиши задачу агенту…'
                : 'Напиши задачу — машина поднимется сама'
            }
          />
          <IconButton label="Отправить" className="send-button" onClick={() => void submit()}>
            <Send size={16} />
          </IconButton>
        </div>
        <span className="composer-hint">
          Enter — отправить · Shift + Enter — новая строка · файлы можно
          перетащить
        </span>
      </div>

      <AnimatePresence>
        {confirmStop && (
          <motion.div
            className="modal-scrim"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={() => setConfirmStop(false)}
          >
            <motion.div
              className="confirm-card"
              initial={{ y: 12, scale: 0.97 }}
              animate={{ y: 0, scale: 1 }}
              exit={{ y: 8, scale: 0.98 }}
              onClick={(event) => event.stopPropagation()}
            >
              <span className="danger-icon">
                <CircleStop size={19} />
              </span>
              <h2>Выключить {project.name}?</h2>
              <p>
                Агент сейчас работает — прогон прервётся. VM, диск и общие файлы
                проекта сохранятся, процессы внутри погибнут.
              </p>
              <div>
                <Button onClick={() => setConfirmStop(false)}>Отмена</Button>
                <Button variant="danger" onClick={() => void stop()}>
                  Выключить
                </Button>
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </section>
  );
}

function AgentWorkspaceFeed({ vm }: { vm: VirtualMachine }) {
  const toast = useAppStore((state) => state.toast);
  const command = `jarvis vm shell ${vm.project} --resume`;
  return (
    <motion.div
      aria-label="Результат агента"
      className="agent-workspace-feed"
      role="region"
      initial={{ opacity: 0, y: 8 }}
      animate={{ opacity: 1, y: 0 }}
    >
      <header className="agent-presence">
        <span className="agent-presence__icon">
          <Bot size={17} />
        </span>
        <span className="agent-presence__copy">
          <strong>Агент готов к работе</strong>
          <small>Claude · {vm.name}</small>
        </span>
        <span className="agent-presence__status">
          <StatusDot status="active" pulse />
          на связи
        </span>
      </header>

      <div className="agent-result-grid">
        <section className="agent-result-card">
          <span className="eyebrow">Последний результат</span>
          <h2>Рабочее дерево проверено</h2>
          <p>Подготовка завершена. Агент сохранил контекст и ждёт следующую задачу.</p>
          <div className="agent-result-stats">
            <span><Check size={14} /> 17 файлов изменено</span>
            <span><Check size={14} /> 42 теста прошли</span>
          </div>
        </section>

        <aside className="agent-resume-card">
          <span className="agent-resume-card__icon">
            <Terminal size={16} />
          </span>
          <div>
            <strong>Продолжить в терминале</strong>
            <small>Если нужен прямой доступ к сессии</small>
          </div>
          <code>{command}</code>
        <button
          aria-label="Скопировать команду входа"
          onClick={() => {
            void navigator.clipboard?.writeText(command);
            toast({ kind: 'success', title: 'Команда скопирована', body: command });
          }}
        >
            <Copy size={14} /> Копировать
        </button>
        </aside>
      </div>
    </motion.div>
  );
}

function VmStateScene({
  state,
  title,
  body,
  action,
  detail,
}: {
  state: 'stopped' | 'booting' | 'error';
  title: string;
  body: string;
  action?: React.ReactNode;
  detail: string;
}) {
  return (
    <motion.div
      className={`vm-scene vm-scene--${state}`}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
    >
      <div className="vm-orbit">
        <motion.span
          className="vm-orbit__ring"
          {...(state === 'booting' ? { animate: { rotate: 360 } } : {})}
          transition={{ duration: 7, repeat: Infinity, ease: 'linear' }}
        />
        <Box size={50} strokeWidth={1.15} />
        <span className="vm-orbit__signal" />
      </div>
      <h2>{title}</h2>
      <p>{body}</p>
      {action && <div className="vm-scene__actions">{action}</div>}
      <small>{detail}</small>
    </motion.div>
  );
}

function VmControlPanel({
  vm,
  moreOpen,
  onMore,
  onStart,
  onStop,
  onFiles,
}: {
  vm: VirtualMachine;
  moreOpen: boolean;
  onMore: () => void;
  onStart: () => void;
  onStop: () => void;
  onFiles: () => void;
}) {
  const patch = useAppStore((state) => state.patchSnapshot);
  const toast = useAppStore((state) => state.toast);
  const live = vm.state === 'running' || vm.state === 'booting';
  return (
    <motion.aside
      className="vm-control-panel"
      initial={{ opacity: 0, height: 0 }}
      animate={{ opacity: 1, height: 'auto' }}
      exit={{ opacity: 0, height: 0 }}
    >
      <div className="vm-control-head">
        <StatusDot status={live ? 'active' : 'idle'} pulse={live} />
        <div>
          <strong>{live ? 'Работает' : 'Остановлена'}</strong>
          <span>{vm.name}</span>
        </div>
        <div className="vm-metrics">
          <span><Cpu size={13} /> {vm.cpus} CPU</span>
          <span><MemoryStick size={13} /> {vm.ramUsed}/{vm.ramTotal} ГиБ</span>
          <span><HardDrive size={13} /> {vm.diskUsed} ГБ</span>
        </div>
      </div>
      <div className="vm-control-grid">
        <span>Файлы проекта<strong>чтение и запись</strong></span>
        <span>Агенты<strong>Claude · Codex</strong></span>
        <span>Диск<strong>{vm.diskUsed} из {vm.diskMax} ГиБ</strong></span>
      </div>
      <div className="vm-control-auto">
        <div>
          <strong>Запускать вместе с Jarvis</strong>
          <span>Поднимается только VM, без запуска агента</span>
        </div>
        <Switch
          label="Автозапуск VM"
          checked={vm.autostart}
          onChange={(checked) =>
            patch((snapshot) => {
              const current = snapshot.vms.find((item) => item.id === vm.id);
              if (current) current.autostart = checked;
            })
          }
        />
      </div>
      <div className="vm-control-actions">
        <Button onClick={onFiles}><FolderOpen size={14} /> Файлы</Button>
        {live ? (
          <>
            <Button onClick={onStart}><RefreshCw size={14} /> Перезапустить</Button>
            <Button variant="danger" onClick={onStop}>Выключить</Button>
          </>
        ) : (
          <Button variant="primary" onClick={onStart}><Play size={14} /> Запустить</Button>
        )}
        <Button onClick={onMore}>Ещё <ChevronDown size={13} /></Button>
      </div>
      <AnimatePresence>
        {moreOpen && (
          <motion.div
            className="vm-more-actions"
            initial={{ opacity: 0, y: -5 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -4 }}
          >
            {['Открыть терминал', 'Копировать вход', 'Освободить кэш 841 МБ'].map(
              (label) => (
                <button
                  key={label}
                  onClick={() => toast({ kind: 'info', title: label })}
                >
                  {label}
                </button>
              ),
            )}
          </motion.div>
        )}
      </AnimatePresence>
    </motion.aside>
  );
}

function EnvironmentList() {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const navigate = useAppStore((state) => state.navigate);
  const client = useAppStore((state) => state.client);
  const refresh = useAppStore((state) => state.refresh);
  const toast = useAppStore((state) => state.toast);
  const [mirrorOpen, setMirrorOpen] = useState(false);
  const [skillsOpen, setSkillsOpen] = useState(false);
  const [maintenanceOpen, setMaintenanceOpen] = useState(false);
  const [expandedVm, setExpandedVm] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [confirmVm, setConfirmVm] = useState<string | null>(null);
  const skills = snapshot.skills.filter((skill) =>
    skill.toLowerCase().includes(query.toLowerCase()),
  );
  const running = snapshot.vms.filter((vm) => vm.state === 'running').length;
  const disk = snapshot.vms.reduce((sum, vm) => sum + vm.diskUsed, 0);
  const statusCopy: Record<VmState, { label: string; note: string }> = {
    running: { label: 'Работает', note: 'готова к работе' },
    booting: { label: 'Запускается', note: 'обычно меньше минуты' },
    stopped: { label: 'Остановлена', note: 'запустится по запросу' },
    error: { label: 'Требует внимания', note: 'нужно проверить запуск' },
    none: { label: 'Не создана', note: 'можно настроить позже' },
  };
  const mirrorItems = [
    {
      name: 'Память проекта',
      value: '28 файлов',
      status: 'перенесено',
      tone: 'ok',
    },
    {
      name: 'Память Codex',
      value: 'MEMORY.md, сырые записи и выжимки',
      status: 'перенесено',
      tone: 'ok',
    },
    {
      name: 'Навыки и команды',
      value: `${snapshot.skills.length} навыков · 12 команд · 8 агентов`,
      status: 'перенесено',
      tone: 'ok',
    },
    {
      name: 'Настройки агента',
      value: 'модель, стиль и режим доступа',
      status: 'перенесено',
      tone: 'ok',
    },
    {
      name: 'MCP-серверы',
      value: '5 из 6 · postgres-local недоступен в VM',
      status: 'с оговоркой',
      tone: 'warning',
    },
    {
      name: 'Хуки',
      value: 'команды хоста внутри VM не вызываются',
      status: 'не переносится',
      tone: 'muted',
    },
    {
      name: 'Транскрипты и кэш',
      value: 'агенту не нужны и занимают сотни мегабайт',
      status: 'не переносится',
      tone: 'muted',
    },
    {
      name: 'Токены доступа',
      value: 'авторизация выполняется отдельно защищённым каналом',
      status: 'защищено',
      tone: 'protected',
    },
  ] as const;

  const setState = async (vm: VirtualMachine, state: VmState) => {
    await client.setVmState(vm.id, state);
    await refresh();
    setConfirmVm(null);
    toast({
      kind: state === 'running' ? 'success' : 'info',
      title: state === 'running' ? `${vm.project} запущена` : `${vm.project} выключена`,
    });
  };

  return (
    <section className="page environment-page">
      <header className="page-header environment-page__header">
        <div>
          <span className="eyebrow">Agent VM</span>
          <h1>Виртуальные машины</h1>
          <p>{running} из {snapshot.vms.length} работает · агент меняет файлы внутри машины, не трогая твой Mac</p>
        </div>
        <div className="page-header__spacer" />
        <div className="environment-maintenance">
          <IconButton
            label="Обслуживание машин"
            onClick={() => setMaintenanceOpen((value) => !value)}
          >
            <MoreHorizontal size={17} />
          </IconButton>
          <AnimatePresence>
            {maintenanceOpen && (
              <motion.div
                className="environment-maintenance__menu"
                initial={{ opacity: 0, y: -6, scale: 0.98 }}
                animate={{ opacity: 1, y: 0, scale: 1 }}
                exit={{ opacity: 0, y: -4, scale: 0.98 }}
              >
                <button
                  aria-label="Очистить кэш · 841 МБ"
                  onClick={() => {
                    setMaintenanceOpen(false);
                    toast({
                      kind: 'success',
                      title: 'Кэш очищен',
                      body: '841 МБ освобождено, существующие машины не затронуты',
                    });
                  }}
                >
                  <Trash2 size={15} />
                  <span>
                    <strong>Очистить кэш</strong>
                    <small>Освободить 841 МБ, не затрагивая машины</small>
                  </span>
                </button>
              </motion.div>
            )}
          </AnimatePresence>
        </div>
      </header>
      <div className="environment-list">
        {snapshot.vms.map((vm) => {
          const detailsOpen = expandedVm === vm.id;
          const copy = statusCopy[vm.state];
          return (
            <article
              className={`environment-item environment-item--${vm.state}`}
              key={vm.id}
            >
              <div className="environment-row">
                <StatusDot
                  status={vm.state === 'running' ? 'active' : vm.state === 'error' ? 'error' : 'idle'}
                  pulse={vm.state === 'running' || vm.state === 'booting'}
                />
                <div className="environment-name">
                  <strong>{vm.project}</strong>
                  <span>{vm.name}</span>
                </div>
                <div className="environment-summary">
                  <strong>{copy.label}</strong>
                  <span>{vm.state === 'running' && vm.uptime ? `уже ${vm.uptime}` : copy.note}</span>
                </div>
                <div className="environment-actions">
                  <Button
                    onClick={() =>
                      navigate({
                        section: 'projects',
                        view: 'environment-files',
                        vmId: vm.id,
                      })
                    }
                  >
                    <FolderOpen size={14} /> Файлы
                  </Button>
                  {vm.state === 'running' ? (
                    <>
                      <Button onClick={() => void setState(vm, 'booting')}>
                        <RefreshCw size={14} /> Перезапустить
                      </Button>
                      {confirmVm === vm.id ? (
                        <span className="inline-confirm">
                          <Button onClick={() => setConfirmVm(null)}>Отмена</Button>
                          <Button variant="danger" onClick={() => void setState(vm, 'stopped')}>Выключить</Button>
                        </span>
                      ) : (
                        <Button variant="danger" onClick={() => setConfirmVm(vm.id)}>Выключить</Button>
                      )}
                    </>
                  ) : (
                    <Button variant="primary" onClick={() => void setState(vm, 'running')}>
                      <Play size={14} /> Запустить
                    </Button>
                  )}
                </div>
                <IconButton
                  className={detailsOpen ? 'is-open' : ''}
                  label={`Подробнее о машине ${vm.project}`}
                  onClick={() => setExpandedVm(detailsOpen ? null : vm.id)}
                >
                  <ChevronDown size={16} />
                </IconButton>
              </div>
              <AnimatePresence initial={false}>
                {detailsOpen && (
                  <motion.div
                    className="environment-detail"
                    initial={{ height: 0, opacity: 0 }}
                    animate={{ height: 'auto', opacity: 1 }}
                    exit={{ height: 0, opacity: 0 }}
                  >
                    <div className="environment-detail__inner">
                      <div>
                        <span>Ресурсы</span>
                        <strong>{vm.cpus} CPU</strong>
                        <strong>{vm.ramTotal} ГиБ RAM</strong>
                      </div>
                      <div>
                        <span>Диск</span>
                        <strong>{vm.diskUsed} из {vm.diskMax} ГБ</strong>
                        <small>{disk.toFixed(1)} ГБ занято всеми машинами</small>
                      </div>
                      <div>
                        <span>Сейчас</span>
                        <strong>
                          {vm.state === 'running'
                            ? `${vm.ramUsed} ГиБ памяти занято`
                            : copy.label}
                        </strong>
                        <small>{vm.autostart ? 'автозапуск включён' : 'запуск вручную'}</small>
                      </div>
                    </div>
                  </motion.div>
                )}
              </AnimatePresence>
            </article>
          );
        })}
      </div>
      <div className="mirror-card">
        <button
          aria-expanded={mirrorOpen}
          aria-label="Что доступно агенту"
          className="mirror-head"
          onClick={() => setMirrorOpen((value) => !value)}
        >
          <span className="mirror-check"><ShieldCheck size={18} /></span>
          <span>
            <strong>Что доступно агенту</strong>
            <small>Память, навыки и настройки синхронизированы</small>
          </span>
          <em>41 файл</em>
          <ChevronDown className={mirrorOpen ? 'is-open' : ''} size={16} />
        </button>
        <AnimatePresence>
          {mirrorOpen && (
            <motion.div
              className="mirror-detail"
              initial={{ opacity: 0, height: 0 }}
              animate={{ opacity: 1, height: 'auto' }}
              exit={{ opacity: 0, height: 0 }}
            >
              {mirrorItems.map((item) => (
                <div className={`mirror-row mirror-row--${item.tone}`} key={item.name}>
                  <span className="mirror-row__icon">
                    {item.tone === 'ok' ? <Check size={13} /> : <ShieldCheck size={13} />}
                  </span>
                  <span className="mirror-row__copy">
                    <strong>{item.name}</strong>
                    <small>{item.value}</small>
                  </span>
                  <em>{item.status}</em>
                </div>
              ))}
              <button className="skills-toggle" onClick={() => setSkillsOpen((value) => !value)}>
                <span>Посмотреть навыки агента</span>
                <small>{snapshot.skills.length}</small>
                <ChevronDown size={14} />
              </button>
              {skillsOpen && (
                <div className="skills-panel">
                  <label><Search size={13} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={`Поиск среди ${snapshot.skills.length} навыков`} /></label>
                  <div>{skills.map((skill) => <span key={skill}>{skill}</span>)}</div>
                  <small>{query ? `найдено ${skills.length}` : `показаны ${skills.length}`}</small>
                </div>
              )}
              <p className="mirror-note">
                Это snapshot-копия, а не общая папка. Исключение — каталог
                проекта: он доступен на чтение и запись в обе стороны.
              </p>
            </motion.div>
          )}
        </AnimatePresence>
      </div>
    </section>
  );
}

function EnvironmentFiles({ vmId }: { vmId: string }) {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const navigate = useAppStore((state) => state.navigate);
  const patch = useAppStore((state) => state.patchSnapshot);
  const toast = useAppStore((state) => state.toast);
  const vm = snapshot.vms.find((item) => item.id === vmId);
  const [path, setPath] = useState<string[]>([]);
  if (!vm) return null;

  const root = vm.home;
  let nodes: FileNode[] = root;
  for (const step of path) {
    nodes = nodes.find((node) => node.name === step)?.children ?? [];
  }
  const sorted = [...nodes].sort((a, b) =>
    a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === 'directory' ? -1 : 1,
  );

  return (
    <section className="page">
      <header className="page-header">
        <IconButton
          label="К списку машин"
          onClick={() => navigate({ section: 'projects', view: 'environments' })}
        >
          <ArrowLeft size={17} />
        </IconButton>
        <div>
          <span className="eyebrow">файлы внутри VM</span>
          <h1>{vm.project}</h1>
          <p>{vm.diskUsed} ГБ из {vm.diskMax} ГиБ</p>
        </div>
        <div className="page-header__spacer" />
        <Button onClick={() => toast({ kind: 'info', title: 'Открываю терминал', body: vm.name })}>
          <Terminal size={14} /> Открыть терминал
        </Button>
      </header>
      <div className="file-layout">
        <aside className="mounts-panel">
          <h3>Общие папки с хостом</h3>
          {vm.mounts.map((mount) => (
            <div className="mount-row" key={mount.id}>
              <Folder size={15} />
              <span>
                <strong>{mount.host} → {mount.guest}</strong>
                <small>{mount.description}</small>
              </span>
              <em className={mount.mode === 'rw' ? 'is-rw' : ''}>
                {mount.mode === 'rw' ? 'чтение и запись' : 'только чтение'}
              </em>
              {mount.removable && (
                <IconButton
                  label="Убрать папку"
                  onClick={() =>
                    patch((state) => {
                      const target = state.vms.find((item) => item.id === vm.id);
                      if (target)
                        target.mounts = target.mounts.filter((item) => item.id !== mount.id);
                    })
                  }
                >
                  <X size={13} />
                </IconButton>
              )}
            </div>
          ))}
          <Button
            onClick={() =>
              patch((state) => {
                const target = state.vms.find((item) => item.id === vm.id);
                target?.mounts.push({
                  id: crypto.randomUUID(),
                  host: '~/Documents/new-reference',
                  guest: '/mnt/new-reference',
                  mode: 'ro',
                  description: 'Добавлено в прототипе',
                  removable: true,
                });
              })
            }
          >
            <Plus size={14} /> Добавить папку
          </Button>
          <p>Ключи, `.ssh`, `.claude` auth и `.codex` auth нельзя открыть как дополнительные mount.</p>
        </aside>
        <div className="file-browser">
          <div className="breadcrumbs">
            <button onClick={() => setPath([])} className={!path.length ? 'is-current' : ''}>
              <FolderOpen size={14} /> home
            </button>
            {path.map((step, index) => (
              <span key={`${step}-${index}`}>
                <ChevronRight size={12} />
                <button
                  className={index === path.length - 1 ? 'is-current' : ''}
                  onClick={() => setPath(path.slice(0, index + 1))}
                >
                  {step}
                </button>
              </span>
            ))}
          </div>
          {sorted.length ? (
            <div className="file-list">
              {sorted.map((node) => (
                <button
                  type="button"
                  key={node.name}
                  disabled={node.kind === 'file'}
                  onClick={() => node.kind === 'directory' && setPath([...path, node.name])}
                >
                  {node.kind === 'directory' ? <Folder size={15} /> : <File size={15} />}
                  <strong>{node.name}</strong>
                  <span>{node.note ?? node.size}</span>
                  {node.kind === 'directory' && <ChevronRight size={14} />}
                </button>
              ))}
            </div>
          ) : (
            <EmptyState
              icon={<FolderOpen size={34} />}
              title="Папка пуста"
              body="Содержимое будет читаться из работающей VM."
            />
          )}
        </div>
      </div>
    </section>
  );
}
