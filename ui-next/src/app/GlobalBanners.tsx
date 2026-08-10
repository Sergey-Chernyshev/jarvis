import { AlertTriangle, Check, RotateCcw, X } from 'lucide-react';
import { AnimatePresence, motion } from 'motion/react';
import { useAppStore } from './app-store';
import { Button, IconButton } from '../shared/ui/controls';

export function GlobalBanners() {
  const visible = useAppStore((state) => state.configErrorVisible);
  const repairing = useAppStore((state) => state.repairingConfig);
  const repair = useAppStore((state) => state.repairConfig);
  const toast = useAppStore((state) => state.toast);

  return (
    <AnimatePresence initial={false}>
      {visible && (
        <motion.div
          className="config-banner"
          initial={{ height: 0, opacity: 0 }}
          animate={{ height: 'auto', opacity: 1 }}
          exit={{ height: 0, opacity: 0 }}
          transition={{ duration: 0.24 }}
        >
          <span className="banner-icon">
            <AlertTriangle size={16} />
          </span>
          <div>
            <strong>Jarvis нашёл проблему в конфигурации</strong>
            <span>
              Конфликтующий permissions-блок мешает агенту читать настройки.
              Валидные поля будут сохранены.
            </span>
          </div>
          <Button
            variant="quiet"
            onClick={() =>
              toast({
                kind: 'warning',
                title: 'Конфликт в settings.json',
                body: 'permissions содержит неизвестное поле. Перед исправлением Jarvis сохранит резервную копию.',
              })
            }
          >
            Подробнее
          </Button>
          <Button variant="primary" onClick={() => void repair()} disabled={repairing}>
            {repairing ? <RotateCcw className="spin" size={14} /> : <Check size={14} />}
            {repairing ? 'Исправляю…' : 'Исправить'}
          </Button>
        </motion.div>
      )}
    </AnimatePresence>
  );
}

export function ToastHost() {
  const toasts = useAppStore((state) => state.toasts);
  const dismiss = useAppStore((state) => state.dismissToast);

  return (
    <div className="toast-host" aria-live="polite">
      <AnimatePresence>
        {toasts.map((toast) => (
          <motion.div
            layout
            key={toast.id}
            className={`toast-card toast-card--${toast.kind}`}
            initial={{ opacity: 0, x: 28, scale: 0.97 }}
            animate={{ opacity: 1, x: 0, scale: 1 }}
            exit={{ opacity: 0, x: 18, scale: 0.97 }}
            transition={{ type: 'spring', stiffness: 430, damping: 34 }}
          >
            <span className="toast-signal" />
            <div>
              <strong>{toast.title}</strong>
              {toast.body && <span>{toast.body}</span>}
            </div>
            <IconButton label="Скрыть" onClick={() => dismiss(toast.id)}>
              <X size={14} />
            </IconButton>
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
