import { useStore } from "../store";
import { bundleUrl } from "../api";

const statusLabel: Record<string, string> = {
  queued: "排队中",
  running: "处理中",
  succeeded: "已完成",
  failed: "失败",
};

export default function HistoryList() {
  const { tasks, selectedId, setSelected, refreshTasks } = useStore();

  return (
    <section className="rounded-lg bg-white shadow-sm border border-gray-200 p-4">
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-gray-700">历史任务</h2>
        <button
          type="button"
          onClick={() => void refreshTasks()}
          className="text-xs text-blue-600 hover:underline"
        >
          刷新
        </button>
      </div>

      {tasks.length === 0 ? (
        <div className="py-8 text-center text-sm text-gray-400">暂无处理记录</div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-left text-xs">
            <thead>
              <tr className="border-b border-gray-200 text-gray-500">
                <th className="py-2 pr-3 font-medium">任务</th>
                <th className="py-2 pr-3 font-medium">参数</th>
                <th className="py-2 pr-3 font-medium">状态</th>
                <th className="py-2 pr-3 font-medium">耗时</th>
                <th className="py-2 pr-3 font-medium">时间</th>
                <th className="py-2 font-medium">操作</th>
              </tr>
            </thead>
            <tbody>
              {tasks.map((t) => (
                <tr
                  key={t.id}
                  onClick={() => setSelected(t.id)}
                  className={`cursor-pointer border-b border-gray-100 ${
                    selectedId === t.id ? "bg-blue-50" : "hover:bg-gray-50"
                  }`}
                >
                  <td className="py-2 pr-3 font-mono text-gray-700">{t.id}</td>
                  <td className="py-2 pr-3 text-gray-600">
                    {t.backgrounds.join("、")} · {t.size}
                    {t.outputs.length > 0 ? `（${t.outputs.length} 个产物）` : ""}
                  </td>
                  <td className="py-2 pr-3">
                    <span
                      className={`inline-block rounded-full px-2 py-0.5 ${
                        t.status === "succeeded"
                          ? "bg-green-100 text-green-700"
                          : t.status === "failed"
                            ? "bg-red-100 text-red-700"
                            : "bg-blue-100 text-blue-700"
                      }`}
                    >
                      {statusLabel[t.status] ?? t.status}
                    </span>
                    {t.message && t.status === "failed" && (
                      <div className="mt-0.5 max-w-[260px] truncate text-red-500" title={t.message}>
                        {t.message}
                      </div>
                    )}
                  </td>
                  <td className="py-2 pr-3 text-gray-500">
                    {t.elapsed_ms != null ? `${(t.elapsed_ms / 1000).toFixed(2)}s` : "-"}
                  </td>
                  <td className="py-2 pr-3 text-gray-500">{t.created_at}</td>
                  <td className="py-2">
                    {t.outputs.length > 0 && (
                      <div className="flex items-center gap-2">
                        <button
                          type="button"
                          onClick={(e) => {
                            e.stopPropagation();
                            setSelected(t.id);
                          }}
                          className="text-blue-600 hover:underline"
                        >
                          预览
                        </button>
                        <a
                          href={bundleUrl(t.id)}
                          download={`${t.id}_bundle.zip`}
                          onClick={(e) => e.stopPropagation()}
                          className="text-blue-600 hover:underline"
                        >
                          下载zip
                        </a>
                      </div>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
