# API 契约

> 版本：2026.09.16.4（CalVer）
> 状态：M3 可实现的最小契约；OpenAPI 可后续从本文导出
> 关联：`02-架构设计.md`、`03-实施计划.md`、`04-模型清单.md`
> 实现边界：`photos-api`；CLI `serve` 与 WebUI **只走本契约**，不另开私有接口

## 1. 通则

| 项 | 约定 |
|---|---|
| 传输 | 本地 HTTP（默认 `127.0.0.1`），JSON UTF-8 |
| 轮询 | 建议 200ms 粒度；不引入 webhook / SSE |
| 统一错误体 | `{ "code": "MODEL_MISSING", "message": "模型缺失：birefnet_lite，请手动放置或启用下载" }` |
| `message` | 必为中文，且已脱敏 |
| 鉴权 | 本阶段无远程鉴权；仅绑定回环地址 |

### 1.1 任务状态机

```
        POST /tasks 接受
              │
              ▼
           queued ──────────────┐
              │ 处理开始         │ 处理失败（校验/推理/IO）
              ▼                 ▼
           running ──────► failed
              │
              │ 成功结束（可携带 warnings）
              ▼
         succeeded
```

| 状态 | 含义 | 终态 |
|---|---|---|
| `queued` | 已入库、未开始推理 | 否 |
| `running` | pipeline 执行中 | 否 |
| `succeeded` | 出图完成；可能仍有 `warnings`（如角度超限降级） | 是 |
| `failed` | 任务失败；`message` 为中文原因 | 是 |

说明：

- **告警不单独成状态**：`|θ_final|>22°` 未自动纠偏等为 `succeeded` + `warnings[]`（不阻断任务）。
- 状态持久化到 `task_history.status`，枚举与 API 一致。
- 单请求多产物；进度仅状态粒度（无百分比）。

### 1.2 产物类型

`artifacts[].kind`：

| kind | 额外字段 | 含义 |
|---|---|---|
| `id_photo` | `background` | 某底色证件照；`background=transparent` 为透明底 PNG（`params.transparent` 触发），`background=custombg` 为自定义背景图合成结果（`params.bg_image` 触发） |
| `layout` | `layout`（`6inch`\|`a4`） | 排版相纸（`params.pdf` 为 true 时同一 kind 下另有 `.pdf` 产物，见 §3.1） |
| `effect` | — | 通用效果图 |
| `bundle` | — | 全部产物打包 zip（仅下载侧） |

