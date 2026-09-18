import { useStore } from "../store";
import { outputUrl } from "../api";

/** 本地近似实时预览：在透明底产物上实时合成当前所选底色（最终以后端出图为准） */
export default function LivePreviewPanel() {
  const { detail, selectedId, params, config } = useStore();

  // 基准图：任务输出的透明底 PNG（含 alpha，可真实换底）
  const transparent = detail?.artifacts.find(
    (a) => a.kind === "id_photo" && a.background === "transparent",
  );
  const src = selectedId && transparent ? outputUrl(selectedId, "id_photo", "transparent") : null;

  // 当前参数对应的底色：自定义色优先，其次首个所选内置底色
  const builtin = config?.backgrounds.find((b) => b.id === params.backgrounds[0]);
  const bgHex = params.customBg ?? (builtin ? `rgb(${builtin.rgb.join(",")})` : null);

  return (
    <section className="rounded-lg bg-white shadow-sm border border-gray-200 p-4">
      <div className="mb-3 flex items-center justify-between gap-2">
        <h2 className="text-sm font-semibold text-gray-700">4. 实时预览</h2>
        <span className="shrink-0 rounded border border-amber-200 bg-amber-50 px-1.5 py-0.5 text-[11px] text-amber-700">
          本地近似预览，最终以后端出图为准
        </span>
      </div>

      {src ? (
        <div className="flex flex-col items-center">
          <div
            className="overflow-hidden rounded-md border border-gray-200"
            style={{ backgroundColor: bgHex ?? "transparent" }}
          >
            <img src={src} alt="换底色实时预览" className="block max-h-[420px]" />
          </div>
          <p className="mt-2 text-xs text-gray-500">
            底色：{bgHex ?? "未选择（保持透明）"}
            {params.customBg ? "（自定义）" : ""}
          </p>
        </div>
      ) : (
        <p className="py-8 text-center text-sm text-gray-400">
          底色实时预览需要当前任务已输出透明底 PNG。
          <br />
          在参数面板勾选「输出透明底 PNG（带 alpha 通道）」后重新处理，即可在此实时切换底色查看效果。
        </p>
      )}
    </section>
  );
}