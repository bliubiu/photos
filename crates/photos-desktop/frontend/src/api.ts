// 与后端同源的 API 客户端（契约 docs/05-API契约.md）。

export interface ModeOption {
  id: string;
  label: string;
}
export interface SizeOption {
  id: string;
  name: string;
  width_px: number;
  height_px: number;
}
export interface BackgroundOption {
  id: string;
  name: string;
  rgb: number[];
}
export interface LayoutOption {
  id: string;
  name: string;
}
export interface AppConfig {
  default_mode: string;
  modes: ModeOption[];
  sizes: SizeOption[];
  backgrounds: BackgroundOption[];
  layouts: LayoutOption[];
  output: { format: string; jpg_quality: number; pdf: boolean };
}

export interface ModelItem {
  id: string;
  path: string;
  ready: boolean;
  check_status: string;
  message: string;
  /** 模型角色：auto | face | keypoint | matting | parsing */
  role: string;
  /** 当前生效路径所属版本（旧布局 / 直连路径为 null） */
  version: string | null;
  /** 已下载到 models/<id>/<版本>/ 的版本列表（升序） */
  versions: string[];
  active_version: string | null;
  /** 是否为内置注册表条目（false = 用户注册） */
  builtin: boolean;
}

/** 模型市场条目（内置清单 + 用户覆盖） */
export interface MarketItem {
  id: string;
  version: string;
  role: string;
  url: string;
  sha256: string;
  size: number;
  license: string;
  source: string;
  /** 清单条目是否启用（内置占位条目为 false，需用户覆盖后启用） */
  enabled: boolean;
  /** 是否具备下载条件（启用且直链与 sha256 齐备） */
  downloadable: boolean;
  /** 该 id 是否已在注册表中 */
  registered: boolean;
  /** 该版本是否已下载到本地 */
  downloaded: boolean;
  active_version: string | null;
}

export interface ModelVersionInfo {
  id: string;
  versions: string[];
  active_version: string | null;
}

/** 注册自定义模型的声明式元数据（预处理缺省沿用内置约定） */
export interface RegisterModelPayload {
  id: string;
  path: string;
  input_dims: number[];
  role?: string;
  /** 留空时若文件已存在则由服务端自动计算 */
  sha256?: string;
  preprocess?: { layout?: string; norm?: unknown; channel?: string };
}

export interface Artifact {
  kind: "id_photo" | "layout" | "effect";
  background?: string | null;
  layout?: string | null;
  filename: string;
}

/** 提交参数快照（GET /tasks/{id} 的 params 字段，供历史记录「复用参数」） */
export interface TaskParamsSnapshot {
  mode?: string;
  size?: string;
  backgrounds?: string[];
  layout?: string | null;
  effect_image?: boolean;
  rotate?: number | null;
  transparent?: boolean;
  bg_image?: string | null;
  output_format?: string | null;
  jpg_quality?: number | null;
  pdf?: boolean | null;
}

/** 单阶段耗时（任务详情 metrics 字段） */
export interface StageMetric {
  stage: string;
  ms: number;
}

export interface TaskDetail {
  id: string;
  status: "queued" | "running" | "succeeded" | "failed";
  message: string | null;
  warnings: string[];
  mode: string;
  size: string;
  backgrounds: string[];
  rotate: number | null;
  params: TaskParamsSnapshot | null;
  elapsed_ms: number | null;
  /** 分阶段耗时指标（未采集到时为空数组） */
  metrics: StageMetric[];
  created_at: string;
  artifacts: Artifact[];
}

export interface TaskItem {
  id: string;
  input_path: string;
  mode: string;
  size: string;
  backgrounds: string[];
  status: string;
  message: string | null;
  created_at: string;
  elapsed_ms: number | null;
  outputs: string[];
}

export interface TaskList {
  total: number;
  items: TaskItem[];
}

/** 美颜参数（enabled 开关；强度缺省取全局配置 `[beauty]` 默认值，0..=1） */
export interface BeautyParams {
  enabled: boolean;
  /** 磨皮强度 */
  skin_smooth: number;
  /** 提亮强度 */
  brighten: number;
  /** 美白强度 */
  whiten: number;
}

export interface SubmitParams {
  mode: string;
  size: string;
  backgrounds: string[];
  rotate: number | null;
  layout: string | null;
  effect_image: boolean;
  /** 美颜（磨皮 / 提亮 / 美白，仅 enabled 为 true 时生效） */
  beauty: BeautyParams;
  /** 额外输出透明底 PNG */
  transparent: boolean;
  /** 自定义背景图（服务端本地路径） */
  bg_image: string | null;
  /** 图片输出格式：jpg | webp */
  output_format: string;
  /** JPG 压缩质量 1..=100（webp 为无损，不受此项影响） */
  jpg_quality: number;
  /** 排版相纸额外输出 PDF */
  pdf: boolean;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, init);
  const text = await res.text();
  let body: unknown = null;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = text;
    }
  }
  if (!res.ok) {
    const message =
      body && typeof body === "object" && "message" in body
        ? String((body as { message: string }).message)
        : `请求失败（HTTP ${res.status}）`;
    throw new Error(message);
  }
  return body as T;
}

export async function fetchConfig(): Promise<AppConfig> {
  return request<AppConfig>("/config");
}

export async function fetchModels(): Promise<{ items: ModelItem[] }> {
  return request<{ items: ModelItem[] }>("/models");
}

export interface DownloadResult {
  id: string;
  ok: boolean;
  message: string;
}