## 2. 端点总表

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/tasks` | multipart 提交，立即返回任务 id（202 语义） |
| GET | `/tasks/{id}` | 轮询状态、告警、产物清单与提交参数 |
| DELETE | `/tasks/{id}` | 删除任务记录，并连带删除磁盘产物与上传原图 |
| GET | `/tasks/{id}/output` | 下载指定产物 |
| GET | `/tasks/{id}/input` | 读取上传原图（供「原图/结果」对比） |
| GET | `/tasks` | 历史列表（读 `task_history`，支持筛选与分页） |
| DELETE | `/tasks` | 清空历史任务（连带删除磁盘产物与上传原图） |
| GET | `/models` | 模型注册表与校验状态（含角色、版本与内置标记） |
| POST | `/models/download` | 一键下载指定（缺省为全部缺失）模型；可指定 `version` 走市场清单版本化下载 |
| GET | `/models/market` | 模型市场清单（内置 + 用户覆盖），标注可下载与本地已下载/激活状态 |
| POST | `/models/register` | 注册自定义模型（写入 `<data_dir>/models.custom.toml`，重启生效） |
| GET | `/models/versions` | 指定模型的已下载版本与当前激活版本 |
| POST | `/models/activate` | 切换 / 回滚模型激活版本 |
| GET | `/config` | 驱动前端下拉的选项集 |
| GET | `/metrics` | 可观测性指标：任务统计、平均耗时、各阶段平均耗时、错误总数 |
| GET | `/errors` | 可观测性错误上报记录（倒序，`limit` 钳制 1..=200，默认 20） |
| GET | `/ping` | 健康检查 |

## 3. 详细契约

### 3.1 POST `/tasks`

- `Content-Type: multipart/form-data`
- 字段：
  - `file`：图片（jpg/jpeg/png；单文件建议上限 20MB，超限 `413` + 中文 message）
  - `params`：JSON 字符串

#### 请求 `params` 示例

```json
{
  "mode": "balanced",
  "size": "one_inch",
  "backgrounds": ["white", "blue"],
  "beauty": { "enabled": false, "skin_smooth": 0.3, "brighten": 0.2, "whiten": 0.1 },
  "dress": { "enabled": false, "garment_path": null, "style": "suit_navy", "garments": { "top": null, "bottom": null, "shoes": null } },
  "rotate": null,
  "layout": null,
  "effect_image": false,
  "transparent": false,
  "bg_image": null,
  "output_format": "jpg",
  "jpg_quality": 90,
  "pdf": false,
  "steps": ["read_image", "keypoint", "matting", "face_detect", "pose", "rotate", "background"]
}
```

| 字段 | 类型 | 约束 |
|---|---|---|
| `mode` | string | `speed`\|`balanced`\|`quality`；缺省用 `general.default_mode` |
| `size` | string | 内置 id（须存在于 `[sizes.*]`）或自定义形式 `px:宽x高`（如 `px:295x413`，DPI 取 300）/ `mm:宽x高@DPI`（如 `mm:35x45@300`，像素 = mm ÷ 25.4 × DPI 四舍五入）；非法形式返回 `400` |
| `backgrounds` | string[] | 1..N，元素为内置 id（须存在于 `[backgrounds.*]`）或自定义 RGB（`#RRGGBB` / `rgb:R,G,B`）；非法元素返回 `400` |
| `beauty` | object | 可选；`enabled` 默认 false；`skin_smooth`/`brighten`/`whiten` 可选，取值 `[0,1]`，缺省用全局配置 `[beauty]` 默认值（0.3/0.2/0.1）；越界返回 `400` |
| `dress` | object | 可选；`enabled` 默认 false；`garment_path`（服务端服装图路径）、`style`（`suit_navy`\|`suit_black`\|`shirt_white` 上半身，`suit_full_navy`\|`suit_full_black` 全身套装）、`garments`（`{top?, bottom?, shoes?}` 分部位服装图路径，`top`/`bottom`/`shoes` 任一存在即生效，全空视为未提供）三选一，`garments` 优先于 `garment_path`、`garment_path` 优先于 `style`；三者皆缺或 `style` 非法返回 `400` |
| `rotate` | number\|null | 手动纠偏角（度），`[-45,45]`；null=自动 |
| `layout` | string\|null | `6inch`\|`a4`\|null |
| `effect_image` | bool | 是否输出通用效果图 |
| `transparent` | bool | 是否额外输出透明底 PNG（RGBA，alpha 取抠图掩膜）；缺省 false |
| `bg_image` | string\|null | 自定义背景图服务端本地路径；按证件照尺寸 cover 等比铺满并居中裁切后与人像合成，额外出 `background=custombg` 产物；读取失败返回 `400` |
| `output_format` | string\|null | `jpg`\|`webp`（大小写不敏感，`jpeg` 等价 `jpg`）；缺省用 `[output].format`；非法值返回 `400` |
| `jpg_quality` | number\|null | JPG 压缩质量 `1..=100`，缺省用 `[output].jpg_quality`；越界返回 `400`；对 `webp`（VP8L 无损）无效 |
| `pdf` | bool\|null | 是否在排版图片之外额外输出 `task_{id}_layout_{相纸}.pdf`；需同时指定 `layout` 才产出；缺省用 `[output].pdf` |
| `steps` | string[]\|null | 可选；**工作流步骤表**（元素为步骤 id 或 `[pipeline.custom]` 中的自定义步骤名），顺序即执行顺序。缺省 `null` = 取全局配置 `[pipeline] steps`，全局为空时用内置默认十步。步骤 id 未知、重复或依赖缺失返回 `400`；请求了美颜 / 排版 / 换底 / 换装但对应步骤未启用时不报错，仅出图并在 `warnings` 中中文告警 |

#### 步骤 id 与依赖

