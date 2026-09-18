import { useCallback, useEffect, useState } from "react";
import { ErrorItem, MetricsSummary, fetchErrors, fetchMetrics } from "../api";

/** 耗时展示（毫秒 → 秒，保留两位） */
function seconds(ms: number): string {
  return `${(ms / 1000).toFixed(2)}s`;
}

/** 统计卡片（标题 + 数值） */
function Stat(props: { label: string; value: string | number; tone?: string }) {
  return (
    <div className="rounded-md border border-gray-200 px-3 py-2">
      <div className="text-xs text-gray-500">{props.label}</div>
      <div className={`text-lg font-semibold ${props.tone ?? "text-gray-800"}`}>{props.value}</div>
    </div>
  );
}

/** 可观测性面板：任务统计、平均耗时、各阶段平均耗时与最近错误（数据来自 GET /metrics、GET /errors） */
export default function ObservabilityPanel() {
  const [metrics, setMetrics] = useState<MetricsSummary | null>(null);
  const [errors, setErrors] = useState<ErrorItem[]>([]);
  const [errorTotal, setErrorTotal] = useState(0);
  const [failed, setFailed] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [m, e] = await Promise.all([fetchMetrics(), fetchErrors(10)]);
      setMetrics(m);
      setErrors(e.items);
      setErrorTotal(e.total);
      setFailed(null);
    } catch (err) {
      setFailed((err as Error).message);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // 各阶段平均耗时占最大值的比例（用于条形宽度，仅展示不引入图表库）
  const maxAvg = Math.max(1, ...(metrics?.stages ?? []).map((s) => s.avg_ms));

  return (
    <section className="rounded-lg bg-white shadow-sm border border-gray-200 p-4">
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-gray-700">运行指标</h2>
        <button
          type="button"
          onClick={() => void refresh()}
          disabled={loading}
          className="rounded-md border border-gray-300 px-2 py-1 text-xs text-gray-600 disabled:opacity-50"
        >
          {loading ? "刷新中…" : "刷新"}
        </button>
      </div>

      {failed && (
        <div className="mb-3 rounded-md bg-red-50 border border-red-200 px-3 py-2 text-xs text-red-700">
          {failed}
        </div>
      )}

      {!metrics ? (
        <div className="py-6 text-center text-sm text-gray-400">指标加载中…</div>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
            <Stat label="任务总数" value={metrics.tasks.total} />
            <Stat label="已完成" value={metrics.tasks.succeeded} tone="text-green-700" />
            <Stat label="失败" value={metrics.tasks.failed} tone="text-red-700" />
            <Stat label="处理中" value={metrics.tasks.queued + metrics.tasks.running} />
          </div>

          <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-4">
            <Stat
              label={`平均总耗时（${metrics.elapsed_ms.samples} 次）`}
              value={metrics.elapsed_ms.samples > 0 ? seconds(metrics.elapsed_ms.avg) : "-"}
            />
            <Stat label="错误记录" value={metrics.errors.total} tone="text-red-700" />
          </div>

          <div className="mt-4">
            <h3 className="mb-2 text-xs font-semibold text-gray-600">各阶段平均耗时</h3>
            {metrics.stages.length === 0 ? (
              <div className="text-xs text-gray-400">暂无已完成任务的分阶段数据</div>
            ) : (
              <ul className="space-y-1.5">
                {metrics.stages.map((s) => (
                  <li key={s.stage} className="flex items-center gap-2 text-xs text-gray-600">
                    <span className="w-24 shrink-0 truncate">{s.stage}</span>
                    <span className="h-2 flex-1 rounded-full bg-gray-100">
                      <span
                        className="block h-2 rounded-full bg-blue-400"
                        style={{ width: `${Math.max(2, (s.avg_ms / maxAvg) * 100)}%` }}
                      />
                    </span>
                    <span className="w-16 shrink-0 text-right tabular-nums">
                      {s.avg_ms.toFixed(1)}ms
                    </span>
                    <span className="w-12 shrink-0 text-right text-gray-400">{s.samples} 次</span>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="mt-4">
            <h3 className="mb-2 text-xs font-semibold text-gray-600">
              最近错误（共 {errorTotal} 条）
            </h3>
            {errors.length === 0 ? (
              <div className="text-xs text-gray-400">暂无错误记录</div>
            ) : (
              <ul className="space-y-1">
                {errors.map((e) => (
                  <li
                    key={e.id}
                    className="rounded-md border border-gray-200 px-2 py-1.5 text-xs text-gray-600"
                  >
                    <div className="flex items-center gap-2 text-gray-400">
                      <span className="rounded bg-gray-100 px-1 py-0.5">{e.code}</span>
                      <span>{e.stage}</span>
                      <span className="ml-auto">{e.created_at}</span>
                    </div>
                    <div className="mt-1 break-all text-gray-700">{e.message}</div>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </>
      )}
    </section>
  );
}
