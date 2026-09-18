import { useRef, useState } from "react";
import { useStore } from "../store";

/** 受理的图片扩展名（与后端上传校验一致） */
const IMAGE_EXT = /\.(jpe?g|png|bmp)$/i;

/** 递归收集拖入项：支持文件夹（webkitGetAsEntry），按扩展名过滤图片 */
async function collectImages(dt: DataTransfer): Promise<File[]> {
  const entries: FileSystemEntry[] = [];
  for (const item of Array.from(dt.items)) {
    if (item.kind !== "file") continue;
    const entry = item.webkitGetAsEntry?.();
    if (entry) entries.push(entry);
  }
  // 浏览器不支持 entry 接口时退回扁平文件列表
  if (entries.length === 0) {
    return Array.from(dt.files).filter((f) => IMAGE_EXT.test(f.name));
  }
  const out: File[] = [];
  for (const entry of entries) {
    await walkEntry(entry, out);
  }
  return out;
}

/** 深度遍历文件/文件夹（readEntries 单次最多返回 100 项，需循环读空） */
async function walkEntry(entry: FileSystemEntry, out: File[]): Promise<void> {
  if (entry.isFile) {
    const file = await new Promise<File | null>((resolve) =>
      (entry as FileSystemFileEntry).file(resolve, () => resolve(null)),
    );
    if (file && IMAGE_EXT.test(file.name)) out.push(file);
    return;
  }
  if (!entry.isDirectory) return;
  const reader = (entry as FileSystemDirectoryEntry).createReader();
  for (;;) {
    const batch = await new Promise<FileSystemEntry[]>((resolve) =>
      reader.readEntries(resolve, () => resolve([])),
    );
    if (batch.length === 0) break;
    for (const child of batch) {
      await walkEntry(child, out);
    }
  }
}

export default function UploadPanel() {
  const { files, setFiles, submitting } = useStore();
  const inputRef = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);

  const onDrop = async (e: React.DragEvent<HTMLElement>) => {
    e.preventDefault();
    setDragging(false);
    if (submitting) return;
    const picked = await collectImages(e.dataTransfer);
    if (picked.length === 0) {
      useStore.setState({ error: "未发现可处理的图片（仅支持 JPG / PNG / BMP）" });
      return;
    }
    useStore.setState({ error: null });
    setFiles(picked);
  };

  return (
    <section
      className="rounded-lg bg-white shadow-sm border border-gray-200 p-4"
      onDragOver={(e) => {
        e.preventDefault();
        if (!submitting) setDragging(true);
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDragging(false);
      }}
      onDrop={(e) => void onDrop(e)}
    >
      <h2 className="mb-3 text-sm font-semibold text-gray-700">1. 上传图片</h2>
      <button
        type="button"
        disabled={submitting}
        onClick={() => inputRef.current?.click()}
        className={`w-full rounded-md border-2 border-dashed px-4 py-6 text-sm transition disabled:opacity-50 ${
          dragging
            ? "border-blue-500 bg-blue-50 text-blue-600"
            : "border-gray-300 text-gray-500 hover:border-blue-400 hover:text-blue-500"
        }`}
      >
        {dragging ? "松开鼠标即可导入" : "点击选择，或拖拽图片 / 文件夹到此处（支持多选批量处理）"}
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