| 步骤 id | 阶段（metrics.stage） | 依赖（须一并启用） |
|---|---|---|
| `read_image` | 读图 | — |
| `keypoint` | 人体关键点 | `read_image` |
| `matting` | 人像抠图 | `read_image` |
| `face_detect` | 人脸检测 | `read_image` |
| `pose` | 姿态求解 | `keypoint` |
| `rotate` | 几何纠偏 | `pose` |
| `dress` | 换装 | `rotate` |
| `beauty` | 美颜 | `rotate` |
| `background` | 换底裁切 | `rotate` |
| `layout` | 排版 | `background` |

降级规则：缺 `face_detect` → 按整图居中裁切并中文告警；缺 `matting` → 跳过换底合成与透明底输出（仍出图）并中文告警；缺 `keypoint` → 隐藏人脸检测与姿态求解。

#### 成功响应

```json
{ "id": "task_17c0f0a2", "status": "queued", "created_at": "2026-09-16 12:00:00.000" }
```

| 情况 | 状态码 |
|---|---|
| 创建成功 | `202` + 任务 id |
| 参数非法 | `400` + 错误体 |
| 模型缺失 | `503` + `code=MODEL_MISSING` |
| 文件过大 | `413` + 错误体 |

### 3.2 GET `/tasks/{id}`

```json
{
  "id": "task_17c0f0a2",
  "status": "succeeded",
  "message": null,
  "warnings": ["角度超限，本次未自动纠偏（测量角 25.3°）"],
  "mode": "balanced",
  "size": "one_inch",
  "backgrounds": ["white", "blue"],
  "rotate": null,
  "params": {
    "mode": "balanced",
    "size": "one_inch",
    "backgrounds": ["white", "blue"],
    "layout": null,
    "effect_image": false,
    "rotate": null,
    "transparent": false,
    "bg_image": null,
    "output_format": "jpg",
    "jpg_quality": 90,
    "pdf": false
  },
  "beauty": "{ \"enabled\": true, \"skin_smooth\": 0.8, \"brighten\": 0.2, \"whiten\": null }",
  "dress": "{ \"enabled\": true, \"garment_path\": null, \"style\": \"suit_navy\", \"garments\": { \"top\": \"/data/demo_top.jpg\", \"bottom\": \"/data/demo_bottom.jpg\", \"shoes\": null } }",
  "elapsed_ms": 2345,
  "metrics": [
    { "stage": "读图", "ms": 12.3 },
    { "stage": "人体关键点", "ms": 88.4 },
    { "stage": "人像抠图", "ms": 420.1 },
    { "stage": "人脸检测", "ms": 96.7 },
    { "stage": "姿态求解", "ms": 0.4 },
    { "stage": "几何纠偏", "ms": 18.9 },
    { "stage": "换底裁切", "ms": 240.5 }
  ],
  "created_at": "2026-09-16 12:00:00.000",
  "artifacts": [
    {
      "kind": "id_photo",
      "background": "white",
      "filename": "task_17c0f0a2_one_inch_white.jpg"
    },
    {
      "kind": "id_photo",
      "background": "blue",
      "filename": "task_17c0f0a2_one_inch_blue.jpg"
    }
  ]
}
```

`mode`/`size`/`backgrounds`/`rotate` 为受理时归一化后的落库值；`params` 为**提交参数快照**（与受理时一致，尺寸/底色为归一化 id，可原样回传复用），历史库无该列（旧数据）时为 `null`；`metrics` 为分阶段耗时（未采集到或旧数据时为空数组 `[]`，失败任务只含已完成的阶段）。

| 情况 | 行为 |
|---|---|
| 不存在 | `404` |
| `failed` | `message` 非空，`artifacts` 为空数组 |
| `succeeded` | 可非空 `warnings`，`artifacts` 非空 |

### 3.3 GET `/tasks/{id}/output`

| query | 含义 |
|---|---|
| `artifact=id_photo&background=white` | 某底色证件照 |
| `artifact=layout&layout=6inch` | 排版相纸 |
| `artifact=effect` | 通用效果图 |
| `artifact=bundle` | 全部产物 zip |

