import { create } from "zustand";
import {
  AppConfig,
  EMPTY_TASK_FILTER,
  MarketItem,
  ModelItem,
  RegisterModelPayload,
  SubmitParams,
  TaskDetail,
  TaskFilter,
  TaskItem,
  activateModel as requestActivateModel,
  clearTasks as requestClearTasks,
  deleteTask as requestDeleteTask,
  downloadModels as requestModelDownload,
  fetchConfig,
  fetchMarket,
  fetchModels,
  fetchTaskDetail,
  fetchTasks,
  registerModel as requestRegisterModel,
  submitTask,
} from "./api";

export interface ParamsState {
  mode: string;
  size: string;
  backgrounds: string[];
  layout: string | null;
  effect: boolean;
  /** AI 美颜开关（磨皮 / 提亮 / 美白） */
  beautyEnabled: boolean;
  /** 磨皮强度 0..=1 */
  beautySkinSmooth: number;
  /** 提亮强度 0..=1 */
  beautyBrighten: number;
  /** 美白强度 0..=1 */
  beautyWhiten: number;
  rotate: number | null; // null = 自动
  transparent: boolean; // 额外输出透明底 PNG
  bgImage: string | null; // 自定义背景图路径
  customBg: string | null; // 自定义 RGB 底色（#RRGGBB），null = 不启用
  customSize: string | null; // 自定义尺寸标识（mm:宽x高@DPI），null = 用内置尺寸
  outputFormat: string; // 图片输出格式：jpg | webp
  jpgQuality: number; // JPG 压缩质量 1..=100
  pdf: boolean; // 排版相纸额外输出 PDF
}

/** 参数预设（「我的常用参数」）：名称 + 参数快照，持久化到 localStorage */
export interface Preset {
  id: string;
  name: string;
  createdAt: string;
  params: ParamsState;
}

const PRESET_KEY = "photos.presets";

/** 美颜强度默认值（与 photos-core `[beauty]` 默认值一致，前端调整后随请求显式下发） */
export const BEAUTY_DEFAULTS = { skinSmooth: 0.3, brighten: 0.2, whiten: 0.1 };

