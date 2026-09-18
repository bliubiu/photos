import { useEffect } from "react";
import { useStore } from "./store";
import UploadPanel from "./components/UploadPanel";
import ParamsPanel from "./components/ParamsPanel";
import BatchPanel from "./components/BatchPanel";
import PreviewPanel from "./components/PreviewPanel";
import LivePreviewPanel from "./components/LivePreviewPanel";
import HistoryList from "./components/HistoryList";
import ObservabilityPanel from "./components/ObservabilityPanel";
import ModelPanel from "./components/ModelPanel";
import WorkflowPanel from "./components/WorkflowPanel";

export default function App() {
  const { init, error } = useStore();

  useEffect(() => {
    void init();
  }, [init]);

  return (
    <div className="min-h-screen bg-gray-100 text-gray-900">
      <header className="bg-white shadow-sm px-6 py-4">
        <h1 className="text-xl font-bold text-gray-900">智能证件照处理工具</h1>
        <p className="text-sm text-gray-500">
          纯本地离线 · 自动姿态纠偏 · AI 抠图 · 多底色 / 排版 · 批量处理
        </p>
      </header>

      {error && (
        <div className="mx-6 mt-4 max-w-7xl rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700">
          {error}
          <button className="ml-3 underline" onClick={() => useStore.setState({ error: null })}>
            关闭
          </button>
        </div>
      )}

      <main className="mx-auto max-w-7xl grid grid-cols-1 lg:grid-cols-3 gap-6 p-6">
        <section className="lg:col-span-1 space-y-6">
          <UploadPanel />
          <ParamsPanel />
          <BatchPanel />
        </section>
        <section className="lg:col-span-2 space-y-6">
          <PreviewPanel />
          <LivePreviewPanel />
          <WorkflowPanel />
          <ObservabilityPanel />
          <ModelPanel />
        </section>
      </main>

      <div className="mx-auto max-w-7xl px-6 pb-8">
        <HistoryList />
      </div>
    </div>
  );
}