返回二进制流；`Content-Disposition` 带 UTF-8 文件名（与 `GET /tasks/{id}` 的 `artifacts[].filename` 一致，该字段同时供前端展示与下载定位）。参数不匹配或产物不存在：`404`。

### 3.4 GET `/tasks`

查询：`?limit=20&offset=0`（默认 limit=20，最大建议 100），可叠加筛选条件：

| query | 类型 | 含义 |
|---|---|---|
| `limit` | number | 每页条数，默认 20（上限 100） |
| `offset` | number | 偏移，默认 0 |
| `status` | string | `queued`\|`running`\|`succeeded`\|`failed`；非法值返回 `400` |
| `mode` | string | `speed`\|`balanced`\|`quality`；非法值返回 `400` |
| `size` | string | 尺寸 id（内置 id 或自定义形式，同 §3.1 `size`），受理时归一化后比较 |
| `background` | string | 底色 id（按逗号分隔的 `backgrounds` 列做精确元素匹配，形式同 §3.1 `backgrounds` 元素） |
| `since` | string | 起始创建时间 `YYYY-MM-DD`（按文本比较 `created_at >= since`） |

筛选条件按 AND 组合，且在**后端 SQL 完成**：`total` 为筛选后的总条数，与分页一致。任一筛选值非法统一返回 `400` + `code=INVALID_PARAMS`。

```json
{
  "total": 1,
  "items": [
    {
      "id": "task_17c0f0a2",
      "input_path": "D:/photos/in.jpg",
      "mode": "balanced",
      "size": "one_inch",
      "backgrounds": ["white", "blue"],
      "status": "succeeded",
      "message": null,
      "created_at": "2026-09-16 12:00:00.000",
      "elapsed_ms": 2345,
      "outputs": ["task_17c0f0a2_one_inch_white.jpg", "task_17c0f0a2_one_inch_blue.jpg"]
    }
  ]
}
```

说明：列表供 WebUI **历史任务**（M3 必含）与 CLI 排查使用；只返回元数据，不含像素。`outputs` 为产物文件名数组（与详情 `artifacts` 一致），满足历史任务「结果路径」展示。

自定义尺寸/底色在受理时归一化为文件名安全 id 后落库与命名：`px:295x413` → `px_295x413`、`mm:35x45@300` → `mm_35x45_300`、`#ff0000` → `rgb-ff0000`（产物如 `task_17c0f0a2_px_295x413_rgb-ff0000.jpg`）。归一化 id 可再次提交，解析幂等。

### 3.5 DELETE `/tasks/{id}`

删除任务记录，并**连带删除磁盘产物与上传原图**（仅删除位于输出目录 / 上传目录内的文件，路径越界一律跳过）。

```json
{ "id": "task_17c0f0a2", "deleted_outputs": 3 }
```

| 情况 | 状态码 |
|---|---|
| 删除成功 | `200` + 已删除的磁盘文件数（`deleted_outputs`） |
| 任务不存在 | `404` + `code=TASK_NOT_FOUND` |

### 3.6 DELETE `/tasks`

清空全部历史任务，并连带删除磁盘产物与上传原图。

```json
{ "deleted": 5 }
```

`deleted` 为删除的任务记录数；无记录时返回 `{"deleted": 0}`（仍为 `200`）。

### 3.7 GET `/tasks/{id}/input`

返回该任务**上传原图**的二进制流（`Content-Type` 依扩展名推断），供 WebUI「原图 / 结果」对比展示。

| 情况 | 状态码 |
|---|---|
| 成功 | `200` + 原图字节流 |
| 任务不存在，或原图不在上传目录内（如 CLI 记录的本地任意路径） | `404` + `code=ARTIFACT_NOT_FOUND` |

### 3.8 GET `/models`

```json
{
  "items": [
    {
      "id": "birefnet_lite",
      "path": "models/birefnet-lite.onnx",
      "ready": false,
      "check_status": "missing",
      "message": "模型文件不存在",
      "role": "matting",
      "version": null,
      "versions": ["1.0.0", "1.1.0"],
      "active_version": "1.1.0",
      "builtin": true
    }
  ]
}
```

