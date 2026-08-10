import { useMemo, useRef, useState } from 'react';
import {
  ArrowLeft,
  Bookmark,
  Bot,
  CheckCircle2,
  ChevronDown,
  CircleDashed,
  ClipboardList,
  Code2,
  File,
  FileDiff,
  FolderSearch,
  Image,
  ListChecks,
  MessageSquareText,
  Paperclip,
  Pin,
  Play,
  Search,
  Send,
  SkipForward,
  Terminal,
  Trash2,
  X,
} from 'lucide-react';
import { AnimatePresence, motion } from 'motion/react';
import type {
  AgentQuestionSet,
  Attachment,
  Session,
  TaskBoard,
  TurnFile,
} from '../../core/client/types';
import { useAppStore } from '../../app/app-store';
import {
  Button,
  IconButton,
  SelectionMark,
  StatusDot,
} from '../../shared/ui/controls';

const statusLabel: Record<Session['status'], string> = {
  working: 'работает',
  waiting: 'ждёт тебя',
  done: 'готово',
  interrupted: 'прервано',
  error: 'ошибка',
};

const statusTone = (status: Session['status']) =>
  status === 'working'
    ? 'active'
    : status === 'waiting'
      ? 'waiting'
      : status === 'error'
        ? 'error'
        : status === 'done'
          ? 'done'
          : 'idle';

export function ChatsFeature() {
  const route = useAppStore((state) => state.route);
  if (route.section !== 'chats') return null;
  if (route.view === 'session') return <ChatSession sessionId={route.sessionId} />;
  return <SessionList />;
}

function SessionList() {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const search = useAppStore((state) => state.search).toLowerCase();
  const navigate = useAppStore((state) => state.navigate);
  const client = useAppStore((state) => state.client);
  const refresh = useAppStore((state) => state.refresh);
  const toast = useAppStore((state) => state.toast);
  const sessions = useMemo(
    () =>
      [...snapshot.sessions]
        .filter((session) =>
          `${session.project} ${session.summary} ${session.agent}`
            .toLowerCase()
            .includes(search),
        )
        .sort(
          (a, b) =>
            Number(b.pinned) - Number(a.pinned) ||
            (b.doneAt ?? b.createdAt) - (a.doneAt ?? a.createdAt),
        ),
    [snapshot.sessions, search],
  );

  const clear = async () => {
    await client.clearFinishedSessions();
    await refresh();
    toast({ kind: 'success', title: 'Завершённые сессии убраны' });
  };

  return (
    <section className="page">
      <header className="page-header">
        <div>
          <span className="eyebrow">Сейчас</span>
          <h1>Активные чаты</h1>
          <p>
            {snapshot.sessions.filter((item) => item.status === 'working').length}{' '}
            работают ·{' '}
            {snapshot.sessions.filter((item) => item.status === 'waiting').length}{' '}
            ждёт ответа
          </p>
        </div>
        <Button onClick={() => void clear()}>
          <Trash2 size={14} /> Убрать завершённые
        </Button>
      </header>
      <div className="session-list line-free-list" role="listbox">
        {sessions.map((session, index) => (
          <motion.button
            key={session.id}
            type="button"
            className={`session-row line-free-row session-row--${session.status}`}
            onClick={() =>
              navigate({
                section: 'chats',
                view: 'session',
                sessionId: session.id,
              })
            }
            initial={{ opacity: 0, y: 5 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ delay: index * 0.025 }}
          >
            <StatusDot
              status={statusTone(session.status)}
              pulse={session.status === 'working'}
            />
            <span className="session-identity">
              <strong>{session.project}</strong>
              {session.branch && <small>⎇ {session.branch}</small>}
            </span>
            {session.agent === 'codex' && (
              <span className="agent-label">codex</span>
            )}
            <span className="model-label">{session.model}</span>
            <span className="session-summary">
              <strong>{session.summary}</strong>
              <small>
                {statusLabel[session.status]} · {session.detail}
              </small>
            </span>
            <span className="host-label">{session.host}</span>
            {session.pinned && (
              <span className="pin-mark">
                <Bookmark size={12} fill="currentColor" />
              </span>
            )}
            <time>
              {new Date(session.createdAt).toLocaleTimeString('ru', {
                hour: '2-digit',
                minute: '2-digit',
              })}
            </time>
          </motion.button>
        ))}
        {!sessions.length && (
          <div className="list-empty">
            <Search size={24} />
            <strong>Ничего не найдено</strong>
            <span>Попробуй название проекта или агента.</span>
          </div>
        )}
      </div>
    </section>
  );
}