/** 一键下载模型：不传 ids 时下载全部「缺失」模型（服务端逐个下载，单个失败不阻断其余）；
 *  传入 version 时按市场清单条目下载到 `models/<id>/<版本>/`（需同时指定 ids） */
export async function downloadModels(
  ids?: string[],
  version?: string,
): Promise<{ items: DownloadResult[] }> {
  const payload: { ids?: string[]; version?: string } = {};
  if (ids && ids.length > 0) payload.ids = ids;
  if (version) payload.version = version;
  return request<{ items: DownloadResult[] }>("/models/download", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
}

/** 模型市场清单（内置 + 用户覆盖，含已下载与激活状态） */
export async function fetchMarket(): Promise<{ items: MarketItem[] }> {
  return request<{ items: MarketItem[] }>("/models/market");
}

/** 查询指定模型已下载版本与当前激活版本 */
export async function fetchVersions(id: string): Promise<ModelVersionInfo> {
  return request<ModelVersionInfo>(`/models/versions?id=${encodeURIComponent(id)}`);
}

/** 切换 / 回滚到指定已下载版本（服务端校验文件存在与 sha256） */
export async function activateModel(
  id: string,
  version: string,
): Promise<{ id: string; active_version: string; message: string }> {
  return request<{ id: string; active_version: string; message: string }>("/models/activate", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ id, version }),
  });
}

/** 注册自定义模型（写入 data/models.custom.toml，重启服务后生效） */
export async function registerModel(
  payload: RegisterModelPayload,
): Promise<{ id: string; registered: boolean; replaced: boolean; restart_required: boolean; message: string }> {
  return request<{
    id: string;
    registered: boolean;
    replaced: boolean;
    restart_required: boolean;
    message: string;
  }>("/models/register", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
}

/** 历史筛选条件（空串表示该条件不过滤） */
export interface TaskFilter {
  status: string;
  mode: string;
  size: string;
  background: string;
  /** 起始创建日期（YYYY-MM-DD） */
  since: string;
}

/** 默认筛选（全部不过滤） */
export const EMPTY_TASK_FILTER: TaskFilter = {
  status: "",
  mode: "",
  size: "",
  background: "",
  since: "",
};

export async function fetchTasks(
  limit = 50,
  offset = 0,
  filter?: TaskFilter,
): Promise<TaskList> {
  const params = new URLSearchParams({ limit: String(limit), offset: String(offset) });
  if (filter) {
    for (const [key, value] of Object.entries(filter)) {
      if (value) params.set(key, value);
    }
  }
  return request<TaskList>(`/tasks?${params.toString()}`);
}

export async function fetchTaskDetail(id: string): Promise<TaskDetail> {
  return request<TaskDetail>(`/tasks/${id}`);
}

/** 删除单个历史任务（连带删除磁盘产物与上传原图） */
export async function deleteTask(id: string): Promise<{ id: string; deleted_outputs: number }> {
  return request<{ id: string; deleted_outputs: number }>(`/tasks/${id}`, { method: "DELETE" });
}

/** 清空全部历史任务（连带删除磁盘产物与上传原图） */
export async function clearTasks(): Promise<{ deleted: number }> {
  return request<{ deleted: number }>("/tasks", { method: "DELETE" });
}

/** 提交单个任务：上传一个文件，返回任务 id（批量逐个调用，便于逐项展示进度与失败重试） */
export async function submitTask(file: File, params: SubmitParams): Promise<string> {
  const form = new FormData();
  form.append("file", file);
  form.append(
    "params",
    JSON.stringify({
      mode: params.mode,
      size: params.size,
      backgrounds: params.backgrounds,
      rotate: params.rotate,
      layout: params.layout,
      effect_image: params.effect_image,
      beauty: params.beauty,
      transparent: params.transparent,
      bg_image: params.bg_image,
      output_format: params.output_format,
      jpg_quality: params.jpg_quality,
      pdf: params.pdf,
    }),
  );
  const created = await request<{ id: string; status: string }>("/tasks", {
    method: "POST",
    body: form,
  });
  return created.id;
}

/** 可观测性：任务统计 + 平均耗时 + 各阶段平均耗时 + 错误总数（GET /metrics） */
export interface MetricsSummary {
  tasks: { total: number; queued: number; running: number; succeeded: number; failed: number };
  elapsed_ms: { avg: number; samples: number };
  stages: { stage: string; avg_ms: number; samples: number }[];
  errors: { total: number };
}

export async function fetchMetrics(): Promise<MetricsSummary> {
  return request<MetricsSummary>("/metrics");
}

/** 错误上报记录（GET /errors，倒序） */
export interface ErrorItem {
  id: number;
  created_at: string;
  code: string;
  stage: string;
  message: string;
  task_id: string | null;
}

export async function fetchErrors(limit = 20): Promise<{ total: number; items: ErrorItem[] }> {
  return request<{ total: number; items: ErrorItem[] }>(`/errors?limit=${limit}`);
}

/** 产物下载地址（同源，直接可用于 <a> 或 <img>） */
export function outputUrl(id: string, kind: string, background?: string, layout?: string): string {
  const params = new URLSearchParams({ artifact: kind });
  if (background) params.set("background", background);
  if (layout) params.set("layout", layout);
  return `/tasks/${id}/output?${params.toString()}`;
}

export function bundleUrl(id: string): string {
  return `/tasks/${id}/output?artifact=bundle`;
}

/** 上传原图地址（供历史记录「原图/结果」对比） */
export function inputUrl(id: string): string {
  return `/tasks/${id}/input`;
}