`check_status`：`ready` | `missing` | `hash_mismatch` | `cached_ok`（与 `04-模型清单.md` §5 一致）。

插件化扩展字段：

| 字段 | 含义 |
|---|---|
| `role` | 模型角色：`auto` \| `face` \| `keypoint` \| `matting` \| `parsing`（注册表未声明时为 `auto`） |
| `version` | **当前实际解析到的版本目录名**；注册表 `path` 直连单文件（旧布局）时为 `null` |
| `versions` | 本地已下载版本列表（`models/<id>/` 下的目录名，升序；无则空数组） |
| `active_version` | sqlite `prefs` 中记录的激活版本；无记录时取最高版本并回写，目录为空时为 `null` |
| `builtin` | 是否属内置注册表（`Config::default().models`）；false 表示来自 `models.custom.toml` 的自定义注册 |

### 3.9 POST `/models/download`

`Content-Type: application/json`，请求体可省略：

```json
{ "ids": ["birefnet_lite", "retinaface"] }
```

| 字段 | 类型 | 约束 |
|---|---|---|
| `ids` | string[] | 可选；缺省或空数组时下载全部「文件缺失」（`check_status=missing`）的模型；含未注册 id 返回 `400` |
| `version` | string\|null | 可选；指定时走**市场清单版本化下载**（落到 `models/<id>/<version>/`），此时 `ids` 必填 |

逐个模型下载到注册表路径（`[models.<id>].path`），下载地址取 `[models.<id>].download.url`。已存在的文件直接跳过视为成功；单个模型失败**不阻断**其余，逐项返回中文原因。

`version` 分支的额外约束：条目须在市场清单中且满足 `downloadable`（`enabled = true` 且 `url`、`sha256` 齐备、`sha256` 非占位），否则该项 `ok = false`（内置条目默认如此，原因形如「条目未启用下载或缺少直链/sha256」）；下载后走完整 sha256 校验，成功后**自动切换激活该版本**。该版本已存在时跳过下载（不改变激活版本），逐项结果附 `version` 字段。

```json
{
  "items": [
    { "id": "retinaface", "ok": false, "message": "模型“retinaface”未配置下载地址（请在配置 [models.retinaface].download.url 填写）" }
  ]
}
```

| 情况 | 状态码 |
|---|---|
| 处理完成（含部分失败） | `200` + 逐项结果 |
| `ids` 含未注册 id | `400` + `code=INVALID_PARAMS` |
| 指定 `version` 但 `ids` 为空 | `400` + `code=INVALID_PARAMS` |

说明：模型体积较大（单个可达数百 MB），本端点同步等待下载完成后返回，**耗时较长且无进度推送**；前端以「下载中」状态提示，完成后重新拉取 `GET /models` 刷新就绪状态。

### 3.10 GET `/config`

```json
{
  "default_mode": "balanced",
  "modes": [
    { "id": "speed", "label": "极速" },
    { "id": "balanced", "label": "CPU 高性能" },
    { "id": "quality", "label": "GPU 高质量" }
  ],
  "sizes": [{ "id": "one_inch", "name": "一寸", "width_px": 295, "height_px": 413 }],
  "backgrounds": [{ "id": "white", "name": "白", "rgb": [255, 255, 255] }],
  "layouts": [{ "id": "6inch", "name": "6寸相纸" }, { "id": "a4", "name": "A4" }],
  "output": { "format": "jpg", "jpg_quality": 90, "pdf": false },
  "pipeline": {
    "steps": [
      { "id": "read_image", "label": "读图", "stage": "读图", "requires": [] },
      { "id": "keypoint", "label": "人体关键点", "stage": "人体关键点", "requires": ["read_image"] }
    ],
    "effective": ["read_image", "keypoint", "matting", "face_detect", "pose", "rotate", "dress", "beauty", "background", "layout"]
  }
}
```

选项来源：`application.toml` + 默认值；驱动前端下拉，前端不硬编码尺寸表。

`pipeline.steps` 为步骤元数据（内置十步 + `[pipeline.custom]` 自定义步骤，`label` 为中文名、`requires` 为依赖 id），`pipeline.effective` 为当前**生效**的步骤表（全局 `[pipeline] steps`，未配置时为内置默认十步）。前端步骤编排面板据此展示开关与调序，提交时通过 `params.steps` 覆盖。

