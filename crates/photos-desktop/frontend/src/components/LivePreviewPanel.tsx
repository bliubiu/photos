import { useEffect, useRef } from "react";
import { useStore } from "../store";
import { outputUrl } from "../api";

/** 预览渲染最长边（超出则等比缩小，控制逐像素处理开销） */
const PREVIEW_MAX_SIDE = 800;

interface BeautyOption {
  /** 磨皮强度 0..=1 */
  smooth: number;
  /** 提亮强度 0..=1 */
  brighten: number;
  /** 美白强度 0..=1 */
  whiten: number;
}

/**
 * 本地近似渲染：与 photos-core 的美颜顺序一致（磨皮 → 提亮 → 美白），
 * 再按所选底色合成；底层为透明底产物时可真实换底。
 */
function renderPreview(
  el: HTMLCanvasElement,
  img: HTMLImageElement,
  bg: string | null,
  beauty: BeautyOption,
) {
  const ctx = el.getContext("2d");
  if (!ctx) return;
  const sw = img.naturalWidth;
  const sh = img.naturalHeight;
  const scale = Math.min(1, PREVIEW_MAX_SIDE / Math.max(sw, sh));
  const w = Math.max(1, Math.round(sw * scale));
  const h = Math.max(1, Math.round(sh * scale));
  el.width = w;
  el.height = h;

  // 美颜在独立图层上处理，便于按原图 alpha 还原透明边缘
  const layer = document.createElement("canvas");
  layer.width = w;
  layer.height = h;
  const lc = layer.getContext("2d");
  if (!lc) return;
  lc.drawImage(img, 0, 0, w, h);

  // 磨皮近似：模糊层按强度叠加（后端为五官保护区加权的双边滤波）
  if (beauty.smooth > 0) {
    const blur = document.createElement("canvas");
    blur.width = w;
    blur.height = h;
    const bc = blur.getContext("2d");
    if (bc) {
      const radius = ((Math.max(w, h) / 300) * beauty.smooth).toFixed(2);
      bc.filter = `blur(${radius}px)`;
      bc.drawImage(layer, 0, 0);
      bc.filter = "none";
      lc.globalAlpha = 0.5 * Math.min(1, beauty.smooth);
      lc.drawImage(blur, 0, 0);
      lc.globalAlpha = 1;
      restoreAlpha(lc, img, w, h);
    }
  }

  // 提亮近似：加法叠加等效于后端「逐通道 +255×强度」
  if (beauty.brighten > 0) {
    const delta = Math.round(255 * Math.min(1, beauty.brighten));
    lc.globalCompositeOperation = "lighter";
    lc.fillStyle = `rgb(${delta},${delta},${delta})`;
    lc.fillRect(0, 0, w, h);
    lc.globalCompositeOperation = "source-over";
    restoreAlpha(lc, img, w, h);
  }

  // 美白近似：肤色像素向白色靠拢（与后端 is_skin 同规则，仅作用皮肤像素）
  if (beauty.whiten > 0) {
    const k = Math.min(1, beauty.whiten);
    const data = lc.getImageData(0, 0, w, h);
    const px = data.data;
    for (let i = 0; i < px.length; i += 4) {
      const r = px[i];
      const g = px[i + 1];
      const b = px[i + 2];
      if (r > 95 && g > 40 && b > 20 && r > g && r > b && r - g > 15) {
        px[i] = Math.round(r * (1 - k) + 255 * k);
        px[i + 1] = Math.round(g * (1 - k) + 255 * k);
        px[i + 2] = Math.round(b * (1 - k) + 255 * k);
      }
    }
    lc.putImageData(data, 0, 0);
  }

  ctx.clearRect(0, 0, w, h);
  if (bg) {
    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, w, h);
  }
  ctx.drawImage(layer, 0, 0);
}

/** 用原图 alpha 还原图层透明度（避免模糊/加法叠加污染透明区域） */
function restoreAlpha(ctx: CanvasRenderingContext2D, img: HTMLImageElement, w: number, h: number) {
  ctx.globalCompositeOperation = "destination-in";
  ctx.drawImage(img, 0, 0, w, h);
  ctx.globalCompositeOperation = "source-over";
}

/** 本地近似实时预览：在最近产物上实时叠加美颜与所选底色（最终以后端出图为准） */
export default function LivePreviewPanel() {
  const { detail, selectedId, params, config } = useStore();
  const canvasRef = useRef<HTMLCanvasElement>(null);

  // 基准图优先透明底产物（可真实换底），否则用首个证件照（仅美颜可预览）
  const transparent = detail?.artifacts.find(
    (a) => a.kind === "id_photo" && a.background === "transparent",
  );
  const base = transparent ?? detail?.artifacts.find((a) => a.kind === "id_photo");
  const src = selectedId && base ? outputUrl(selectedId, "id_photo", base.background ?? undefined) : null;

  // 当前参数对应的底色：自定义色优先，其次首个所选内置底色（无透明底产物时无法本地换底）
  const builtin = config?.backgrounds.find((b) => b.id === params.backgrounds[0]);
  const bgHex = transparent ? (params.customBg ?? (builtin ? `rgb(${builtin.rgb.join(",")})` : null)) : null;

  const beauty: BeautyOption = params.beautyEnabled
    ? {
        smooth: params.beautySkinSmooth,
        brighten: params.beautyBrighten,
        whiten: params.beautyWhiten,
      }
    : { smooth: 0, brighten: 0, whiten: 0 };

  useEffect(() => {
    const el = canvasRef.current;
    if (!el || !src) return;
    const img = new Image();
    img.onload = () => renderPreview(el, img, bgHex, beauty);
    img.src = src;
    // beauty 由 params 字段组合而成，逐项作为依赖以便强度变动时重绘
  }, [
    src,
    bgHex,
    beauty.smooth,
    beauty.brighten,
    beauty.whiten,
  ]);

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
            <canvas ref={canvasRef} className="block max-h-[420px] w-auto" />
          </div>
          <p className="mt-2 text-xs text-gray-500">
            底色：{bgHex ?? "未选择（保持透明）"}
            {params.customBg ? "（自定义）" : ""} · 美颜：
            {params.beautyEnabled
              ? `磨皮 ${Math.round(params.beautySkinSmooth * 100)}% / 提亮 ${Math.round(
                  params.beautyBrighten * 100,
                )}% / 美白 ${Math.round(params.beautyWhiten * 100)}%`
              : "关闭"}
          </p>
          {!transparent && (
            <p className="mt-1 text-[11px] text-gray-400">
              底色预览需任务输出透明底 PNG：勾选「输出透明底 PNG（带 alpha 通道）」后重新处理即可。
            </p>
          )}
        </div>
      ) : (
        <p className="py-8 text-center text-sm text-gray-400">
          处理完成或选择历史任务后，可在此实时查看底色与美颜的近似效果。
        </p>
      )}
    </section>
  );
}