import { create } from "zustand";
import {
  AppConfig,
  EMPTY_TASK_FILTER,
  ModelItem,
  SubmitParams,
  TaskDetail,
  TaskFilter,
  TaskItem,
  clearTasks as requestClearTasks,
  deleteTask as requestDeleteTask,
  downloadModels as requestModelDownload,
  fetchConfig,
  fetchModels,
  fetchTaskDetail,
  fetchTasks,
  submitTask,
} from "./api";

export interface ParamsState {
  mode: string;
  size: string;
  backgrounds: string[];
  layout: string | null;
  effect: boolean;
  rotate: number | null; // null = 自动
  transparent: boolean; // 额外输出透明底 PNG
  bgImage: string | null; // 自定义背景图路径
  customBg: string | null; // 自定义 RGB 底色（#RRGGBB），null = 不启用
  customSize: string | null; // 自定义尺寸标识（mm:宽x高@DPI），null = 用内置尺寸
  outputFormat: string; // 图片输出格式：jpg | webp
  jpgQuality: number; // JPG 压缩质量 1..=100
  pdf: boolean; // 排版相纸额外输出 PDF
}

/** 批量项状态：上传中 → 处理中 → 成功 / 失败 */
export type BatchStatus = "uploading" | "processing" | "succeeded" | "failed";

/** 批量项（保留原文件，供失败重试） */
export interface BatchItem {
  name: string;
  file: File;
  id: string | null;
  status: BatchStatus;
  message?: string | null;
  elapsed_ms?: number | null;
}

interface AppState {
  config: AppConfig | null;
  models: { id: string; ready: boolean; check_status: string; message: string }[];
  tasks: TaskItem[];
  /** 历史列表筛选条件 */
  taskFilter: TaskFilter;
  selectedId: string | null;
  detail: TaskDetail | null;
  files: File[];
  params: ParamsState;
  /** 本次批量的逐项进度（空数组表示无批量任务） */
  batch: BatchItem[];
  /** 本次批量使用的提交参数（失败重试沿用同一套参数） */
  lastPayload: SubmitParams | null;
  submitting: boolean;
  /** 模型一键下载进行中 */
  downloading: boolean;
  error: string | null;

  init: () => Promise<void>;
  setFiles: (files: File[]) => void;
  setParams: (patch: Partial<ParamsState>) => void;
  setSelected: (id: string | null) => void;
  setTaskFilter: (patch: Partial<TaskFilter>) => void;
  refreshTasks: () => Promise<void>;
  /** 把历史任务的提交参数回填到参数面板 */
  reuseParams: (id: string) => Promise<void>;
  deleteTask: (id: string) => Promise<void>;
  clearTasks: () => Promise<void>;
  refreshDetail: () => Promise<void>;
  refreshBatch: () => Promise<void>;
  submit: () => Promise<void>;
  retryFailed: () => Promise<void>;
  downloadModels: () => Promise<void>;
}

/** 参数面板状态 → 提交参数（自定义底色/尺寸以字符串追加，服务端归一化为文件名安全 id） */
function toSubmitParams(params: ParamsState): SubmitParams {
  return {
    mode: params.mode,
    size: params.customSize ?? params.size,
    backgrounds: params.customBg ? [...params.backgrounds, params.customBg] : params.backgrounds,
    rotate: params.rotate,
    layout: params.layout,
    effect_image: params.effect,
    transparent: params.transparent,
    bg_image: params.bgImage,
    output_format: params.outputFormat,
    jpg_quality: params.jpgQuality,
    pdf: params.pdf,
  };
}

/** 归一化底色 id（`rgb-ff0000`）→ 参数面板用 `#RRGGBB`；内置 id 原样返回 */
function bgIdToHex(id: string): string {
  return id.startsWith("rgb-") ? `#${id.slice(4)}` : id;
}

/** 任务终态判定（queued/running 视为处理中） */
function toBatchStatus(status: string): BatchStatus {
  if (status === "succeeded") return "succeeded";
  if (status === "failed") return "failed";
  return "processing";
}

