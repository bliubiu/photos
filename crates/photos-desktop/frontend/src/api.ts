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

export interface TaskDetail {
  id: string;
  status: "queued" | "running" | "succeeded" | "failed";
  message: string | null;
  warnings: string[];
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

export async function fetchTasks(limit = 50, offset = 0): Promise<TaskList> {
  return request<TaskList>(`/tasks?limit=${limit}&offset=${offset}`);
}

export async function fetchTaskDetail(id: string): Promise<TaskDetail> {
  return request<TaskDetail>(`/tasks/${id}`);
}

/** 批量提交任务：逐个上传（每个文件一个任务），返回任务 id 列表 */
export async function submitTasks(files: File[], params: SubmitParams): Promise<string[]> {
  const ids: string[] = [];
  for (const file of files) {
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
      }),
    );
    const created = await request<{ id: string; status: string }>("/tasks", {
      method: "POST",
      body: form,
    });
    ids.push(created.id);
  }
  return ids;
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
