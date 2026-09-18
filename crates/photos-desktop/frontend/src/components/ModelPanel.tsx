// 模型管理面板（插件化模型）：注册表（角色 / 版本切换）+ 市场清单（下载）+ 自定义注册表单。
// 契约：docs/05-API契约.md §3.14~§3.17；注册结果需重启服务生效（面板内显式提示）。

import { useEffect, useState } from "react";
import { useStore } from "../store";

/** 角色中文标签 */
const ROLE_LABELS: Record<string, string> = {
  auto: "未声明",
  face: "人脸检测",
  keypoint: "人体关键点",
  matting: "人像抠图",
  parsing: "人像解析",
};

/** 归一化下拉可选项（与 photos-core `Norm` 序列化一致） */
const NORM_OPTIONS = [
  { id: "unit", label: "除以255（内置约定）" },
  { id: "none", label: "不归一化（0-255）" },
  { id: "mean", label: "减均值" },
  { id: "mean_std", label: "减均值除标准差" },
];

/** 按后端 `Norm` 的 serde 形态构造归一化声明（unit/none 为字符串，其余为对象） */
function buildNorm(kind: string, mean: string, std: string): unknown {
  const parseTriple = (text: string) =>
    text
      .split(/[,，\s]+/)
      .filter(Boolean)
      .map(Number)
      .filter((n) => !Number.isNaN(n));
  if (kind === "none") return "none";
  if (kind === "mean") return { mean: parseTriple(mean) };
  if (kind === "mean_std") return { mean_std: { mean: parseTriple(mean), std: parseTriple(std) } };
  return "unit";
}

