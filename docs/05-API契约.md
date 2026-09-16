# API 契约

> 版本：2026.09.16.3（CalVer）
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
| `id_photo` | `background` | 某底色证件照 |
| `layout` | `layout`（`6inch`\|`a4`） | 排版相纸 |
| `effect` | — | 通用效果图 |
| `bundle` | — | 全部产物打包 zip（仅下载侧） |

## 2. 端点总表

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/tasks` | multipart 提交，立即返回任务 id（202 语义） |
| GET | `/tasks/{id}` | 轮询状态、告警、产物清单 |
| GET | `/tasks/{id}/output` | 下载指定产物 |
| GET | `/tasks` | 历史列表（读 `task_history`） |
| GET | `/models` | 模型注册表与校验状态 |
| GET | `/config` | 驱动前端下拉的选项集 |
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
  "rotate": null,
  "layout": null,
  "effect_image": false
}
```

| 字段 | 类型 | 约束 |
|---|---|---|
| `mode` | string | `speed`\|`balanced`\|`quality`；缺省用 `general.default_mode` |
| `size` | string | 必须存在于 `[sizes.*]` |
| `backgrounds` | string[] | 1..N，元素存在于 `[backgrounds.*]` |
| `beauty` | object | 可选；`enabled` 默认 false |
| `rotate` | number\|null | 手动纠偏角（度），`[-45,45]`；null=自动 |
| `layout` | string\|null | `6inch`\|`a4`\|null |
| `effect_image` | bool | 是否输出通用效果图 |

#### 成功响应

```json
{ "id": "task_17c0f0a2", "status": "queued", "created_at": "2026-09-16 12:00:00.000" }
```

| 情况 | 状态码 |
|---|---|
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
  "elapsed_ms": 2345,
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

返回二进制流；`Content-Disposition` 带 UTF-8 文件名。参数不匹配或产物不存在：`404`。

### 3.4 GET `/tasks`

查询：`?limit=20&offset=0`（默认 limit=20，最大建议 100）。

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
      "elapsed_ms": 2345
    }
  ]
}
```

说明：列表供 WebUI **历史任务**（M3 必含）与 CLI 排查使用；只返回元数据，不含像素。

### 3.5 GET `/models`

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

### 3.6 GET `/config`

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
  "layouts": [{ "id": "6inch", "name": "6寸相纸" }, { "id": "a4", "name": "A4" }]
}
```

选项来源：`application.toml` + 默认值；驱动前端下拉，前端不硬编码尺寸表。

### 3.7 GET `/ping`

```json
{ "status": "ok" }
```

## 4. 实现约束

- 契约变更须同步：本文件、`02-架构设计.md` 存储字段（若涉及）、`03-实施计划.md` M3、CHANGELOG。
- 错误码建议枚举（持续扩充）：`INVALID_PARAMS`、`MODEL_MISSING`、`FILE_TOO_LARGE`、`UNSUPPORTED_MEDIA`、`TASK_NOT_FOUND`、`ARTIFACT_NOT_FOUND`、`INTERNAL`。
- 多任务并发上限与队列策略实现期确定；契约层先保证单任务状态机正确。
- 安全与脱敏见 [`06-安全设计.md`](06-安全设计.md)。

## 5. 参考

- 架构运行拓扑与领域模型：`02-架构设计.md`
- 模型状态：`04-模型清单.md`
- 产品侧 CLI/WebUI 需求：`01-PRD产品需求说明书.md` §4
