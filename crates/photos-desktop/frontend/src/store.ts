import { create } from "zustand";
import {
  AppConfig,
  SubmitParams,
  TaskDetail,
  TaskItem,
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
}

interface AppState {
  config: AppConfig | null;
  models: { id: string; ready: boolean; message: string }[];
  tasks: TaskItem[];
  selectedId: string | null;
  detail: TaskDetail | null;
  files: File[];
  params: ParamsState;
  submitting: boolean;
  error: string | null;

  init: () => Promise<void>;
  setFiles: (files: File[]) => void;
  setParams: (patch: Partial<ParamsState>) => void;
  setSelected: (id: string | null) => void;
  refreshTasks: () => Promise<void>;
  refreshDetail: () => Promise<void>;
  submit: () => Promise<void>;
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
  },
  submitting: false,
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
        models: models.items.map((m) => ({ id: m.id, ready: m.ready, message: m.message })),
        tasks: tasks.items,
        params: {
          mode: config.default_mode,
          size: config.sizes[0]?.id ?? "one_inch",
          backgrounds: [config.backgrounds[0]?.id ?? "white"],
          layout: null,
          effect: false,
          rotate: null,
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
      const payload: SubmitParams = {
        mode: params.mode,
        size: params.size,
        backgrounds: params.backgrounds,
        rotate: params.rotate,
        layout: params.layout,
        effect_image: params.effect,
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
