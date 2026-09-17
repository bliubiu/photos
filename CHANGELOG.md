# 更新日志

项目版本采用 CalVer（日历版本）：`YYYY.MM.DD.MICRO`。正式发布在稳定分支打 Tag，Tag 名称与版本号一致。

## [2026.09.17.6] - 0.1.0

### ✨ New Features 新增功能
- 【M4 美颜算子】新增 `vision/beauty.rs`：逐通道双边滤波磨皮（保细节混合）、提亮、肤色检测美白（经典 RGB 肤色规则，防误判背景）；`[beauty]` 配置 `skin_smooth`/`brighten`/`whiten`（0..1），`enabled=false` 原样返回
- 【M4 链路接入】`ProcessRequest.beauty` 升级为 `Option<BeautyParams>`（enabled + 三项可选强度，缺省取全局配置）；pipeline 换底色前对旋转后原图应用美颜，证件照与效果图同步生效
- 【M4 CLI】`--beauty` 开关 + `--beauty-smooth`/`--beauty-brighten`/`--beauty-whiten` 强度参数（缺省用配置默认值）
- 【M4 API】`params.beauty` 完整透传（enabled + 强度，越界返回 400）；`GET /tasks/{id}` 响应新增 `beauty` 字段；任务记录完整保存美颜参数
- 【M4 CUDA】`InferenceEngine::load` 增加 `ExecutionProvider` 参数：模式套件 `execution_provider=cuda` 时优先 CUDA EP，新增可选 feature `ort-cuda`，未编译时自动降级 CPU 并中文告警（PRD 降级策略）
- 【M4 批量报告】CLI 批量处理结束汇总「成功率 + 总耗时 + 平均耗时/张」

### 📈 Improvements 性能/体验优化
- 【M4 体积】workspace `[profile.release]` 启用 `lto=thin` + `codegen-units=1` + `strip=symbols` + `panic=abort`，从源头减小发布体积

### 📚 Docs 文档更新
- `docs/03-实施计划.md`：M4 落地说明与验证数据
- `docs/05-API契约.md`：`beauty` 参数完整约束（0..1 越界 400）+ 任务详情 `beauty` 字段

### 🔧 Dependencies 依赖更新
- `photos-core` 新增可选 feature `ort-cuda`（= `ort/cuda`，需配合 `ort` 使用）

## [2026.09.17.5] - 0.1.0

### ✨ New Features 新增功能
- 【M3 API】`photos-api` 实现 `docs/05-API契约.md` 全部 7 端点：POST /tasks（multipart 提交 + 202 任务 id）、GET /tasks/{id}（轮询状态/告警/产物清单）、GET /tasks/{id}/output（单产物下载 + bundle zip）、GET /tasks（历史分页列表）、GET /models、GET /config、GET /ping
- 【M3 任务状态机】queued → running → succeeded | failed：入库即返回任务 id，后台异步推理（spawn_blocking 不阻塞请求）；产物按 CLI 命名规约落盘 `data/out/`
- 【M3 错误契约】统一错误体 `{code, message}`（全中文）：INVALID_PARAMS / MODEL_MISSING / FILE_TOO_LARGE / UNSUPPORTED_MEDIA / TASK_NOT_FOUND / ARTIFACT_NOT_FOUND / INTERNAL
- 【M3 模型预检】POST /tasks 创建时按模式套件三件套预检模型就绪，缺失返回 503 MODEL_MISSING（不自动下载阻塞请求）
- 【M3 打包下载】`artifact=bundle` 以 zip 打包该任务全部产物（zip 2 纯 Rust）
- 【M3 serve】`photos serve` 子命令：默认绑定 127.0.0.1 随机端口并打印访问地址，支持 `--host`/`--port`；桌面壳与无头服务共用 `photos-api`
- 【M3 前端】React19 + Tailwind + Zustand 单页三区（上传/参数/预览）+ 历史任务列表 + 下载（单产物/zip bundle），UI 全中文；生产构建产物由 photos-api 同源静态托管，`http://127.0.0.1:端口/` 直接打开 WebUI
- 【M3 批量】前端多选文件逐个提交任务；任务并发由后台队列执行（spawn_blocking，不阻塞请求）
- 【M3 演示引擎】未编译 ort 的构建中 serve 自动使用内置 demo 引擎（按输入图尺寸回放），模型缺失时仍可演示全链路；`/models` 反映真实模型状态
- 【M3 桌面壳】`photos-desktop` Tauri 2 壳：启动即内嵌 Axum 服务（127.0.0.1 随机端口）并托管前端，WebView 加载本地页面；`photos gui` 一键启动桌面版
- 【M3 并发控制】推理队列信号量并发上限（默认 2），批量提交排队执行，避免挤爆 CPU
- 【M3 存储】task_history 支持分页查询（`list_tasks_paged` + `count_tasks`）

