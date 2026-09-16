# 更新日志

项目版本采用 CalVer（日历版本）：`YYYY.MM.DD.MICRO`。正式发布在稳定分支打 Tag，Tag 名称与版本号一致。

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