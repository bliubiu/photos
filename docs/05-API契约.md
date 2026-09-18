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
| GET | `/models` | 模型注册表与校验状态 |
| POST | `/models/download` | 一键下载指定（缺省为全部缺失）模型 |
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
  "pdf": false
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
      "message": "模型文件不存在"
    }
  ]
}
```

`check_status`：`ready` | `missing` | `hash_mismatch` | `cached_ok`（与 `04-模型清单.md` §5 一致）。

### 3.9 POST `/models/download`

`Content-Type: application/json`，请求体可省略：

```json
{ "ids": ["birefnet_lite", "retinaface"] }
```

| 字段 | 类型 | 约束 |
|---|---|---|
| `ids` | string[] | 可选；缺省或空数组时下载全部「文件缺失」（`check_status=missing`）的模型；含未注册 id 返回 `400` |

逐个模型下载到注册表路径（`[models.<id>].path`），下载地址取 `[models.<id>].download.url`。已存在的文件直接跳过视为成功；单个模型失败**不阻断**其余，逐项返回中文原因。

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
  "output": { "format": "jpg", "jpg_quality": 90, "pdf": false }
}
```

选项来源：`application.toml` + 默认值；驱动前端下拉，前端不硬编码尺寸表。

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

## 4. 实现约束

- 契约变更须同步：本文件、`02-架构设计.md` 存储字段（若涉及）、`03-实施计划.md` M3、CHANGELOG。
- 错误码建议枚举（持续扩充）：`INVALID_PARAMS`、`MODEL_MISSING`、`FILE_TOO_LARGE`、`UNSUPPORTED_MEDIA`、`TASK_NOT_FOUND`、`ARTIFACT_NOT_FOUND`、`INTERNAL`。
- 多任务并发上限与队列策略实现期确定；契约层先保证单任务状态机正确。
- 安全与脱敏见 [`06-安全设计.md`](06-安全设计.md)。

## 5. 参考

- 架构运行拓扑与领域模型：`02-架构设计.md`
- 模型状态：`04-模型清单.md`
- 产品侧 CLI/WebUI 需求：`01-PRD产品需求说明书.md` §4
