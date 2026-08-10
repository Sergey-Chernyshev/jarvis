import { useState } from 'react';
import { Activity, CalendarDays, Coins, Gauge, Zap } from 'lucide-react';
import { motion } from 'motion/react';
import type { UsageSummary } from '../../core/client/types';
import { useAppStore } from '../../app/app-store';
import { Segmented } from '../../shared/ui/controls';

const compact = (value: number) =>
  new Intl.NumberFormat('ru', { notation: 'compact', maximumFractionDigits: 1 }).format(value);

export function StatsPage() {
  const snapshot = useAppStore((state) => state.snapshot)!;
  const client = useAppStore((state) => state.client);
  const patch = useAppStore((state) => state.patchSnapshot);
  const [loading, setLoading] = useState(false);
  const usage = snapshot.usage;
  const max = Math.max(...usage.points.map((point) => point.claudeTokens + point.codexTokens));

  const setPeriod = async (period: UsageSummary['period']) => {
    setLoading(true);
    const next = await client.updateUsagePeriod(period);
    patch((state) => { state.usage = next; });
    setLoading(false);
  };

  return (
    <section className={`page stats-page ${loading ? 'is-loading' : ''}`}>
      <header className="page-header">
        <div>
          <span className="eyebrow">Локальная телеметрия</span>
          <h1>Статистика</h1>
          <p>Токены, лимиты и активность без содержимого переписки</p>
        </div>
        <Segmented
          label="Период статистики"
          value={usage.period}
          onChange={(period) => void setPeriod(period)}
          options={[
            { value: 'today', label: 'Сегодня' },
            { value: '7d', label: '7 дней' },
            { value: '30d', label: '30 дней' },
          ]}
        />
      </header>
      <div className="stats-ledger">
        <div><Zap size={16} /><span>Всего токенов</span><strong>{compact(usage.totalTokens)}</strong><small>{compact(usage.planTokens)} в подписке</small></div>
        <div><Activity size={16} /><span>Сессии</span><strong>{usage.sessions}</strong><small>Claude и Codex</small></div>
        <div><Coins size={16} /><span>API сверх плана</span><strong>${usage.apiCost.toFixed(2)}</strong><small>только Codex API</small></div>
      </div>
      <section className="activity-section">
        <header><div><h2>Ритм работы</h2><p>Claude · Codex по дням</p></div><span><CalendarDays size={14} /> текущий период</span></header>
        <div className="usage-chart">
          {usage.points.map((point) => {
            const total = point.claudeTokens + point.codexTokens;
            return (
              <div className="chart-column" key={point.date}>
                <div className="chart-value">{compact(total)}</div>
                <div className="chart-bar">
                  <motion.i className="chart-claude" initial={{ height: 0 }} animate={{ height: `${(point.claudeTokens / max) * 100}%` }} />
                  <motion.i className="chart-codex" initial={{ height: 0 }} animate={{ height: `${(point.codexTokens / max) * 100}%` }} />
                </div>
                <strong>{point.date}</strong><small>{point.sessions} сессий</small>
              </div>
            );
          })}
        </div>
        <div className="chart-legend"><span><i className="is-claude" /> Claude</span><span><i className="is-codex" /> Codex</span></div>
      </section>
      <section className="limits-section">
        <header><div><h2>Лимиты</h2><p>Состояние аккаунта Claude · Max plan</p></div><Gauge size={18} /></header>
        {usage.limits.map((limit) => (
          <div className="limit-row" key={limit.id}>
            <span><strong>{limit.label}</strong><small>{limit.resetAt}</small></span>
            <div className="limit-track"><motion.i initial={{ width: 0 }} animate={{ width: `${limit.usedPercent}%` }} /></div>
            <b>{limit.usedPercent}%</b>
          </div>
        ))}
      </section>
    </section>
  );
}
