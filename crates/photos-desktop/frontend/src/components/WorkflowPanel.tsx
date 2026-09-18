// 工作流编排面板（图形化）：以分层 DAG 渲染步骤节点与 requires 依赖连线。
// 交互：节点内「关闭」停用步骤、同层 ←/→ 调序、「恢复默认」置空 steps 回到服务端配置。
// 数据源：GET /config 的 pipeline.steps（元数据）与 pipeline.effective（生效顺序）。

import { useState } from "react";
import { useStore } from "../store";

/** 画布节点尺寸与间距（坐标按层与同层次序算出，绝对定位以便位置过渡动画） */
const NODE_W = 176;
const NODE_H = 56;
const GAP_X = 20;
const GAP_Y = 46;
const MIN_CANVAS_W = 480;

export default function WorkflowPanel() {
  const { config, params, setParams } = useStore();
  const [hover, setHover] = useState<string | null>(null);

  const metas = config?.pipeline.steps ?? [];
  const effective = config?.pipeline.effective ?? [];
  // null = 沿用服务端全局配置 `[pipeline] steps` / 内置默认十步
  const steps = params.steps ?? effective;
  const enabled = steps.filter((s) => metas.some((m) => m.id === s));
  const metaOf = (id: string) => metas.find((m) => m.id === id);
  const isOn = (id: string) => enabled.includes(id);
  const labelOf = (id: string) => metaOf(id)?.label ?? id;

  /** 分层：layer = 启用依赖的最大层 + 1（依赖关系由服务端校验，无环） */
  const layers = new Map<string, number>();
  const layerOf = (id: string): number => {
    const cached = layers.get(id);
    if (cached !== undefined) return cached;
    layers.set(id, 0); // 防环兜底
    const deps = (metaOf(id)?.requires ?? []).filter(isOn);
    const l = deps.length === 0 ? 0 : 1 + Math.max(...deps.map(layerOf));
    layers.set(id, l);
    return l;
  };

  const byLayer = new Map<number, string[]>();
  for (const id of enabled) {
    const l = layerOf(id);
    const list = byLayer.get(l);
    if (list) list.push(id);
    else byLayer.set(l, [id]);
  }
  const maxLayer = Math.max(-1, ...byLayer.keys());
  const rows: string[][] = [];
  for (let l = 0; l <= maxLayer; l += 1) rows.push(byLayer.get(l) ?? []);

  const rowWidth = (n: number) => n * NODE_W + (n - 1) * GAP_X;
  const canvasW = Math.max(MIN_CANVAS_W, ...rows.map((r) => rowWidth(r.length)));
  const canvasH = rows.length === 0 ? 0 : rows.length * NODE_H + (rows.length - 1) * GAP_Y;

  /** 节点坐标：同层按执行顺序左→右、整层居中 */
  const pos = new Map<string, { x: number; y: number }>();
  rows.forEach((row, l) => {
    const start = (canvasW - rowWidth(row.length)) / 2;
    row.forEach((id, i) => pos.set(id, { x: start + i * (NODE_W + GAP_X), y: l * (NODE_H + GAP_Y) }));
  });

  /** 依赖连线：贝塞尔曲线，仅画启用步骤之间的依赖 */
  const edges = enabled.flatMap((id) =>
    (metaOf(id)?.requires ?? [])
      .filter(isOn)
      .map((from) => {
        const a = pos.get(from);
        const b = pos.get(id);
        if (!a || !b) return null;
        const x1 = a.x + NODE_W / 2;
        const y1 = a.y + NODE_H;
        const x2 = b.x + NODE_W / 2;
        const y2 = b.y;
        const my = (y1 + y2) / 2;
        return {
          key: `${from}->${id}`,
          from,
          to: id,
          d: `M ${x1} ${y1} C ${x1} ${my}, ${x2} ${my}, ${x2} ${y2}`,
        };
      })
      .filter((e): e is NonNullable<typeof e> => e !== null),
  );

  /** 启用步骤时按其内置顺序插入，关闭时移除（至少保留一个步骤） */
  const toggle = (id: string) => {
    if (isOn(id)) {
      if (enabled.length <= 1) return;
      setParams({ steps: enabled.filter((s) => s !== id) });
      return;
    }
    const at = metas.findIndex((m) => m.id === id);
    const idx = enabled.filter((s) => metas.findIndex((m) => m.id === s) < at).length;
    setParams({ steps: [...enabled.slice(0, idx), id, ...enabled.slice(idx)] });
  };

  /** 同层调序（跨层交换会破坏依赖，服务端会拒绝） */
  const moveInLayer = (id: string, delta: -1 | 1) => {
    const row = rows[layerOf(id)] ?? [];
    const i = row.indexOf(id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= row.length) return;
    const next = [...enabled];
    const a = next.indexOf(id);
    const b = next.indexOf(row[j]);
    const tmp = next[a];
    next[a] = next[b];
    next[b] = tmp;
    setParams({ steps: next });
  };

  // 依赖缺失与请求冲突提示（服务端同样会校验并告警）
  const missing = enabled.flatMap((id) =>
    (metaOf(id)?.requires ?? [])
      .filter((r) => !isOn(r))
      .map((r) => `「${labelOf(id)}」缺少依赖步骤「${labelOf(r)}」`),
  );
  const conflicts = [
    params.beautyEnabled && !isOn("beauty")
      ? "已开启 AI 美颜，但「美颜」步骤未启用，该请求将被忽略"
      : null,
    params.layout && !isOn("layout")
      ? "已选择排版，但「排版」步骤未启用，该请求将被忽略"
      : null,
    (params.transparent || params.bgImage) && !isOn("background")
      ? "已选择透明底或自定义背景，但「换底裁切」步骤未启用，该请求将被忽略"
      : null,
  ].filter((s): s is string => Boolean(s));

  const offSteps = metas.filter((m) => !isOn(m.id));

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
        ，箭头为依赖方向，同一行内可用 ←/→ 调序
      </p>

      <div className="overflow-x-auto">
        <div className="relative mx-auto" style={{ width: canvasW, height: canvasH }}>
          <svg
            className="pointer-events-none absolute inset-0"
            width={canvasW}
            height={canvasH}
            aria-hidden="true"
          >
            <defs>
              <marker
                id="wf-arrow"
                viewBox="0 0 10 10"
                refX="9"
                refY="5"
                markerWidth="6"
                markerHeight="6"
                orient="auto"
              >
                <path d="M 0 0 L 10 5 L 0 10 z" fill="#cbd5e1" />
              </marker>
              <marker
                id="wf-arrow-active"
                viewBox="0 0 10 10"
                refX="9"
                refY="5"
                markerWidth="6"
                markerHeight="6"
                orient="auto"
              >
                <path d="M 0 0 L 10 5 L 0 10 z" fill="#3b82f6" />
              </marker>
            </defs>
            {edges.map((e) => {
              const active = hover === e.from || hover === e.to;
              return (
                <path
                  key={e.key}
                  d={e.d}
                  fill="none"
                  stroke={active ? "#3b82f6" : "#cbd5e1"}
                  strokeWidth={active ? 2 : 1.5}
                  markerEnd={active ? "url(#wf-arrow-active)" : "url(#wf-arrow)"}
                />
              );
            })}
          </svg>

          {enabled.map((id) => {
            const meta = metaOf(id);
            const p = pos.get(id);
            if (!meta || !p) return null;
            const row = rows[layerOf(id)] ?? [];
            const i = row.indexOf(id);
            const lack = (meta.requires ?? []).some((r) => !isOn(r));
            return (
              <div
                key={id}
                onMouseEnter={() => setHover(id)}
                onMouseLeave={() => setHover(null)}
                style={{
                  left: p.x,
                  top: p.y,
                  width: NODE_W,
                  height: NODE_H,
                  transition: "left 300ms ease, top 300ms ease",
                }}
                className={`absolute rounded-lg border px-2 py-1.5 shadow-sm transition-colors ${
                  lack
                    ? "animate-pulse border-red-300 bg-red-50"
                    : hover === id
                      ? "border-blue-400 bg-white"
                      : "border-gray-300 bg-white"
                }`}
              >
                <div className="flex items-center justify-between gap-1">
                  <span className="truncate text-xs font-medium text-gray-800">{meta.label}</span>
                  <div className="flex shrink-0 gap-0.5">
                    <button
                      type="button"
                      disabled={i <= 0}
                      onClick={() => moveInLayer(id, -1)}
                      className="rounded border border-gray-200 px-1 text-[11px] text-gray-600 transition hover:border-gray-300 disabled:opacity-40"
                    >
                      ←
                    </button>
                    <button
                      type="button"
                      disabled={i < 0 || i >= row.length - 1}
                      onClick={() => moveInLayer(id, 1)}
                      className="rounded border border-gray-200 px-1 text-[11px] text-gray-600 transition hover:border-gray-300 disabled:opacity-40"
                    >
                      →
                    </button>
                  </div>
                </div>
                <div className="mt-1 flex items-center justify-between gap-1 text-[10px] text-gray-400">
                  <span className="truncate">
                    第 {enabled.indexOf(id) + 1} 步 · {meta.stage}
                  </span>
                  <button
                    type="button"
                    disabled={enabled.length <= 1}
                    onClick={() => toggle(id)}
                    className="shrink-0 rounded border border-red-200 px-1 text-red-600 transition hover:border-red-300 disabled:opacity-40"
                  >
                    关闭
                  </button>
                </div>
              </div>
            );
          })}

          {enabled.length === 0 && (
            <p className="py-8 text-center text-sm text-gray-400">
              至少需启用一个步骤：在下方「已关闭步骤」中点击「启用」。
            </p>
          )}
        </div>
      </div>

      <div className="mt-4 space-y-1">
        <h3 className="text-xs font-medium text-gray-500">已关闭步骤</h3>
        {offSteps.length === 0 ? (
          <p className="text-[11px] text-gray-400">无（全部步骤已启用）</p>
        ) : (
          offSteps.map((m) => (
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