export default function ModelPanel() {
  const {
    models,
    market,
    modelBusy,
    modelNotice,
    refreshMarket,
    registerModel,
    activateModel,
    downloadVersion,
  } = useStore();
  // 每个模型选中的版本（默认取激活版本）
  const [picked, setPicked] = useState<Record<string, string>>({});
  const [form, setForm] = useState({
    id: "",
    path: "",
    dims: "1,3,512,512",
    role: "matting",
    sha256: "",
    layout: "auto",
    norm: "unit",
    mean: "",
    std: "",
    channel: "rgb",
  });

  useEffect(() => {
    void refreshMarket();
  }, [refreshMarket]);

  const submitRegister = () => {
    const dims = form.dims
      .split(/[,，\s]+/)
      .filter(Boolean)
      .map(Number)
      .filter((n) => !Number.isNaN(n));
    void registerModel({
      id: form.id.trim(),
      path: form.path.trim(),
      input_dims: dims,
      role: form.role,
      sha256: form.sha256.trim() || undefined,
      preprocess: {
        layout: form.layout,
        norm: buildNorm(form.norm, form.mean, form.std),
        channel: form.channel,
      },
    });
  };

  return (
    <section className="rounded-lg bg-white p-5 shadow-sm">
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-gray-700">模型管理</h2>
        <button
          type="button"
          onClick={() => void refreshMarket()}
          className="rounded-md border border-gray-200 px-2 py-1 text-xs text-gray-600 transition hover:border-gray-300"
        >
          刷新
        </button>
      </div>

      {modelNotice && (
        <p className="mb-3 rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-700">
          {modelNotice}
        </p>
      )}

      {/* 注册表：角色 + 已下载版本切换 / 回滚 */}
      <div className="space-y-2">
        <h3 className="text-xs font-medium text-gray-500">已注册模型（{models.length}）</h3>
        <div className="max-h-72 space-y-2 overflow-y-auto pr-1">
          {models.map((m) => {
            const selected = picked[m.id] ?? m.active_version ?? m.versions[0] ?? "";
            return (
              <div key={m.id} className="rounded-md border border-gray-200 px-3 py-2">
                <div className="flex flex-wrap items-center gap-2 text-xs">
                  <span className="font-mono text-gray-800">{m.id}</span>
                  <span className="rounded bg-gray-100 px-1.5 py-0.5 text-gray-600">
                    {ROLE_LABELS[m.role] ?? m.role}
                  </span>
                  {!m.builtin && (
                    <span className="rounded bg-blue-50 px-1.5 py-0.5 text-blue-600">自定义</span>
                  )}
                  <span
                    className={`rounded px-1.5 py-0.5 ${
                      m.ready ? "bg-green-50 text-green-700" : "bg-red-50 text-red-600"
                    }`}
                  >
                    {m.check_status}
                  </span>
                  {m.active_version && (
                    <span className="text-gray-400">当前版本 {m.active_version}</span>
                  )}
                </div>
                {m.versions.length > 0 ? (
                  <div className="mt-2 flex items-center gap-2">
                    <select
                      value={selected}
                      onChange={(e) => setPicked({ ...picked, [m.id]: e.target.value })}
                      className="rounded-md border border-gray-300 px-2 py-1 text-xs"
                    >
                      {m.versions.map((v) => (
                        <option key={v} value={v}>
                          {v}
                        </option>
                      ))}
                    </select>
                    <button
                      type="button"
                      disabled={modelBusy || !selected || selected === m.active_version}
                      onClick={() => void activateModel(m.id, selected)}
                      className="rounded-md bg-gray-700 px-2 py-1 text-xs text-white transition hover:bg-gray-800 disabled:opacity-50"
                    >
                      切换 / 回滚
                    </button>
                  </div>
                ) : (
                  <p className="mt-1 text-[11px] text-gray-400">
                    本地无版本目录（使用 {m.path}）
                  </p>
                )}
              </div>
            );
          })}
        </div>
      </div>

      {/* 市场清单：内置条目默认禁用下载，补齐直链后由用户覆盖文件启用 */}
      <div className="mt-4 space-y-2">
        <h3 className="text-xs font-medium text-gray-500">模型市场（{market.length}）</h3>
        <div className="max-h-60 space-y-2 overflow-y-auto pr-1">
          {market.map((it) => (
            <div
              key={`${it.id}-${it.version}`}
              className="flex flex-wrap items-center gap-2 rounded-md border border-gray-200 px-3 py-2 text-xs"
            >
              <span className="font-mono text-gray-800">{it.id}</span>
              <span className="text-gray-500">{it.version}</span>
              <span className="rounded bg-gray-100 px-1.5 py-0.5 text-gray-600">
                {ROLE_LABELS[it.role] ?? it.role}
              </span>
              {it.downloaded && (
                <span className="rounded bg-green-50 px-1.5 py-0.5 text-green-700">已下载</span>
              )}
              {!it.enabled && (
                <span className="rounded bg-gray-100 px-1.5 py-0.5 text-gray-500">未启用</span>
              )}
              <button
                type="button"
                disabled={modelBusy || !it.downloadable || it.downloaded}
                onClick={() => void downloadVersion(it.id, it.version)}
                className="ml-auto rounded-md border border-gray-300 px-2 py-1 text-xs text-gray-700 transition hover:border-gray-400 disabled:opacity-50"
              >
                下载该版本
              </button>
            </div>
          ))}
        </div>
        <p className="text-[11px] text-gray-400">
          内置清单的直链与 sha256 为占位（禁用下载）；在 data/model_market.toml 按 id 覆盖后即可启用下载。
        </p>
      </div>

      {/* 注册表单：任意 ONNX + 声明式预处理 */}
      <div className="mt-4 space-y-2">
        <h3 className="text-xs font-medium text-gray-500">注册自定义模型</h3>
        <div className="grid grid-cols-2 gap-2">
          <input
            value={form.id}
            onChange={(e) => setForm({ ...form, id: e.target.value })}
            placeholder="模型 id（字母数字_-）"
            className="rounded-md border border-gray-300 px-2 py-1.5 text-xs"
          />
          <input
            value={form.path}
            onChange={(e) => setForm({ ...form, path: e.target.value })}
            placeholder="onnx 路径（如 models/my.onnx）"
            className="rounded-md border border-gray-300 px-2 py-1.5 text-xs"
          />
          <input
            value={form.dims}
            onChange={(e) => setForm({ ...form, dims: e.target.value })}
            placeholder="输入维度（如 1,3,512,512）"
            className="rounded-md border border-gray-300 px-2 py-1.5 text-xs"
          />
          <select
            value={form.role}
            onChange={(e) => setForm({ ...form, role: e.target.value })}
            className="rounded-md border border-gray-300 px-2 py-1.5 text-xs"
          >
            {Object.entries(ROLE_LABELS).map(([id, label]) => (
              <option key={id} value={id}>
                {label}
              </option>
            ))}
          </select>
          <input
            value={form.sha256}
            onChange={(e) => setForm({ ...form, sha256: e.target.value })}
            placeholder="sha256（留空则按已有文件自动计算）"
            className="col-span-2 rounded-md border border-gray-300 px-2 py-1.5 text-xs"
          />
          <select
            value={form.layout}
            onChange={(e) => setForm({ ...form, layout: e.target.value })}
            className="rounded-md border border-gray-300 px-2 py-1.5 text-xs"
          >
            <option value="auto">布局：自动推断</option>
            <option value="nchw">布局：NCHW</option>
            <option value="nhwc">布局：NHWC</option>
          </select>
          <select
            value={form.norm}
            onChange={(e) => setForm({ ...form, norm: e.target.value })}
            className="rounded-md border border-gray-300 px-2 py-1.5 text-xs"
          >
            {NORM_OPTIONS.map((n) => (
              <option key={n.id} value={n.id}>
                归一化：{n.label}
              </option>
            ))}
          </select>
          <select
            value={form.channel}
            onChange={(e) => setForm({ ...form, channel: e.target.value })}
            className="rounded-md border border-gray-300 px-2 py-1.5 text-xs"
          >
            <option value="rgb">通道序：RGB</option>
            <option value="bgr">通道序：BGR</option>
          </select>
          {(form.norm === "mean" || form.norm === "mean_std") && (
            <input
              value={form.mean}
              onChange={(e) => setForm({ ...form, mean: e.target.value })}
              placeholder="均值（如 0.485,0.456,0.406）"
              className="rounded-md border border-gray-300 px-2 py-1.5 text-xs"
            />
          )}
          {form.norm === "mean_std" && (
            <input
              value={form.std}
              onChange={(e) => setForm({ ...form, std: e.target.value })}
              placeholder="标准差（如 0.229,0.224,0.225）"
              className="rounded-md border border-gray-300 px-2 py-1.5 text-xs"
            />
          )}
        </div>
        <button
          type="button"
          disabled={modelBusy}
          onClick={submitRegister}
          className="w-full rounded-md bg-blue-600 px-3 py-2 text-xs font-medium text-white transition hover:bg-blue-700 disabled:opacity-50"
        >
          {modelBusy ? "提交中…" : "注册模型（写入 data/models.custom.toml）"}
        </button>
        <p className="text-[11px] text-gray-400">
          注册通过校验后落盘，需重启服务生效；声明非法（角色 / 布局 / 归一化）会被拒绝且不落盘。
        </p>
      </div>
    </section>
  );
}