### 📈 Improvements 性能/体验优化
- 【M3 并发】推理任务放入 `spawn_blocking`，HTTP 请求不被 CPU 密集推理阻塞
- 【M3 演示】未编译 ort 的构建下 serve/桌面壳自动降级 demo 引擎，无模型环境可完整演示 WebUI

### 📚 Docs 文档更新
- 更新 `docs/03-实施计划.md`：M3 落地说明与验证方式（serve/桌面壳/前端构建命令）

### 🔧 Dependencies 依赖更新
- 新增 `axum`（multipart）、`tokio`、`tower-http`、`zip`、`mime`（workspace 统一管理）

## [2026.09.17.4] - 0.1.0

### ✨ New Features 新增功能
- 【M2 排版】新增 `vision/layout` 排版引擎：相纸毫米→像素换算（按 DPI）、行列计算（边距+间距约束）、证件照居中铺版；6 寸 @300dpi 出一寸照 3×4、A4 @300dpi 出 7×7
- 【M2 多底色】`run_pipeline` 支持底色列表 1..N：检测/抠图/纠偏只做一次，换底色（廉价 alpha 混合）按底色重复，一次请求出齐红白蓝多张证件照
- 【M2 效果图】通用效果图出口：`--effect` 输出每底色各一张换底后全图尺寸效果图（抠图换底图）
- 【M2 排版产物】`--layout 6inch|a4` 以首个底色证件照按相纸规格铺版输出整版相纸
- 【M2 批处理】`photos process` 支持文件夹批量：递归收集 jpg/jpeg/png/webp/bmp，逐个落库 task_history，真实引擎复用一次装载
- 【M2 命名规约】输出命名：`task_{id}_{size}_{bg}.jpg`（证件照）、`task_{id}_effect_{bg}.jpg`（效果图）、`task_{id}_layout_{相纸}.jpg`（排版）
- 【M2 CLI】参数面补齐：`-b/--backgrounds` 多值逗号分隔、`--rotate`、`--effect`、`--layout`、`--beauty`（参数面预留，美颜算子属 M4）

### 📈 Improvements 性能/体验优化
- 【M2 批处理】批量处理结束汇总「成功/失败」统计，单文件失败不中断其余文件

## [2026.09.17.3] - 0.1.0

### 🐛 Bug Fixes  问题修复
- 【M1 人脸】RetinaFace 解码对齐 Hivision 官方约定：prior 中心相对特征图尺寸归一化（此前用网格坐标导致人脸框超出画面十余倍）、中心偏移/宽高缩放分别用 variance[0]=0.1/[1]=0.2、landmark 五点统一中心偏移公式
- 【M1 抠图】BiRefNet mask 还原改为 letterbox 逆变换（先裁出内容区再等比缩放），修复整幅画布 resize 导致的几何畸变与 mask 错位
- 【M1 验证】真实推理端到端出图通过：多人合影测试图正确检测 4 张人脸、白底替换生效、输出 295x413 一寸照

## [2026.09.17.2] - 0.1.0

### ✨ New Features 新增功能
- 【M1 模型】模型缺失自动下载：`photos process` 处理时检测到模型文件缺失，按注册表下载地址静默自动补全（无需手动执行 `photos models download`），下载失败给出中文指引

## [2026.09.17.1] - 0.1.0