function ChatSession({ sessionId }: { sessionId: string }) {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const navigate = useAppStore((state) => state.navigate);
  const client = useAppStore((state) => state.client);
  const refresh = useAppStore((state) => state.refresh);
  const toast = useAppStore((state) => state.toast);
  const sendMessage = useAppStore((state) => state.sendMessage);
  const session = snapshot.sessions.find((item) => item.id === sessionId);
  const messages = snapshot.messages[sessionId] ?? [];
  const summaries = snapshot.summaries[sessionId] ?? [];
  const [summaryMode, setSummaryMode] = useState(true);
  const [text, setText] = useState('');
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [sending, setSending] = useState(false);
  const [boardOpen, setBoardOpen] = useState(false);
  const [questionOpen, setQuestionOpen] = useState(Boolean(session?.question));
  const [document, setDocument] = useState<TurnFile | null>(null);
  const [commandOpen, setCommandOpen] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  if (!session) return null;

  const submit = async () => {
    if (!text.trim() && !attachments.length) return;
    setSending(true);
    await sendMessage(session.id, text.trim(), attachments);
    setText('');
    setAttachments([]);
    setSending(false);
    toast({ kind: 'success', title: 'Сообщение отправлено', body: session.project });
  };

  const togglePin = async () => {
    await client.pinSession(session.id, !session.pinned);
    await refresh();
  };

  return (
    <section className="chat-page">
      <header className="chat-header">
        <IconButton
          label="К списку чатов · Esc"
          onClick={() => navigate({ section: 'chats', view: 'list' })}
        >
          <ArrowLeft size={17} />
        </IconButton>
        <div className="chat-title">
          <span>сессия</span>
          <strong>{session.project}</strong>
        </div>
        <span className="model-label">{session.model}</span>
        <span className="agent-label">{session.agent}</span>
        <span className="chat-live">
          <StatusDot
            status={statusTone(session.status)}
            pulse={session.status === 'working'}
          />
          {session.detail}
        </span>
        <div className="chat-header__spacer" />
        {session.board && (
          <Button onClick={() => setBoardOpen(true)}>
            <ListChecks size={14} />
            {session.board.tasks.filter((task) => task.status === 'completed').length}/
            {session.board.tasks.length}
          </Button>
        )}
        {session.question && (
          <Button onClick={() => setQuestionOpen(true)}>
            <MessageSquareText size={14} /> варианты
          </Button>
        )}
        <Button
          className={summaryMode ? 'is-active' : ''}
          onClick={() => setSummaryMode((value) => !value)}
        >
          {summaryMode ? 'Сводка' : 'Лента'}
        </Button>
        <IconButton
          label={session.pinned ? 'Открепить' : 'Закрепить'}
          onClick={() => void togglePin()}
        >
          <Pin size={15} fill={session.pinned ? 'currentColor' : 'none'} />
        </IconButton>
      </header>

      <div className="transcript">
        <div className="transcript-column">
          {messages.map((message) =>
            message.role === 'tool' ? (
              <div className="tool-chip" key={message.id}>
                <Code2 size={12} />
                {message.tool?.label ?? message.text}
                {(message.tool?.count ?? 0) > 1 && (
                  <em>×{message.tool?.count}</em>
                )}
              </div>
            ) : (
              <article
                className={`message message--${message.role}`}
                key={message.id}
              >
                <header>
                  <strong>{message.role === 'user' ? 'Ты' : 'Jarvis'}</strong>
                  <time>
                    {new Date(message.timestamp).toLocaleTimeString('ru', {
                      hour: '2-digit',
                      minute: '2-digit',
                    })}
                  </time>
                </header>
                <p>{message.text}</p>
                {!!message.attachments?.length && (
                  <div className="message-attachments">
                    {message.attachments.map((attachment) => (
                      <span key={attachment.id}>
                        {attachment.kind === 'image' ? (
                          <Image size={12} />
                        ) : (
                          <File size={12} />
                        )}
                        {attachment.name}
                      </span>
                    ))}
                  </div>
                )}
              </article>
            ),
          )}
          {summaryMode &&
            summaries.map((summary) => (
              <section className="turn-summary" key={summary.id}>
                <div className="turn-summary__signal">
                  <CheckCircle2 size={15} />
                </div>
                <div>
                  <span className="eyebrow">Сводка хода</span>
                  <p>{summary.summary}</p>
                  <div className="summary-files">
                    {summary.files.map((file) => (
                      <button key={file.path} onClick={() => setDocument(file)}>
                        {file.kind === 'edited' ? (
                          <FileDiff size={13} />
                        ) : (
                          <File size={13} />
                        )}
                        <span>
                          <strong>{file.path.split('/').pop()}</strong>
                          <small>{file.note}</small>
                        </span>
                      </button>
                    ))}
                  </div>
                  <small>{summary.commands.join(' · ')}</small>
                </div>
              </section>
            ))}
          {sending && (
            <div className="pending-reply">
              <span className="typing-dots"><i /><i /><i /></span>
              Сообщение в очереди…
            </div>
          )}
        </div>
      </div>

      <div className="chat-composer">
        {!!attachments.length && (
          <div className="attachment-shelf">
            {attachments.map((attachment) => (
              <span key={attachment.id}>
                <File size={12} />
                {attachment.name}
                <button
                  onClick={() =>
                    setAttachments((items) =>
                      items.filter((item) => item.id !== attachment.id),
                    )
                  }
                >
                  <X size={11} />
                </button>
              </span>
            ))}
          </div>
        )}
        <div className="composer-line">
          <IconButton label="Добавить вложения" onClick={() => fileInput.current?.click()}>
            <Paperclip size={16} />
          </IconButton>
          <input
            ref={fileInput}
            hidden
            multiple
            type="file"
            onChange={(event) =>
              setAttachments(
                Array.from(event.target.files ?? []).map((file) => ({
                  id: crypto.randomUUID(),
                  name: file.name,
                  size: file.size,
                  kind: file.type.startsWith('image/') ? 'image' : 'file',
                })),
              )
            }
          />
          <textarea
            rows={1}
            value={text}
            placeholder="Ответить агенту…"
            onChange={(event) => {
              setText(event.target.value);
              setCommandOpen(event.target.value.trim().startsWith('/'));
            }}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && !event.shiftKey) {
                event.preventDefault();
                void submit();
              }
            }}
          />
          <IconButton label="Отправить" className="send-button" onClick={() => void submit()}>
            <Send size={16} />
          </IconButton>
          <AnimatePresence>
            {commandOpen && (
              <CommandPalette
                session={session}
                onClose={() => setCommandOpen(false)}
              />
            )}
          </AnimatePresence>
        </div>
        <div className="composer-status">
          <span>{session.usage.tokens.toLocaleString('ru')} токенов</span>
          <span>
            {session.usage.billing === 'plan'
              ? 'включено в план'
              : `$${session.usage.cost.toFixed(2)}`}
          </span>
          <button
            onClick={() =>
              toast({
                kind: 'info',
                title: 'Команда входа',
                body: `claude --resume ${session.id}`,
              })
            }
          >
            <Terminal size={12} /> продолжить в терминале
          </button>
        </div>
      </div>

      <AnimatePresence>
        {boardOpen && session.board && (
          <TaskBoardPanel board={session.board} onClose={() => setBoardOpen(false)} onPrefill={setText} />
        )}
        {questionOpen && session.question && (
          <QuestionPanel
            sessionId={session.id}
            set={session.question}
            onClose={() => setQuestionOpen(false)}
          />
        )}
        {document && (
          <DocumentPanel file={document} onClose={() => setDocument(null)} />
        )}
      </AnimatePresence>
    </section>
  );
}

