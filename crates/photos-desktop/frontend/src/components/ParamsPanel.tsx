import { useState } from "react";

import { useStore } from "../store";

export default function ParamsPanel() {
  const { config, models, params, setParams, submit, submitting, downloading, downloadModels } =
    useStore();
  // 自定义底色/尺寸输入值（仅在启用时写入 store）
  const [bgColor, setBgColor] = useState("#3a7afe");
  const [sizeMm, setSizeMm] = useState({ w: 35, h: 45, dpi: 300 });
  if (!config) return <section className="text-sm text-gray-500">配置加载中…</section>;

  const toggleBg = (id: string) => {
    const cur = params.backgrounds;
    setParams({
      backgrounds: cur.includes(id) ? cur.filter((b) => b !== id) : [...cur, id],
    });
  };

  /** 自定义尺寸标识（契约：`mm:宽x高@DPI`） */
  const customSizeId = (v: { w: number; h: number; dpi: number }) => `mm:${v.w}x${v.h}@${v.dpi}`;

  const updateSizeMm = (patch: Partial<typeof sizeMm>) => {
    const next = { ...sizeMm, ...patch };
    setSizeMm(next);
    if (params.customSize !== null) setParams({ customSize: customSizeId(next) });
  };

  const missing = models.filter((m) => m.check_status === "missing").length;

  return (
    <section className="rounded-lg bg-white shadow-sm border border-gray-200 p-4 space-y-4">
      <h2 className="text-sm font-semibold text-gray-700">2. 参数设置</h2>

      {missing > 0 && (
        <div className="space-y-2 rounded-md bg-amber-50 border border-amber-200 px-3 py-2 text-xs text-amber-700">
          <p>有 {missing} 个模型文件缺失，处理会失败，可一键下载。</p>
          <button
            type="button"
            disabled={downloading}
            onClick={() => void downloadModels()}
            className="rounded-md bg-amber-600 px-3 py-1.5 font-medium text-white transition hover:bg-amber-700 disabled:opacity-50"
          >
            {downloading ? "下载中…（模型较大，请耐心等待）" : `一键下载缺失模型（${missing}）`}
          </button>
        </div>
      )}

      <div>
        <label className="mb-1 block text-xs text-gray-500">运行模式</label>
        <div className="grid grid-cols-3 gap-2">
          {config.modes.map((m) => (
            <button
              key={m.id}
              type="button"
              onClick={() => setParams({ mode: m.id })}
              className={`rounded-md border px-2 py-2 text-xs transition ${
                params.mode === m.id
                  ? "border-blue-500 bg-blue-50 text-blue-700"
                  : "border-gray-200 text-gray-600 hover:border-gray-300"
              }`}
            >
              {m.label}
            </button>
          ))}
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className="mb-1 block text-xs text-gray-500">尺寸</label>
          <select
            value={params.size}
            onChange={(e) => setParams({ size: e.target.value })}
            className="w-full rounded-md border border-gray-300 px-2 py-1.5 text-sm"
          >
            {config.sizes.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}（{s.width_px}×{s.height_px}）
              </option>
            ))}
          </select>
        </div>
        <div>
          <label className="mb-1 block text-xs text-gray-500">排版相纸（可选）</label>
          <select
            value={params.layout ?? ""}
            onChange={(e) => setParams({ layout: e.target.value || null })}
            className="w-full rounded-md border border-gray-300 px-2 py-1.5 text-sm"
          >
            <option value="">不排版</option>
            {config.layouts.map((l) => (
              <option key={l.id} value={l.id}>
                {l.name}
              </option>
            ))}
          </select>
        </div>
      </div>

      <div className="space-y-2">
        <label className="flex items-center gap-2 text-xs text-gray-600">
          <input
            type="checkbox"
            checked={params.customSize !== null}
            onChange={(e) =>
              setParams({ customSize: e.target.checked ? customSizeId(sizeMm) : null })
            }
          />
          使用自定义尺寸（毫米）
        </label>
        {params.customSize !== null && (
          <div className="grid grid-cols-3 gap-2">
            <div>
              <label className="mb-1 block text-[11px] text-gray-500">宽（mm）</label>
              <input
                type="number"
                min={1}
                max={1000}
                step={0.5}
                value={sizeMm.w}
                onChange={(e) => updateSizeMm({ w: Number(e.target.value) })}
                className="w-full rounded-md border border-gray-300 px-2 py-1.5 text-xs"
              />
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-gray-500">高（mm）</label>
              <input
                type="number"
                min={1}
                max={1000}
                step={0.5}
                value={sizeMm.h}
                onChange={(e) => updateSizeMm({ h: Number(e.target.value) })}
                className="w-full rounded-md border border-gray-300 px-2 py-1.5 text-xs"
              />
            </div>
            <div>
              <label className="mb-1 block text-[11px] text-gray-500">DPI</label>
              <input
                type="number"
                min={72}
                max={2400}
                step={1}
                value={sizeMm.dpi}
                onChange={(e) => updateSizeMm({ dpi: Number(e.target.value) })}
                className="w-full rounded-md border border-gray-300 px-2 py-1.5 text-xs"
              />
            </div>
          </div>
        )}
      </div>

      <div>
        <label className="mb-1 block text-xs text-gray-500">底色（可多选）</label>
        <div className="flex flex-wrap gap-2">
          {config.backgrounds.map((b) => {
            const active = params.backgrounds.includes(b.id);
            const [r, g, bl] = b.rgb;
            return (
              <button
                key={b.id}
                type="button"
                onClick={() => toggleBg(b.id)}
                className={`flex items-center gap-1.5 rounded-md border px-2 py-1.5 text-xs transition ${
                  active ? "border-blue-500 bg-blue-50 text-blue-700" : "border-gray-200 text-gray-600"
                }`}
              >
                <span
                  className="inline-block h-3.5 w-3.5 rounded-full border border-gray-300"
                  style={{ backgroundColor: `rgb(${r},${g},${bl})` }}
                />
                {b.name}
              </button>
            );
          })}
        </div>
        <label className="mt-2 flex items-center gap-2 text-xs text-gray-600">
          <input
            type="checkbox"
            checked={params.customBg !== null}
            onChange={(e) => setParams({ customBg: e.target.checked ? bgColor : null })}
          />
          自定义底色
          <input
            type="color"
            value={params.customBg ?? bgColor}
            disabled={params.customBg === null}
            onChange={(e) => {
              setBgColor(e.target.value);
              if (params.customBg !== null) setParams({ customBg: e.target.value });
            }}
            className="h-6 w-10 rounded border border-gray-300 disabled:opacity-50"
          />
          {params.customBg && <span className="text-[11px] text-gray-400">{params.customBg}</span>}
        </label>
      </div>

      <div className="flex items-center justify-between">
        <label className="text-xs text-gray-500">姿态纠偏</label>
        <div className="flex items-center gap-2 text-xs">
          <label className="flex items-center gap-1">
            <input
              type="radio"
              name="rotate"
              checked={params.rotate === null}
              onChange={() => setParams({ rotate: null })}
            />
            自动
          </label>
          <label className="flex items-center gap-1">
            <input
              type="radio"
              name="rotate"
              checked={params.rotate !== null}
              onChange={() => setParams({ rotate: 0 })}
            />
            手动
          </label>
          {params.rotate !== null && (
            <input
              type="number"
              min={-45}
              max={45}
              step={0.5}
              value={params.rotate}
              onChange={(e) => setParams({ rotate: Number(e.target.value) })}
              className="w-20 rounded-md border border-gray-300 px-2 py-1 text-right"
            />
          )}
        </div>
      </div>

      <label className="flex items-center gap-2 text-xs text-gray-600">
        <input
          type="checkbox"
          checked={params.effect}
          onChange={(e) => setParams({ effect: e.target.checked })}
        />
        输出通用效果图（保持原尺寸）
      </label>

      <label className="flex items-center gap-2 text-xs text-gray-600">
        <input
          type="checkbox"
          checked={params.transparent}
          onChange={(e) => setParams({ transparent: e.target.checked })}
        />
        输出透明底 PNG（带 alpha 通道）
      </label>

      <div className="grid grid-cols-2 gap-2">
        <div>
          <label className="mb-1 block text-xs text-gray-500">输出格式</label>
          <select
            value={params.outputFormat}
            onChange={(e) => setParams({ outputFormat: e.target.value })}
            className="w-full rounded-md border border-gray-300 px-2 py-1.5 text-sm"
          >
            <option value="jpg">JPG（有损）</option>
            <option value="webp">WebP（无损）</option>
          </select>
        </div>
        <div>
          <label className="mb-1 block text-xs text-gray-500">JPG 质量：{params.jpgQuality}</label>
          <input
            type="range"
            min={1}
            max={100}
            value={params.jpgQuality}
            disabled={params.outputFormat !== "jpg"}
            onChange={(e) => setParams({ jpgQuality: Number(e.target.value) })}
            className="w-full"
          />
        </div>
      </div>

      <label className="flex items-center gap-2 text-xs text-gray-600">
        <input
          type="checkbox"
          checked={params.pdf}
          disabled={!params.layout}
          onChange={(e) => setParams({ pdf: e.target.checked })}
        />
        排版结果额外输出 PDF（页面按相纸物理尺寸）
      </label>

      <div>
        <label className="mb-1 block text-xs text-gray-500">自定义背景图（本地路径，可选）</label>
        <input
          type="text"
          value={params.bgImage ?? ""}
          placeholder="如 D:\\图片\\背景.jpg（留空则不输出）"
          onChange={(e) => setParams({ bgImage: e.target.value.trim() || null })}
          className="w-full rounded-md border border-gray-300 px-2 py-1.5 text-xs"
        />
        <p className="mt-1 text-[11px] text-gray-400">
          按证件照尺寸等比裁切后合成，额外出 custombg 产物。
        </p>
      </div>

      <button
        type="button"
        disabled={submitting}
        onClick={() => void submit()}
        className="w-full rounded-md bg-blue-600 px-4 py-2.5 text-sm font-medium text-white transition hover:bg-blue-700 disabled:opacity-50"
      >
        {submitting ? "处理中…" : "开始处理"}
      </button>
    </section>
  );
}