### ✨ New Features 新增功能
- 【M1 模型】`photos models download` 一键下载：ureq 纯 Rust 实现（不依赖外部 curl），按模型注册表下载至 `models/` 并 sha256 校验
- 【M1 前处理】`preprocess::build_input` 按模型 `input_dims` 真实构造输入：letterbox 等比缩放灰边填充（NCHW）/ 直接 resize（NHWC）；RetinaFace 官方预处理 RGB 减均值 (104,117,123)；MoveNet int32 像素适配；BiRefNet 输出 logits 过 sigmoid 归一化（概率图羽化）
- 【M1 人脸】`decode_retinaface` SSD prior 解码：对齐 Hivision 官方 retinaface_r50 输出（loc 偏移、`[1,N,2]` 双列分数取人脸分、landmark 偏移），prior 生成与解码公式（variance 0.1/0.2）、坐标还原与 NMS
- 【M1 流水线】`run_pipeline` 接入真实推理输入：三模型真实输出布局对齐（RetinaFace [bbox, conf, landmark] 顺序重排）、letterbox 逆变换还原人脸框、mask resize 回原图

### 📈 Improvements 性能/体验优化
- 【M1 验证】balanced 三件套（RetinaFace R50 + MoveNet-Lightning + BiRefNet-Lite）真实 ONNX 推理打通端到端出图：一寸白底证件照，背景替换、裁切缩放正确
- 【M1 降级】头像特写等无双肩/双眼场景自动降级：跳过自动纠偏并中文告警，继续出图

### 🔧 Dependencies 依赖更新
- 新增 `ureq`（workspace 统一管理，模型下载使用）

## [2026.09.17.0] - 0.1.0

### ✨ New Features 新增功能
- 【M0 骨架】Cargo workspace 五 crate（photos-core/api/cli/desktop/infra），依赖方向 cli→api→core、infra 供 core
- 【M0 配置】`application.toml` 解析：优先级 命令行 > 环境变量（`PHOTOS_*`）> toml > 默认值；三套模式套件、模型注册表、尺寸/底色/布局/美颜
- 【M0 日志】tracing 中文日志格式 + 文件日轮转（photos-YYYYMMDD.log，保留 32 天）+ 终端双写 + 敏感信息脱敏
- 【M0 存储】sqlite（WAL）三表：`task_history` / `kv_cache` / `prefs`
- 【M0 模型】模型注册表状态报告：惰性 sha256 校验 + 路径/mtime/size 缓存命中（<100ms）
- 【M0 CLI】`photos models` 子命令：表格/JSON 输出就绪状态与缺失指引
- 【M1 推理】`InferenceEngine` 抽象 + `TensorData`；`FakeEngine` mock 回放（默认），`OrtEngine` ONNX Runtime 真后端（`feature = "ort"` 门控，模型定版后就位）
- 【M1 视觉】纯 Rust 算子：姿态角度（0.6 头部 + 0.4 肩线融合、±22° 自动阈值、±45° 手动覆盖）、同步仿射纠偏、RetinaFace 解码 NMS、MoveNet 17 点解码、mask 阈值/开运算/羽化、alpha 换底色、证件照裁剪
- 【M1 流水线】`run_pipeline` 最小闭环：读图 → 检测/抠图 → 角度决策 → 同步纠偏 → 换底色 → 裁切缩放（mock 回放打通端到端）
- 【M1 CLI】`photos process` 子命令：单图处理、task_history 落库、缺模型中文指引
- 【M1 CLI】`photos process --demo` 演示模式：内置 mock 回放（居中椭圆人形 mask）无模型跑通全链路，验证纠偏/换底/裁切输出

### 🔧 Dependencies 依赖更新
- `ort` 2.0.0-rc.13 改为显式 feature（默认关闭），规避 Windows 构建文件锁与 DirectML 默认下载问题

### 📚 Docs 文档更新
- 实施计划、架构设计引用同步（目录结构调整为 `crates/*`，新增 `photos-infra`）

## [2026.09.16.4] - 0.1.0