/** 模型列表状态映射（GET /models → 前端状态） */
function mapModels(items: ModelItem[]) {
  return items.map((m) => ({
    id: m.id,
    ready: m.ready,
    check_status: m.check_status,
    message: m.message,
  }));
}

export const useStore = create<AppState>((set, get) => ({
  config: null,
  models: [],
  tasks: [],
  taskFilter: EMPTY_TASK_FILTER,
  selectedId: null,
  detail: null,
  files: [],
  params: {
    mode: "balanced",
    size: "one_inch",
    backgrounds: ["white"],
    layout: null,
    effect: false,
    rotate: null,
    transparent: false,
    bgImage: null,
    customBg: null,
    customSize: null,
    outputFormat: "jpg",
    jpgQuality: 90,
    pdf: false,
  },
  batch: [],
  lastPayload: null,
  submitting: false,
  downloading: false,
  error: null,

  async init() {
    try {
      const [config, models, tasks] = await Promise.all([
        fetchConfig(),
        fetchModels(),
        fetchTasks(),
      ]);
      set({
        config,
        models: mapModels(models.items),
        tasks: tasks.items,
        params: {
          mode: config.default_mode,
          size: config.sizes[0]?.id ?? "one_inch",
          backgrounds: [config.backgrounds[0]?.id ?? "white"],
          layout: null,
          effect: false,
          rotate: null,
          transparent: false,
          bgImage: null,
          customBg: null,
          customSize: null,
          outputFormat: config.output?.format ?? "jpg",
          jpgQuality: config.output?.jpg_quality ?? 90,
          pdf: config.output?.pdf ?? false,
        },
      });
    } catch (e) {
      set({ error: (e as Error).message });
    }
  },

  setFiles(files) {
    set({ files });
  },

  setParams(patch) {
    set({ params: { ...get().params, ...patch } });
  },

  setSelected(id) {
    set({ selectedId: id, detail: null });
    if (id) {
      get().refreshDetail();
    }
  },

  async refreshTasks() {
    const list = await fetchTasks(50, 0, get().taskFilter);
    set({ tasks: list.items });
  },

  setTaskFilter(patch) {
    set({ taskFilter: { ...get().taskFilter, ...patch } });
    void get().refreshTasks();
  },

  /** 回填历史任务的提交参数：尺寸/底色的内置项与自定义项分流到对应控件 */
  async reuseParams(id) {
    try {
      const detail = await fetchTaskDetail(id);
      const p = detail.params;
      if (!p) return;
      const { config, params } = get();
      const size = p.size ?? "";
      const builtinSize = config?.sizes.some((s) => s.id === size) ?? false;
      const bgs = p.backgrounds ?? [];
      const builtinBgs = bgs.filter((b) => config?.backgrounds.some((x) => x.id === b));
      const customBg = bgs.find((b) => !config?.backgrounds.some((x) => x.id === b));
      set({
        params: {
          ...params,
          mode: p.mode ?? params.mode,
          size: builtinSize ? size : params.size,
          customSize: !builtinSize && size ? size : null,
          backgrounds: builtinBgs,
          customBg: customBg ? bgIdToHex(customBg) : null,
          layout: p.layout ?? null,
          effect: p.effect_image ?? false,
          rotate: p.rotate ?? null,
          transparent: p.transparent ?? false,
          bgImage: p.bg_image ?? null,
          outputFormat: p.output_format ?? params.outputFormat,
          jpgQuality: p.jpg_quality ?? params.jpgQuality,
          pdf: p.pdf ?? false,
        },
      });
    } catch (e) {
      set({ error: (e as Error).message });
    }
  },

  async deleteTask(id) {
    try {
      await requestDeleteTask(id);
      if (get().selectedId === id) set({ selectedId: null, detail: null });
      await get().refreshTasks();
    } catch (e) {
      set({ error: (e as Error).message });
    }
  },

  async clearTasks() {
    try {
      await requestClearTasks();
      set({ selectedId: null, detail: null });
      await get().refreshTasks();
    } catch (e) {
      set({ error: (e as Error).message });
    }
  },

  async refreshDetail() {
    const { selectedId } = get();
    if (!selectedId) return;
    const detail = await fetchTaskDetail(selectedId);
    set({ detail });
  },

  async submit() {
    const { files, params } = get();
    if (files.length === 0) {
      set({ error: "请先选择要处理的图片" });
      return;
    }
    const payload = toSubmitParams(params);
    set({
      submitting: true,
      error: null,
      files: [],
      selectedId: null,
      detail: null,
      batch: files.map((f) => ({ name: f.name, file: f, id: null, status: "uploading" })),
      lastPayload: payload,
    });
    try {
      await uploadBatch(payload);
      await get().refreshTasks();
      pollBatch();
    } catch (e) {
      set({ error: (e as Error).message });
    } finally {
      set({ submitting: false });
    }
  },

  async retryFailed() {
    const payload = get().lastPayload;
    if (!payload) return;
    const { batch } = get();
    if (!batch.some((i) => i.status === "failed")) return;
    set({ submitting: true, error: null, batch: resetFailed(batch) });
    try {
      await uploadBatch(payload);
      await get().refreshTasks();
      pollBatch();
    } catch (e) {
      set({ error: (e as Error).message });
    } finally {
      set({ submitting: false });
    }
  },

  /** 拉取批量中「处理中」项的详情，更新进度（同时刷新选中项的预览） */
  async refreshBatch() {
    const { batch } = get();
    if (!batch.some((i) => i.status === "processing")) return;
    const next = await Promise.all(
      batch.map(async (item) => {
        if (item.status !== "processing" || !item.id) return item;
        try {
          const detail = await fetchTaskDetail(item.id);
          if (useStore.getState().selectedId === item.id) {
            useStore.setState({ detail });
          }
          return {
            ...item,
            status: toBatchStatus(detail.status),
            message: detail.message,
            elapsed_ms: detail.elapsed_ms,
          };
        } catch {
          return item;
        }
      }),
    );
    set({ batch: next });
  },

  async downloadModels() {
    set({ downloading: true, error: null });
    try {
      const res = await requestModelDownload();
      // 下载完成后刷新模型状态（成功的模型在列表中转为就绪）
      const models = await fetchModels();
      set({ models: mapModels(models.items) });
      const failed = res.items.filter((i) => !i.ok);
      if (failed.length > 0) {
        set({
          error: `模型下载失败：${failed
            .map((f) => `${f.id}（${f.message}）`)
            .join("；")}`,
        });
      }
    } catch (e) {
      set({ error: (e as Error).message });
    } finally {
      set({ downloading: false });
    }
  },
}));

