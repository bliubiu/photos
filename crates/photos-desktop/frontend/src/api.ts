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

export interface SubmitParams {
  mode: string;
  size: string;
  backgrounds: string[];
  rotate: number | null;
  layout: string | null;
  effect_image: boolean;
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

/** 一键下载模型：不传 ids 时下载全部「缺失」模型（服务端逐个下载，单个失败不阻断其余） */
export async function downloadModels(ids?: string[]): Promise<{ items: DownloadResult[] }> {
  return request<{ items: DownloadResult[] }>("/models/download", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(ids && ids.length > 0 ? { ids } : {}),
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
