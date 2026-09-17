# 更新日志

项目版本采用 CalVer（日历版本）：`YYYY.MM.DD.MICRO`。正式发布在稳定分支打 Tag，Tag 名称与版本号一致。

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