/** 读取本地预设（解析失败或隐私模式不可用时返回空列表） */
function loadPresets(): Preset[] {
  try {
    const raw = localStorage.getItem(PRESET_KEY);
    if (!raw) return [];
    const list = JSON.parse(raw) as Preset[];
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

function persistPresets(list: Preset[]) {
  try {
    localStorage.setItem(PRESET_KEY, JSON.stringify(list));
  } catch {
    // 写入失败（如隐私模式）不影响本次会话使用
  }
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
  models: ModelItem[];
  /** 模型市场清单（内置 + 用户覆盖） */
  market: MarketItem[];
  /** 模型管理操作（注册 / 切换版本 / 版本下载）进行中 */
  modelBusy: boolean;
  /** 注册成功后的重启提示（重启服务后自定义模型才生效） */
  modelNotice: string | null;
  tasks: TaskItem[];
  /** 历史列表筛选条件 */
  taskFilter: TaskFilter;
  selectedId: string | null;
  detail: TaskDetail | null;
  files: File[];
  params: ParamsState;
  /** 「我的常用参数」预设（localStorage 持久化） */
  presets: Preset[];
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
  /** 以当前参数保存预设（同名覆盖） */
  savePreset: (name: string) => void;
  /** 套用预设到参数面板 */
  applyPreset: (id: string) => void;
  deletePreset: (id: string) => void;
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
  /** 刷新模型市场清单 */
  refreshMarket: () => Promise<void>;
  /** 注册自定义模型（重启后生效） */
  registerModel: (payload: RegisterModelPayload) => Promise<void>;
  /** 切换 / 回滚模型版本 */
  activateModel: (id: string, version: string) => Promise<void>;
  /** 按市场清单下载指定模型版本（成功后自动激活该版本） */
  downloadVersion: (id: string, version: string) => Promise<void>;
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
    beauty: {
      enabled: params.beautyEnabled,
      skin_smooth: params.beautySkinSmooth,
      brighten: params.beautyBrighten,
      whiten: params.beautyWhiten,
    },
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

export const useStore = create<AppState>((set, get) => ({
  config: null,
  models: [],
  market: [],
  modelBusy: false,
  modelNotice: null,
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
    beautyEnabled: false,
    beautySkinSmooth: BEAUTY_DEFAULTS.skinSmooth,
    beautyBrighten: BEAUTY_DEFAULTS.brighten,
    beautyWhiten: BEAUTY_DEFAULTS.whiten,
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
  presets: loadPresets(),

  async init() {
    try {
      const [config, models, tasks] = await Promise.all([
        fetchConfig(),
        fetchModels(),
        fetchTasks(),
      ]);
      set({
        config,
        models: models.items,
        tasks: tasks.items,
        params: {
          mode: config.default_mode,
          size: config.sizes[0]?.id ?? "one_inch",
          backgrounds: [config.backgrounds[0]?.id ?? "white"],
          layout: null,
          effect: false,
          beautyEnabled: false,
          beautySkinSmooth: BEAUTY_DEFAULTS.skinSmooth,
          beautyBrighten: BEAUTY_DEFAULTS.brighten,
          beautyWhiten: BEAUTY_DEFAULTS.whiten,
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

  savePreset(name) {
    const trimmed = name.trim();
    if (!trimmed) {
      set({ error: "请输入预设名称" });
      return;
    }
    // 同名视为覆盖，避免预设列表堆积重复项
    const preset: Preset = {
      id: `preset_${Date.now().toString(36)}`,
      name: trimmed,
      createdAt: new Date().toISOString(),
      params: { ...get().params },
    };
    const next = [...get().presets.filter((p) => p.name !== trimmed), preset];
    persistPresets(next);
    set({ presets: next, error: null });
  },

  applyPreset(id) {
    const preset = get().presets.find((p) => p.id === id);
    if (preset) set({ params: { ...preset.params } });
  },

  deletePreset(id) {
    const next = get().presets.filter((p) => p.id !== id);
    persistPresets(next);
    set({ presets: next });
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
      set({ models: models.items });
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

  async refreshMarket() {
    try {
      const [market, models] = await Promise.all([fetchMarket(), fetchModels()]);
      set({ market: market.items, models: models.items });
    } catch (e) {
      set({ error: (e as Error).message });
    }
  },

  async registerModel(payload) {
    set({ modelBusy: true, error: null, modelNotice: null });
    try {
      const res = await requestRegisterModel(payload);
      set({
        modelNotice: res.restart_required
          ? `${res.message}（重启服务后生效）`
          : res.message,
      });
      await get().refreshMarket();
    } catch (e) {
      set({ error: (e as Error).message });
    } finally {
      set({ modelBusy: false });
    }
  },

  async activateModel(id, version) {
    set({ modelBusy: true, error: null, modelNotice: null });
    try {
      const res = await requestActivateModel(id, version);
      set({ modelNotice: `模型“${res.id}”已切换到版本 ${res.active_version}` });
      await get().refreshMarket();
    } catch (e) {
      set({ error: (e as Error).message });
    } finally {
      set({ modelBusy: false });
    }
  },

  async downloadVersion(id, version) {
    set({ modelBusy: true, error: null, modelNotice: null });
    try {
      const res = await requestModelDownload([id], version);
      const failed = res.items.filter((i) => !i.ok);
      if (failed.length > 0) {
        set({
          error: `模型“${id}”版本 ${version} 下载失败：${failed
            .map((f) => f.message)
            .join("；")}`,
        });
      } else {
        set({ modelNotice: `模型“${id}”版本 ${version} 下载完成并已激活` });
      }
      await get().refreshMarket();
    } catch (e) {
      set({ error: (e as Error).message });
    } finally {
      set({ modelBusy: false });
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
