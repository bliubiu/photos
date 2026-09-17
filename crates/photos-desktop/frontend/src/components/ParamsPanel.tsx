import { useStore } from "../store";

export default function ParamsPanel() {
  const { config, models, params, setParams, submit, submitting } = useStore();
  if (!config) return <section className="text-sm text-gray-500">配置加载中…</section>;

  const toggleBg = (id: string) => {
    const cur = params.backgrounds;
    setParams({
      backgrounds: cur.includes(id) ? cur.filter((b) => b !== id) : [...cur, id],
    });
  };

  const missing = models.filter((m) => !m.ready).length;

  return (
    <section className="rounded-lg bg-white shadow-sm border border-gray-200 p-4 space-y-4">
      <h2 className="text-sm font-semibold text-gray-700">2. 参数设置</h2>

      {missing > 0 && (
        <div className="rounded-md bg-amber-50 border border-amber-200 px-3 py-2 text-xs text-amber-700">
          有 {missing} 个模型未就绪，处理可能失败。可用 CLI 执行
          <code className="mx-1 bg-amber-100 px-1 rounded">photos models download all</code>
          一键下载。
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