function CommandPalette({
  session,
  onClose,
}: {
  session: Session;
  onClose: () => void;
}) {
  const client = useAppStore((state) => state.client);
  const refresh = useAppStore((state) => state.refresh);
  const commands = [
    { label: '/model opus-4.8', hint: 'сменить модель', run: () => client.setSessionModel(session.id, 'opus-4.8') },
    { label: '/effort high', hint: 'глубина рассуждений', run: () => client.setSessionEffort(session.id, 'high') },
    { label: '/compact', hint: 'сжать контекст', run: async () => undefined },
    { label: '/terminal', hint: 'показать resume-команду', run: async () => undefined },
  ];
  return (
    <motion.div
      className="command-palette popover"
      initial={{ opacity: 0, y: 6, scale: 0.98 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      exit={{ opacity: 0, y: 4 }}
    >
      <span className="popover-label">Команды сессии</span>
      {commands.map((command) => (
        <button
          key={command.label}
          onClick={async () => {
            await command.run();
            await refresh();
            onClose();
          }}
        >
          <Code2 size={14} />
          <strong>{command.label}</strong>
          <small>{command.hint}</small>
        </button>
      ))}
    </motion.div>
  );
}

function TaskBoardPanel({
  board,
  onClose,
  onPrefill,
}: {
  board: TaskBoard;
  onClose: () => void;
  onPrefill: (text: string) => void;
}) {
  const done = board.tasks.filter((task) => task.status === 'completed').length;
  return (
    <Overlay onClose={onClose}>
      <div className="panel-header">
        <span><ClipboardList size={17} /></span>
        <div><strong>Задачи сессии</strong><small>{done}/{board.tasks.length} выполнено</small></div>
        <IconButton label="Закрыть" onClick={onClose}><X size={15} /></IconButton>
      </div>
      <div className="task-progress"><i style={{ width: `${(done / board.tasks.length) * 100}%` }} /></div>
      <div className="task-list">
        {board.tasks.map((task) => (
          <div className={`task-row task-row--${task.status}`} key={task.id}>
            {task.status === 'completed' ? <CheckCircle2 size={15} /> : task.status === 'in_progress' ? <span className="task-pulse" /> : <CircleDashed size={15} />}
            <span><small>Task {task.number}</small><strong>{task.title}</strong></span>
            <em>{task.model} · {task.status === 'in_progress' ? 'идёт 7м…' : task.status === 'completed' ? `${Math.round((task.durationMs ?? 0) / 60000)}м` : 'в очереди'}</em>
            {task.status !== 'completed' && (
              <div>
                <Button onClick={() => { onPrefill(`Перейди к Task ${task.number}: ${task.title}`); onClose(); }}><Play size={12} /> Перейти</Button>
                <Button onClick={() => { onPrefill(`Пропусти Task ${task.number} и продолжай дальше`); onClose(); }}><SkipForward size={12} /> Пропустить</Button>
              </div>
            )}
          </div>
        ))}
      </div>
      {!!board.subagents.length && <div className="subagent-strip">сабагенты: {board.subagents.map((item) => `${item.name} · ${item.model}`).join('   ·   ')}</div>}
    </Overlay>
  );
}

function QuestionPanel({
  sessionId,
  set,
  onClose,
}: {
  sessionId: string;
  set: AgentQuestionSet;
  onClose: () => void;
}) {
  const client = useAppStore((state) => state.client);
  const refresh = useAppStore((state) => state.refresh);
  const toast = useAppStore((state) => state.toast);
  const [index, setIndex] = useState(set.current);
  const [answers, setAnswers] = useState<Record<string, string[]>>({});
  const [custom, setCustom] = useState('');
  const question = set.questions[index];
  if (!question) return null;
  const selected = answers[question.id] ?? [];
  const toggle = (id: string) =>
    setAnswers((value) => ({
      ...value,
      [question.id]: question.multiSelect
        ? selected.includes(id)
          ? selected.filter((item) => item !== id)
          : [...selected, id]
        : [id],
    }));
  const next = async () => {
    if (custom.trim()) answers[question.id] = [...selected, custom.trim()];
    if (index < set.questions.length - 1) {
      setIndex(index + 1);
      setCustom('');
      return;
    }
    await client.answerQuestion(sessionId, answers);
    await refresh();
    toast({ kind: 'success', title: 'Ответ отправлен агенту' });
    onClose();
  };

  return (
    <Overlay onClose={onClose}>
      <div className="panel-header">
        <span><MessageSquareText size={17} /></span>
        <div><strong>{question.header}</strong><small>{index + 1} из {set.questions.length}</small></div>
        <IconButton label="Закрыть" onClick={onClose}><X size={15} /></IconButton>
      </div>
      <div className="question-body">
        <h2>{question.question}</h2>
        <div className="question-options">
          {question.options.map((option, optionIndex) => (
            <button key={option.id} onClick={() => toggle(option.id)} className={selected.includes(option.id) ? 'is-selected' : ''}>
              <kbd>{optionIndex + 1}</kbd>
              <span><strong>{option.label}</strong>{option.description && <small>{option.description}</small>}</span>
              <SelectionMark selected={selected.includes(option.id)} />
            </button>
          ))}
        </div>
        {question.allowCustom && <input value={custom} onChange={(event) => setCustom(event.target.value)} placeholder="Свой ответ…" />}
        <div className="question-footer">
          <span>{question.multiSelect ? 'Можно выбрать несколько' : 'Выбери один вариант'}</span>
          <Button variant="primary" disabled={!selected.length && !custom.trim()} onClick={() => void next()}>
            {index < set.questions.length - 1 ? 'Далее' : 'Отправить'}
          </Button>
        </div>
      </div>
    </Overlay>
  );
}

function DocumentPanel({ file, onClose }: { file: TurnFile; onClose: () => void }) {
  const [tab, setTab] = useState<'diff' | 'document'>(file.kind === 'edited' ? 'diff' : 'document');
  const toast = useAppStore((state) => state.toast);
  return (
    <Overlay onClose={onClose} wide>
      <div className="panel-header">
        <span><FileDiff size={17} /></span>
        <div><strong>{file.path.split('/').pop()}</strong><small>{file.path}</small></div>
        <Button onClick={() => toast({ kind: 'info', title: 'Показываю файл в Finder', body: file.path })}><FolderSearch size={13} /> Finder</Button>
        <Button onClick={() => toast({ kind: 'info', title: 'Открываю в редакторе', body: file.path })}><Code2 size={13} /> Редактор</Button>
        <IconButton label="Закрыть" onClick={onClose}><X size={15} /></IconButton>
      </div>
      <div className="document-tabs">
        <button className={tab === 'diff' ? 'is-active' : ''} onClick={() => setTab('diff')}>Изменения</button>
        <button className={tab === 'document' ? 'is-active' : ''} onClick={() => setTab('document')}>Документ</button>
      </div>
      {tab === 'diff' ? (
        <pre className="diff-view">
          <span className="diff-context">@@ typed client boundary @@</span>{'\n'}
          <span className="diff-delete">- window.jarvis.invoke('settings_get')</span>{'\n'}
          <span className="diff-add">+ const settings = await client.getSettings()</span>{'\n'}
          <span className="diff-add">+ updateStore(settings)</span>
        </pre>
      ) : (
        <article className="document-view">
          <h1>{file.note}</h1>
          <p>Этот документ показан внутри Jarvis. В реальном приложении содержимое придёт через typed file contract, а внешний URL откроется системным браузером.</p>
          <h2>Результат</h2>
          <p>Интерфейс сохраняет полный путь, умеет показывать изменения и не смешивает preview с редактированием.</p>
        </article>
      )}
    </Overlay>
  );
}

function Overlay({
  children,
  onClose,
  wide = false,
}: {
  children: React.ReactNode;
  onClose: () => void;
  wide?: boolean;
}) {
  return (
    <motion.div className="drawer-scrim" initial={{ opacity: 0 }} animate={{ opacity: 1 }} exit={{ opacity: 0 }} onClick={onClose}>
      <motion.aside
        className={`side-drawer ${wide ? 'side-drawer--wide' : ''}`}
        initial={{ x: 40 }}
        animate={{ x: 0 }}
        exit={{ x: 40 }}
        transition={{ type: 'spring', stiffness: 420, damping: 38 }}
        onClick={(event) => event.stopPropagation()}
      >
        {children}
      </motion.aside>
    </motion.div>
  );
}