### 3.11 GET `/ping`

```json
{ "status": "ok" }
```

### 3.12 GET `/metrics`

可观测性指标聚合（取最近 200 条已完成任务统计；耗时为毫秒，保留一位小数）。

```json
{
  "tasks": { "total": 12, "queued": 0, "running": 1, "succeeded": 10, "failed": 1 },
  "elapsed_ms": { "avg": 2345.6, "samples": 10 },
  "stages": [
    { "stage": "人脸检测", "avg_ms": 96.7, "samples": 10 },
    { "stage": "人像抠图", "avg_ms": 420.1, "samples": 10 }
  ],
  "errors": { "total": 1 }
}
```

- `elapsed_ms.samples` 为参与均值计算的已完成任务数（无数据时为 0，`avg` 为 0）。
- `stages` 仅统计 `succeeded` 且已落库指标的任务，按阶段名升序。
- 无外部依赖（无 Prometheus 等），数据源为 sqlite `task_history` 与 `error_log`。

### 3.13 GET `/errors`

错误上报记录（倒序）。

| query | 含义 |
|---|---|
| `limit` | 返回条数，钳制 1..=200，默认 20 |

```json
{
  "total": 1,
  "items": [
    {
      "id": 1,
      "created_at": "2026-09-18 10:20:30.000",
      "code": "INTERNAL",
      "stage": "换底裁切",
      "message": "读取背景图 no-such-bg.png 失败：No such file or directory",
      "task_id": "task_17c0f0a2"
    }
  ]
}
```

- `code` 沿用错误码枚举（如 `INTERNAL`、`MODEL_MISSING`）；`stage` 为失败时最后完成的流水线阶段（非流水线场景为业务动作名，如「模型下载」「删除任务」）。
- `task_id` 无关联任务时为 `null`。

### 3.14 GET `/models/market`

模型市场清单：内置清单（随二进制内嵌）+ 用户覆盖文件（`<data_dir>/model_market.toml`）合并后的全部条目，并标注本地状态。

```json
{
  "items": [
    {
      "id": "birefnet_lite",
      "version": "1.0.0",
      "role": "matting",
      "url": "",
      "sha256": "",
      "size": 0,
      "license": "MIT",
      "source": "BiRefNet general（bb_swin_v1_tiny）",
      "enabled": false,
      "downloadable": false,
      "registered": true,
      "downloaded": false,
      "active_version": null
    }
  ]
}
```

| 字段 | 含义 |
|---|---|
| `version` / `role` / `url` / `sha256` / `size` / `license` / `source` / `enabled` | 清单条目原始字段（见 `04-模型清单.md` §7.1） |
| `downloadable` | 是否具备下载条件：`enabled` 且 `url` 非空且 `sha256` 为 64 位非占位 |
| `registered` | 该 id 是否已存在于模型注册表（`[models.<id>]` / 自定义注册表） |
| `downloaded` | 本地 `models/<id>/<version>/` 是否已存在该版本 |
| `active_version` | 该模型当前激活版本（同 `GET /models`） |

说明：内置条目**默认 `enabled=false`、`url`/`sha256` 留空**，故 `downloadable` 为 `false`，前端展示但禁用下载按钮；解析失败（清单格式非法）返回 `500`，并上报错误码 `MODEL_MARKET_INVALID`。

### 3.15 POST `/models/register`

注册自定义模型：写入 `<data_dir>/models.custom.toml`（同 id 覆盖），配置加载时作为第二层覆盖合并。

```json
{
  "id": "my_matting",
  "path": "models/my_matting.onnx",
  "input_dims": [1, 3, 512, 512],
  "role": "matting",
  "sha256": null,
  "preprocess": {
    "layout": "nchw",
    "norm": { "mean_std": { "mean": [0.5, 0.5, 0.5], "std": [0.5, 0.5, 0.5] } },
    "channel": "rgb"
  }
}
```