/** 逐个提交批量中「待上传」项（逐项记录成功/失败，单个失败不阻断其余） */
async function uploadBatch(payload: SubmitParams) {
  const next = [...useStore.getState().batch];
  for (let i = 0; i < next.length; i++) {
    if (next[i].status !== "uploading") continue;
    try {
      const id = await submitTask(next[i].file, payload);
      next[i] = { ...next[i], id, status: "processing", message: null };
      // 选中首个提交成功的任务作为预览对象
      if (useStore.getState().selectedId === null) {
        useStore.setState({ selectedId: id, detail: null });
      }
    } catch (e) {
      next[i] = { ...next[i], status: "failed", message: (e as Error).message };
    }
    useStore.setState({ batch: [...next] });
  }
}

/** 失败项重置为「待上传」（供一键重试） */
function resetFailed(batch: BatchItem[]): BatchItem[] {
  return batch.map((item): BatchItem =>
    item.status === "failed" ? { ...item, status: "uploading", message: null } : item,
  );
}

/** 轮询批量任务直至全部终态，随后刷新历史列表 */
function pollBatch() {
  const tick = async () => {
    await useStore.getState().refreshBatch();
    if (useStore.getState().batch.some((i) => i.status === "processing")) {
      window.setTimeout(() => void tick(), 500);
    } else {
      await useStore.getState().refreshTasks();
    }
  };
  void tick();
}
