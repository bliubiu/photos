import { useStore } from "../store";
import { bundleUrl } from "../api";

const statusLabel: Record<string, string> = {
  queued: "排队中",
  running: "处理中",
  succeeded: "已完成",
  failed: "失败",
};

/** 筛选下拉（空值 = 不过滤） */
function FilterSelect(props: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  options: { id: string; label: string }[];
}) {
  return (
    <label className="flex items-center gap-1 text-xs text-gray-500">
      {props.label}
      <select
        value={props.value}
        onChange={(e) => props.onChange(e.target.value)}
        className="rounded-md border border-gray-300 px-1.5 py-1 text-xs"
      >
        <option value="">全部</option>
        {props.options.map((o) => (
          <option key={o.id} value={o.id}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
}

export default function HistoryList() {
  const {
    config,
    tasks,
    selectedId,
    taskFilter,
    setSelected,
    setTaskFilter,
    refreshTasks,
    reuseParams,
    deleteTask,
    clearTasks,
  } = useStore();

  const modes = config?.modes ?? [];
  const sizes = (config?.sizes ?? []).map((s) => ({ id: s.id, label: s.name }));
  const backgrounds = (config?.backgrounds ?? []).map((b) => ({ id: b.id, label: b.name }));

  const onDelete = (id: string) => {
    if (window.confirm(`确定删除任务 ${id} 及其产物文件？此操作不可恢复。`)) {
      void deleteTask(id);
    }
  };
  const onClear = () => {
    if (window.confirm("确定清空全部历史记录？将同时删除磁盘上的产物与上传原图，此操作不可恢复。")) {
      void clearTasks();
    }
  };

  return (
    <section className="rounded-lg bg-white shadow-sm border border-gray-200 p-4">
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-gray-700">历史任务</h2>
        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={() => void refreshTasks()}
            className="text-xs text-blue-600 hover:underline"
          >
            刷新
          </button>
          <button
            type="button"
            onClick={onClear}
            disabled={tasks.length === 0}
            className="text-xs text-red-600 hover:underline disabled:text-gray-300 disabled:no-underline"
          >
            清空历史
          </button>
        </div>
      </div>

      <div className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-2">
        <FilterSelect
          label="状态"
          value={taskFilter.status}
          onChange={(v) => setTaskFilter({ status: v })}
          options={Object.entries(statusLabel).map(([id, label]) => ({ id, label }))}
        />
        <FilterSelect
          label="模式"
          value={taskFilter.mode}
          onChange={(v) => setTaskFilter({ mode: v })}
          options={modes}
        />
        <FilterSelect
          label="尺寸"
          value={taskFilter.size}
          onChange={(v) => setTaskFilter({ size: v })}
          options={sizes}
        />
        <FilterSelect
          label="底色"
          value={taskFilter.background}
          onChange={(v) => setTaskFilter({ background: v })}
          options={backgrounds}
        />
        <label className="flex items-center gap-1 text-xs text-gray-500">
          起始日期
          <input
            type="date"
            value={taskFilter.since}
            onChange={(e) => setTaskFilter({ since: e.target.value })}
            className="rounded-md border border-gray-300 px-1.5 py-1 text-xs"
          />
        </label>
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
                    <div className="flex items-center gap-2">
                      {t.outputs.length > 0 && (
                        <>
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
                        </>
                      )}
                      <button
                        type="button"
                        onClick={(e) => {
                          e.stopPropagation();
                          void reuseParams(t.id);
                        }}
                        className="text-blue-600 hover:underline"
                      >
                        复用参数
                      </button>
                      <button
                        type="button"
                        onClick={(e) => {
                          e.stopPropagation();
                          onDelete(t.id);
                        }}
                        className="text-red-600 hover:underline"
                      >
                        删除
                      </button>
                    </div>
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