### 📚 Docs 文档更新
- 【文档审查】AGENTS 纠偏规则对齐 B+C：超阈值降级继续出图并告警，手动角度覆盖上限 ±45°
- 【文档审查】统一 CHANGELOG 标题格式为 `[CalVer] - SemVer`（补 `.1/.2/.3` 缺失后缀）
- 【文档审查】安全设计新增「本地路径脱敏」规则与「业务字段白名单」例外；修正来源文件名与章节编号
- 【文档审查】修正模型清单 §8 维护规则病句，统一 `models/` 路径写法
- 【文档评审】PRD 版本号同步至 .4；补 quality 无 CUDA 自动降级 CPU 并告警的说明；统一 RMBG 措辞为 RMBG-1.4/2.0
- 【架构设计】§4.1 改为结构骨架并指向唯一权威样例 `docs/examples/application.toml`，消除双份维护漂移
- 【架构设计】pipeline 输入统一为 RGB 色彩空间；补头部角定义（双眼连线与水平夹角）
- 【API契约】`GET /tasks` 列表补 `outputs` 产物路径字段；`POST /tasks` 成功状态码标注 202；下载文件名与 `artifacts[].filename` 关系说明
- 【安全设计】密钥存储路径改为具体 `.photos` 目录；SM4-GCM-SIV 标注待定；来源引用同步新文件名
- 【实施计划】M0 移除冗余 `git init`
- 【各文档】版本号统一升至 2026.09.16.4

## [2026.09.16.3] - 0.1.0

### 📚 Docs 文档更新
- 新增 `docs/05-API契约.md`（任务状态机、端点示例、错误码；架构 §7 改为引用）
- 安全规范由 `.agents/` 迁至 `docs/06-安全设计.md`，区分「脱敏强制 / 配置加密预留」
- 修正测试覆盖率表述：算法与状态机高覆盖，取消全仓 100% 门槛（PRD/实施计划/架构/AGENTS）
- 统一 WebUI「历史任务列表」为 M3 必含（PRD/架构/实施计划）

## [2026.09.16.2] - 0.1.0

### 📚 Docs 文档更新
- 新增 `docs/04-模型清单.md`：权威模型注册表、预设映射、校验状态、放置/下载与许可证登记
- 架构/实施计划/PRD/配置样例改为引用模型清单，避免双份维护

## [2026.09.16.1] - 0.1.0

### 📚 Docs 文档更新
- 【实施计划】修正 M1 笔误「AARR 抠图」为「AI 人像抠图（BiRefNet-Lite）」
- 【架构设计】补充 API 最小契约与任务状态机（queued/running/succeeded/failed；告警并入 succeeded.warnings）
- 【架构设计】补充 `application.toml` 完整样例与模型清单；新增 `docs/examples/application.toml` 作为 M0 解析夹具

## [2026.09.16.0] - 0.1.0

### ✨ New Features 新增功能
- 【文档体系】补全 PRD 产品需求说明书（含任务模型、模式套件、纠偏容错、模型管理、数据边界等设计访谈确认项）
- 【文档体系】重写架构设计文档（四 crate 工作区、纯 Rust 图像栈、纯 HTTP 通信、本地 HTTP 服务、存储设计）
- 【文档体系】新增实施计划（M0~M4 里程碑、测试策略、变更纪律）
- 【文档体系】新增领域术语表 `CONTEXT.md` 与首个架构决策记录 `docs/adr/0001-纯Rust图像栈.md`

### 📚 Docs 文档更新
- 初始化 CHANGELOG.md 与版本规范说明

### ✅ 技术决策确认（经设计访谈）
- 产品定位：通用人像处理引擎 + 证件照场景输出（方案 B），通用能力以 CLI 子命令与 Web UI 双形态暴露
- 图像处理栈：纯 Rust（image + imageproc + nalgebra + 自研算子），弃用 OpenCV-RS（ADR-0001）
- 模型供给：本地目录发现 + sha256 惰性校验 + 可选一键下载
- 前后端通信：唯一通道本地 HTTP（Axum 内嵌 Desktop），非 Tauri IPC
- 进程拓扑：photos-core / photos-api / photos-cli / photos-desktop 四 crate 工作区
- 任务模型：单请求多产物（一次重计算、多底色轻合成）
- 数据边界：只存元数据，不落库图像像素/人脸框/关键点
- 纠偏容错：超自动阈值降级继续并告警；手动角度覆盖上限 ±45°
- WebUI：本阶段砍掉 ECharts（保留 Zustand），单页三区 + 历史任务列表
- 本次推进范围：M0 骨架 + M1 核心链路最小闭环