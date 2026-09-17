import { useRef } from "react";
import { useStore } from "../store";

export default function UploadPanel() {
  const { files, setFiles, submitting } = useStore();
  const inputRef = useRef<HTMLInputElement>(null);

  return (
    <section className="rounded-lg bg-white shadow-sm border border-gray-200 p-4">
      <h2 className="mb-3 text-sm font-semibold text-gray-700">1. 上传图片</h2>
      <button
        type="button"
        disabled={submitting}
        onClick={() => inputRef.current?.click()}
        className="w-full rounded-md border-2 border-dashed border-gray-300 px-4 py-6 text-sm text-gray-500 hover:border-blue-400 hover:text-blue-500 disabled:opacity-50"
      >
        点击选择图片（支持多选批量处理）
      </button>
      <input
        ref={inputRef}
        type="file"
        accept="image/jpeg,image/png,image/bmp"
        multiple
        className="hidden"
        onChange={(e) => setFiles(Array.from(e.target.files ?? []))}
      />
      {files.length > 0 && (
        <ul className="mt-3 space-y-1 text-xs text-gray-600 max-h-40 overflow-auto">
          {files.map((f, i) => (
            <li key={i} className="flex items-center justify-between rounded bg-gray-50 px-2 py-1">
              <span className="truncate">{f.name}</span>
              <span className="ml-2 shrink-0 text-gray-400">
                {(f.size / 1024).toFixed(1)} KB
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
