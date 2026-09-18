import { useStore } from "../store";
import type { BatchStatus } from "../store";

const statusLabel: Record<BatchStatus, string> = {
  uploading: "上传中",
  processing: "处理中",
  succeeded: "已完成",
  failed: "失败",
};

const statusClass: Record<BatchStatus, string> = {
  uploading: "bg-gray-100 text-gray-600",
  processing: "bg-blue-100 text-blue-700",
  succeeded: "bg-green-100 text-green-700",
  failed: "bg-red-100 text-red-700",
};

export default function BatchPanel() {
  const { batch, submitting, retryFailed, setSelected } = useStore();
  if (batch.length === 0) return null;

  const total = batch.length;
  const settled = batch.filter((i) => i.status === "succeeded" || i.status === "failed");
  const failed = batch.filter((i) => i.status === "failed");
  const percent = Math.round((settled.length / total) * 100);

  // 预计剩余时间：按已完成项的平均耗时 × 剩余项数
  const elapsed = settled
    .map((i) => i.elapsed_ms)
    .filter((v): v is number => v != null && v > 0);
  const remaining = total - settled.length;
  const etaMs =
    elapsed.length > 0 && remaining > 0
      ? (elapsed.reduce((a, b) => a + b, 0) / elapsed.length) * remaining
      : null;

  return (
    <section className="rounded-lg bg-white shadow-sm border border-gray-200 p-4 space-y-3">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-gray-700">批量进度</h2>
        <span className="text-xs text-gray-500">
          {settled.length}/{total}
          {etaMs != null && ` · 预计剩余约 ${Math.ceil(etaMs / 1000)} 秒`}
        </span>
      </div>

      <div className="h-2 w-full overflow-hidden rounded-full bg-gray-100">
        <div
          className="h-full rounded-full bg-blue-500 transition-all"
          style={{ width: `${percent}%` }}
        />
      </div>

      {failed.length > 0 && (
        <button
          type="button"
          disabled={submitting}
          onClick={() => void retryFailed()}
          className="w-full rounded-md bg-amber-600 px-3 py-1.5 text-xs font-medium text-white transition hover:bg-amber-700 disabled:opacity-50"
        >
          重试失败项（{failed.length}）
        </button>
      )}

      <ul className="max-h-48 space-y-1 overflow-auto text-xs">
        {batch.map((item, i) => (
          <li key={i} className="flex items-center justify-between gap-2 rounded bg-gray-50 px-2 py-1">
            <button
              type="button"
              disabled={!item.id}
              onClick={() => item.id && setSelected(item.id)}
              className="truncate text-left text-gray-600 disabled:cursor-default"
              title={item.name}
            >
              {item.name}
            </button>
            <span
              className={`shrink-0 rounded-full px-2 py-0.5 ${statusClass[item.status]}`}
              title={item.message ?? undefined}
            >
              {statusLabel[item.status]}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}