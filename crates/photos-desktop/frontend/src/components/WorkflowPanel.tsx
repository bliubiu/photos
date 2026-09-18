// 工作流编排面板：按步骤表开关与调序，支持自定义步骤（服务端 `[pipeline.custom]`）。
// 契约：docs/05-API契约.md（提交参数 steps）与 GET /config 的 pipeline 字段。

import { useStore } from "../store";

export default function WorkflowPanel() {
  const { config, params, setParams } = useStore();
  const metas = config?.pipeline.steps ?? [];
  const effective = config?.pipeline.effective ?? [];
  // null = 沿用服务端全局配置 / 内置默认十步
  const steps = params.steps ?? effective;
  const enabled = steps.filter((s) => metas.some((m) => m.id === s));
  const isOn = (id: string) => enabled.includes(id);
  const labelOf = (id: string) => metas.find((m) => m.id === id)?.label ?? id;

  /** 启用步骤时按其默认顺序插入，关闭时移除 */
  const toggle = (id: string) => {
    if (isOn(id)) {
      setParams({ steps: enabled.filter((s) => s !== id) });
      return;
    }
    const pos = metas.findIndex((m) => m.id === id);
    const at = enabled.filter((s) => metas.findIndex((m) => m.id === s) < pos).length;
    setParams({ steps: [...enabled.slice(0, at), id, ...enabled.slice(at)] });
  };

  /** 与相邻步骤交换位置 */
  const move = (id: string, delta: number) => {
    const i = enabled.indexOf(id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= enabled.length) return;
    const next = [...enabled];
    [next[i], next[j]] = [next[j], next[i]];
    setParams({ steps: next });
  };

  // 依赖缺失与请求冲突提示（服务端同样会校验并告警）
  const missing = enabled.flatMap((s) => {
    const meta = metas.find((m) => m.id === s);
    return (meta?.requires ?? [])
      .filter((r) => !isOn(r))
      .map((r) => `「${labelOf(s)}」缺少依赖步骤「${labelOf(r)}」`);
  });
  const conflicts = [
    params.beautyEnabled && !isOn("beauty") ? "已开启 AI 美颜，但「美颜」步骤未启用，该请求将被忽略" : null,
    params.layout && !isOn("layout") ? "已选择排版，但「排版」步骤未启用，该请求将被忽略" : null,
    (params.transparent || params.bgImage) && !isOn("background")
      ? "已选择透明底或自定义背景，但「换底裁切」步骤未启用，该请求将被忽略"
      : null,
  ].filter((s): s is string => Boolean(s));

  return (
    <section className="rounded-lg bg-white p-5 shadow-sm">
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-gray-700">工作流编排</h2>
        <button
          type="button"
          disabled={params.steps === null}
          onClick={() => setParams({ steps: null })}
          className="rounded-md border border-gray-200 px-2 py-1 text-xs text-gray-600 transition hover:border-gray-300 disabled:opacity-50"
        >
          恢复默认
        </button>
      </div>

      <p className="mb-3 text-[11px] text-gray-400">
        {params.steps === null
          ? `当前沿用默认步骤表（${effective.length} 步），调整后即自定义`
          : `当前为自定义步骤表（${enabled.length} 步）`}
      </p>

      <div className="space-y-1">
        {enabled.map((id, i) => (
          <div
            key={id}
            className="flex items-center gap-2 rounded-md border border-gray-200 px-2 py-1.5 text-xs"
          >
            <span className="w-4 text-right text-gray-400">{i + 1}</span>
            <span className="flex-1 text-gray-800">{labelOf(id)}</span>
            <button
              type="button"
              disabled={i === 0}
              onClick={() => move(id, -1)}
              className="rounded border border-gray-200 px-1.5 text-gray-600 transition hover:border-gray-300 disabled:opacity-40"
            >
              ↑
            </button>
            <button
              type="button"
              disabled={i === enabled.length - 1}
              onClick={() => move(id, 1)}
              className="rounded border border-gray-200 px-1.5 text-gray-600 transition hover:border-gray-300 disabled:opacity-40"
            >
              ↓
            </button>
            <button
              type="button"
              onClick={() => toggle(id)}
              className="rounded border border-red-200 px-1.5 text-red-600 transition hover:border-red-300"
            >
              关闭
            </button>
          </div>
        ))}
      </div>

      <div className="mt-3 space-y-1">
        <h3 className="text-xs font-medium text-gray-500">已关闭步骤</h3>
        {metas.filter((m) => !isOn(m.id)).length === 0 ? (
          <p className="text-[11px] text-gray-400">无（全部步骤已启用）</p>
        ) : (
          metas
            .filter((m) => !isOn(m.id))
            .map((m) => (
              <div
                key={m.id}
                className="flex items-center gap-2 rounded-md bg-gray-50 px-2 py-1.5 text-xs text-gray-500"
              >
                <span className="flex-1">{m.label}</span>
                <button
                  type="button"
                  onClick={() => toggle(m.id)}
                  className="rounded border border-gray-300 px-1.5 text-gray-700 transition hover:border-gray-400"
                >
                  启用
                </button>
              </div>
            ))
        )}
      </div>

      {(missing.length > 0 || conflicts.length > 0) && (
        <div className="mt-3 space-y-1 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-[11px] text-amber-700">
          {missing.map((t) => (
            <p key={t}>依赖提示：{t}</p>
          ))}
          {conflicts.map((t) => (
            <p key={t}>冲突提示：{t}</p>
          ))}
        </div>
      )}
    </section>
  );
}