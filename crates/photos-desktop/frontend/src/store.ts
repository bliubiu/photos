import { create } from "zustand";
import {
  AppConfig,
  ModelItem,
  SubmitParams,
  TaskDetail,
  TaskItem,
  downloadModels as requestModelDownload,
  fetchConfig,
  fetchModels,
  fetchTaskDetail,
  fetchTasks,
  submitTasks,
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
}

interface AppState {
  config: AppConfig | null;
  models: { id: string; ready: boolean; check_status: string; message: string }[];
  tasks: TaskItem[];
  selectedId: string | null;
  detail: TaskDetail | null;
  files: File[];
  params: ParamsState;
  submitting: boolean;
  /** 模型一键下载进行中 */
  downloading: boolean;
  error: string | null;

  init: () => Promise<void>;
  setFiles: (files: File[]) => void;
  setParams: (patch: Partial<ParamsState>) => void;
  setSelected: (id: string | null) => void;
  refreshTasks: () => Promise<void>;
  refreshDetail: () => Promise<void>;
  submit: () => Promise<void>;
  downloadModels: () => Promise<void>;
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
  },
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
    const list = await fetchTasks();
    set({ tasks: list.items });
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
    set({ submitting: true, error: null });
    try {
      // 自定义底色/尺寸以字符串形式追加，服务端归一化为文件名安全 id
      const backgrounds = params.customBg
        ? [...params.backgrounds, params.customBg]
        : params.backgrounds;
      const payload: SubmitParams = {
        mode: params.mode,
        size: params.customSize ?? params.size,
        backgrounds,
        rotate: params.rotate,
        layout: params.layout,
        effect_image: params.effect,
        transparent: params.transparent,
        bg_image: params.bgImage,
      };
      const ids = await submitTasks(files, payload);
      // 选中第一个新任务并开启轮询
      set({ selectedId: ids[0] ?? null, detail: null, files: [] });
      await get().refreshTasks();
      pollUntilSettled(ids);
    } catch (e) {
      set({ error: (e as Error).message });
    } finally {
      set({ submitting: false });
    }
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

/** 轮询任务详情直至终态（succeeded/failed），随后刷新列表 */
function pollUntilSettled(ids: string[]) {
  let timer: number | undefined;
  const tick = async () => {
    const remaining: string[] = [];
    for (const id of ids) {
      try {
        const detail = await fetchTaskDetail(id);
        if (detail.status === "succeeded" || detail.status === "failed") {
          if (useStore.getState().selectedId === id) {
            useStore.setState({ detail });
          }
        } else {
          remaining.push(id);
        }
      } catch {
        remaining.push(id);
      }
    }
    if (remaining.length > 0 && useStore.getState().selectedId) {
      timer = window.setTimeout(tick, 500);
    } else {
      await useStore.getState().refreshTasks();
    }
  };
  void tick();
  return () => {
    if (timer) window.clearTimeout(timer);
  };
}