| 字段 | 类型 | 约束 |
|---|---|---|
| `id` | string | 必填；非空、非 `.`/`..`，仅允许字母、数字与 `_ - .` |
| `path` | string | 必填；非空，`.onnx` 路径（相对项目根或绝对路径） |
| `input_dims` | int[] | 必填；非空 |
| `role` | string\|null | 可选；`auto`\|`face`\|`keypoint`\|`matting`\|`parsing`，缺省 `auto`；非法值返回 `400` |
| `sha256` | string\|null | 可选；64 位十六进制。缺省时：文件已存在则自动计算，文件不存在则返回 `400` |
| `preprocess` | object\|null | 可选；`{ layout, norm, channel }`（缺省 `auto`/`unit`/`rgb`），见 `04-模型清单.md` §2 |

```json
{
  "id": "my_matting",
  "registered": true,
  "replaced": false,
  "restart_required": true,
  "message": "模型“my_matting”已写入 data/models.custom.toml，重启服务后生效"
}
```

| 情况 | 状态码 |
|---|---|
| 注册成功 | `200` + `registered=true` |
| 请求体非法 / id、路径、维度非法 / sha256 非法或缺失且文件不存在 / `role` 或 `preprocess` 声明非法 | `400` + `code=MODEL_REGISTER_INVALID` |

说明：

- `replaced` 表示是否覆盖了同 id 的既有条目；
- 写入前会用「默认配置 + 当前生效模型表 + 候选文件」完整反序列化并 `validate()`，**校验失败不落盘**；
- **`restart_required` 恒为 true**：`AppState.cfg` 为 `Arc<Config>` 不可变，注册结果需重启进程生效。

### 3.16 GET `/models/versions`

| query | 含义 |
|---|---|
| `id` | 模型 id（须已注册） |

```json
{ "id": "birefnet_lite", "versions": ["1.0.0", "1.1.0"], "active_version": "1.1.0" }
```

`versions` 为 `models/<id>/` 下的版本目录名（升序，同一 id 多版本共存）；`active_version` 为 sqlite `prefs` 记录值（无记录时取最高版本并回写，目录为空时为 `null`）。

| 情况 | 状态码 |
|---|---|
| 成功 | `200` |
| 未知模型 id | `400` + `code=MODEL_VERSION_UNKNOWN` |

### 3.17 POST `/models/activate`

切换 / 回滚模型激活版本（`prefs` 键 `model_version:<id>`）。

```json
{ "id": "birefnet_lite", "version": "1.0.0" }
```

```json
{ "id": "birefnet_lite", "active_version": "1.0.0", "message": "已切换激活版本（校验通过）" }
```

| 情况 | 状态码 |
|---|---|
| 切换成功 | `200` |
| 未知模型 id，或该版本未下载（`versions` 不含目标版本） | `400` + `code=MODEL_VERSION_UNKNOWN` |
| 版本目录存在但文件校验失败（缺失 / sha256 不符） | `503` + `code=MODEL_MISSING` |

说明：先判定版本目录是否存在（否则模型类错误会被统一映射为 `503`，语义不符），再读注册表**目标版本文件**做 sha256 校验，**校验通过才写 `prefs`**——校验失败不切换，激活版本保持不变。

## 4. 实现约束

- 契约变更须同步：本文件、`02-架构设计.md` 存储字段（若涉及）、`03-实施计划.md` M3、CHANGELOG。
- 错误码建议枚举（持续扩充）：`INVALID_PARAMS`、`MODEL_MISSING`、`MODEL_REGISTER_INVALID`、`MODEL_VERSION_UNKNOWN`、`FILE_TOO_LARGE`、`UNSUPPORTED_MEDIA`、`TASK_NOT_FOUND`、`ARTIFACT_NOT_FOUND`、`INTERNAL`。
- 多任务并发上限与队列策略实现期确定；契约层先保证单任务状态机正确。
- 安全与脱敏见 [`06-安全设计.md`](06-安全设计.md)。

## 5. 参考

- 架构运行拓扑与领域模型：`02-架构设计.md`
- 模型状态：`04-模型清单.md`
- 产品侧 CLI/WebUI 需求：`01-PRD产品需求说明书.md` §4
