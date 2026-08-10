import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { Check, X } from 'lucide-react';

export function Button({
  children,
  variant = 'ghost',
  className = '',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: 'ghost' | 'primary' | 'danger' | 'quiet';
}) {
  return (
    <button className={`button button--${variant} ${className}`} {...props}>
      {children}
    </button>
  );
}

export function IconButton({
  label,
  children,
  className = '',
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  label: string;
  children: ReactNode;
}) {
  return (
    <button
      className={`icon-button ${className}`}
      aria-label={label}
      title={label}
      {...props}
    >
      {children}
    </button>
  );
}

export function Segmented<T extends string | number>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: Array<{ value: T; label: string }>;
  onChange: (value: T) => void;
  label: string;
}) {
  return (
    <div className="segmented" role="group" aria-label={label}>
      {options.map((option) => (
        <button
          type="button"
          key={option.value}
          className={option.value === value ? 'is-active' : ''}
          aria-pressed={option.value === value}
          onClick={() => onChange(option.value)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export function Switch({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      className={`switch ${checked ? 'is-on' : ''}`}
      onClick={() => onChange(!checked)}
    >
      <span />
    </button>
  );
}

export function StatusDot({
  status,
  pulse = false,
}: {
  status: 'active' | 'waiting' | 'done' | 'error' | 'idle';
  pulse?: boolean;
}) {
  return (
    <span
      className={`status-dot status-dot--${status} ${pulse ? 'is-pulsing' : ''}`}
      aria-hidden="true"
    />
  );
}

export function SettingRow({
  title,
  description,
  control,
  danger = false,
}: {
  title: string;
  description?: string;
  control: ReactNode;
  danger?: boolean;
}) {
  return (
    <div className={`setting-row ${danger ? 'is-danger' : ''}`}>
      <div className="setting-copy">
        <strong>{title}</strong>
        {description && <span>{description}</span>}
      </div>
      <div className="setting-control">{control}</div>
    </div>
  );
}

export function EmptyState({
  icon,
  title,
  body,
  action,
}: {
  icon: ReactNode;
  title: string;
  body: string;
  action?: ReactNode;
}) {
  return (
    <div className="empty-state">
      <div className="empty-state__orbit">{icon}</div>
      <h2>{title}</h2>
      <p>{body}</p>
      {action}
    </div>
  );
}

export function SelectionMark({ selected }: { selected: boolean }) {
  return (
    <span className={`selection-mark ${selected ? 'is-selected' : ''}`}>
      {selected && <Check size={12} strokeWidth={2.5} />}
    </span>
  );
}

export function CloseButton({ onClick, label = 'Закрыть' }: { onClick: () => void; label?: string }) {
  return (
    <IconButton label={label} onClick={onClick}>
      <X size={16} />
    </IconButton>
  );
}
