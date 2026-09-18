import { useState } from "react";
import { useStore } from "../store";
import { bundleUrl, inputUrl, outputUrl } from "../api";
import type { Artifact } from "../api";

function StatusBadge({ status }: { status: string }) {
  const map: Record<string, string> = {
    queued: "bg-gray-100 text-gray-600",
    running: "bg-blue-100 text-blue-700",
    succeeded: "bg-green-100 text-green-700",
    failed: "bg-red-100 text-red-700",
  };
  const label: Record<string, string> = {
    queued: "排队中",
    running: "处理中",
    succeeded: "已完成",
    failed: "失败",
  };
  return (
    <span className={`inline-block rounded-full px-2 py-0.5 text-xs ${map[status] ?? "bg-gray-100 text-gray-600"}`}>
      {label[status] ?? status}
    </span>
  );
}

/** 底色标签：自定义背景图与透明底使用中文名，其余沿用配置 id */
function bgLabel(id?: string | null): string {
  if (id === "transparent") return "透明底";
  if (id === "custombg") return "自定义背景";
  return id ?? "";
}

export default function PreviewPanel() {
  const { detail, selectedId, files } = useStore();
  const [tab, setTab] = useState<Artifact | null>(null);
  // 是否叠加显示上传原图（前后对比滑块）
  const [compare, setCompare] = useState(false);
  // 对比分割线位置（百分比，左侧为原图）
  const [splitPos, setSplitPos] = useState(50);

  if (!selectedId) {
    return (
      <section className="flex h-full min-h-[360px] items-center justify-center rounded-lg border border-dashed border-gray-300 bg-white text-sm text-gray-400">
        {files.length > 0 ? "设置参数后点击「开始处理」" : "请上传图片后开始处理"}
      </section>
    );
  }
  if (!detail) {
    return (
      <section className="flex h-full min-h-[360px] items-center justify-center rounded-lg border border-dashed border-gray-300 bg-white text-sm text-gray-400">
        任务加载中…
      </section>
    );
  }

  const idPhotos = detail.artifacts.filter((a) => a.kind === "id_photo");
  const layouts = detail.artifacts.filter((a) => a.kind === "layout");
  const effects = detail.artifacts.filter((a) => a.kind === "effect");
  const active = tab ?? idPhotos[0] ?? null;

  const elapsed = detail.elapsed_ms != null ? (detail.elapsed_ms / 1000).toFixed(2) : "-";
  // 当前产物下载地址（按钮与图片共用）
  const resultSrc = active
    ? outputUrl(selectedId, active.kind, active.background ?? undefined, active.layout ?? undefined)
    : "";

  return (
    <section className="rounded-lg bg-white shadow-sm border border-gray-200 p-4">
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-gray-700">3. 预览与下载</h2>
        <div className="flex items-center gap-2 text-xs text-gray-500">
          {detail.artifacts.length > 0 && (
            <label className="flex items-center gap-1">
              <input
                type="checkbox"
                checked={compare}
                onChange={(e) => setCompare(e.target.checked)}
              />
              对比原图（滑块）
            </label>
          )}
          <StatusBadge status={detail.status} />
          <span>耗时 {elapsed}s</span>
        </div>
      </div>

      {detail.warnings.length > 0 && (
        <div className="mb-3 rounded-md bg-amber-50 border border-amber-200 px-3 py-2 text-xs text-amber-700">
          {detail.warnings.map((w, i) => (
            <div key={i}>⚠ {w}</div>
          ))}
        </div>
      )}
      {detail.status === "failed" && detail.message && (
        <div className="mb-3 rounded-md bg-red-50 border border-red-200 px-3 py-2 text-xs text-red-700">
          {detail.message}
        </div>
      )}

      {(detail.metrics ?? []).length > 0 && (
        <div className="mb-3 flex flex-wrap gap-x-3 gap-y-1 rounded-md bg-gray-50 border border-gray-200 px-3 py-2 text-xs text-gray-500">
          {(detail.metrics ?? []).map((m) => (
            <span key={m.stage}>
              {m.stage} <span className="tabular-nums text-gray-700">{m.ms.toFixed(1)}ms</span>
            </span>
          ))}
        </div>
      )}

      {detail.artifacts.length === 0 && (
        <div className="py-12 text-center text-sm text-gray-400">暂无产物</div>
      )}

      {idPhotos.length > 0 && (
        <div className="mb-2 flex flex-wrap gap-1.5">
          {idPhotos.map((a) => (
            <button
              key={`id_${a.background}`}
              type="button"
              onClick={() => setTab(a)}
              className={`rounded-md border px-2 py-1 text-xs ${
                active === a ? "border-blue-500 bg-blue-50 text-blue-700" : "border-gray-200 text-gray-600"
              }`}
            >
              证件照·{bgLabel(a.background)}
            </button>
          ))}
          {layouts.map((a) => (
            <button
              key={`lay_${a.layout}`}
              type="button"
              onClick={() => setTab(a)}
              className={`rounded-md border px-2 py-1 text-xs ${
                active === a ? "border-blue-500 bg-blue-50 text-blue-700" : "border-gray-200 text-gray-600"
              }`}
            >
              排版·{a.layout}
            </button>
          ))}
          {effects.map((a, i) => (
            <button
              key={`eff_${i}`}
              type="button"
              onClick={() => setTab(a)}
              className={`rounded-md border px-2 py-1 text-xs ${
                active === a ? "border-blue-500 bg-blue-50 text-blue-700" : "border-gray-200 text-gray-600"
              }`}
            >
              效果图·{a.background}
            </button>
          ))}
        </div>
      )}

      {active && (
        <div className="flex flex-col items-center">
          {compare ? (
            /* 前后对比滑块：结果图为底，原图按分割线裁切叠加 */
            <div className="relative select-none overflow-hidden rounded-md border border-gray-200">
              <img
                src={resultSrc}
                alt={active.filename}
                className="block max-h-[520px]"
                style={{ backgroundImage: "conic-gradient(#eee 25%, #fff 0 50%, #eee 0 75%, #fff 0)" }}
              />
              <img
                src={inputUrl(selectedId)}
                alt="上传原图"
                className="absolute inset-0 h-full w-full object-cover"
                style={{ clipPath: `inset(0 ${100 - splitPos}% 0 0)` }}
              />
              <div
                className="pointer-events-none absolute inset-y-0 w-0.5 -translate-x-1/2 bg-white shadow-[0_0_4px_rgba(0,0,0,0.6)]"
                style={{ left: `${splitPos}%` }}
              />
              <span className="pointer-events-none absolute left-2 top-2 rounded bg-black/50 px-1.5 py-0.5 text-[11px] text-white">
                原图
              </span>
              <span className="pointer-events-none absolute right-2 top-2 rounded bg-black/50 px-1.5 py-0.5 text-[11px] text-white">
                处理结果
              </span>
              <input
                type="range"
                min={0}
                max={100}
                value={splitPos}
                aria-label="拖动对比原图与处理结果"
                onChange={(e) => setSplitPos(Number(e.target.value))}
                className="absolute inset-0 h-full w-full cursor-ew-resize appearance-none bg-transparent opacity-0"
              />
            </div>
          ) : (
            <figure className="flex flex-col items-center">
              <img
                src={resultSrc}
                alt={active.filename}
                className="max-h-[520px] rounded-md border border-gray-200 bg-checker"
                style={{ backgroundImage: "conic-gradient(#eee 25%, #fff 0 50%, #eee 0 75%, #fff 0)" }}
              />
              <figcaption className="mt-1 text-xs text-gray-500">{active.filename}</figcaption>
            </figure>
          )}
          {compare && (
            <p className="mt-1 text-xs text-gray-500">拖动分割线对比（左：原图 / 右：处理结果）</p>
          )}
          <div className="mt-3 flex items-center gap-3">
            <a
              href={resultSrc}
              download={active.filename}
              className="rounded-md bg-blue-600 px-4 py-1.5 text-xs font-medium text-white hover:bg-blue-700"
            >
              下载此图
            </a>
            <a
              href={bundleUrl(selectedId)}
              download={`${selectedId}_bundle.zip`}
              className="rounded-md border border-gray-300 px-4 py-1.5 text-xs text-gray-600 hover:border-gray-400"
            >
              打包下载全部（zip）
            </a>
          </div>
        </div>
      )}
    </section>
  );
}
