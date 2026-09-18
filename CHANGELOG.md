# 更新日志

项目版本采用 CalVer（日历版本）：`YYYY.MM.DD.MICRO`。正式发布在稳定分支打 Tag，Tag 名称与版本号一致。

## [2026.09.18.13] - 0.1.0

### ✨ New Features 新增功能
- 【前端·上传】**拖拽上传（含文件夹）**：上传区支持拖入图片或整个文件夹，通过 `DataTransferItem.webkitGetAsEntry()` 递归遍历目录结构（`readEntries` 循环读空以规避单次最多 100 项的上限），按扩展名过滤 `jpeg/jpg/png/bmp` 并忽略非图片文件；浏览器不支持 entry 接口时回退扁平 `dataTransfer.files`。拖入时上传区高亮并提示「松开鼠标即可导入」，无可用图片时给出中文错误提示；点击选择与多选批量流程保持不变
- 【前端·预览】**前后对比滑块**：预览面板的「对比原图」由并排双图升级为**同区域叠加拖动对比**——处理结果图为底，上传原图按 `clip-path: inset()` 随分割线位置裁切叠加，覆盖透明 range 层拖动即可左右对照，两侧角标区分「原图 / 处理结果」
- 【前端·参数】**参数预设「我的常用参数」**：参数面板顶部支持把当前整套参数（模式/尺寸/底色/排版/纠偏/美颜换装/输出格式等）存为命名预设，同一名称视为覆盖；可随时套用或删除。预设持久化在 `localStorage`（键 `photos.presets`），读写均 try/catch，隐私模式等写入失败场景退化为本次会话可用
- 【前端·预览】**实时预览（本地近似）**：新增实时预览面板，在任务的**透明底 PNG 产物**上实时合成参数面板当前所选底色（含自定义 RGB 取色器），换底效果即时可见；界面固定标注「本地近似预览，最终以后端出图为准」，当前任务无透明底产物时提示勾选「输出透明底 PNG（带 alpha 通道）」后重新处理

### 📚 Docs 文档更新
- `docs/06-运行使用手册.md`：WebUI 章节由「三区」更新为「四区」，补充拖拽上传/文件夹递归、参数预设、对比滑块与实时预览（近似）的说明
- `docs/07-能力增强.md`：前端交互升级（拖拽上传、前后对比滑块、参数预设、实时预览）标记为已落地（2026.09.18.13）

## [2026.09.18.12] - 0.1.0

### ✨ New Features 新增功能
- 【历史记录】**后端筛选与分页**：`GET /tasks` 新增 `status` / `mode` / `size` / `background` / `since` 查询参数（AND 组合，在 SQL 层过滤），`total` 为筛选后总条数、与分页一致；非法筛选值统一返回 `400 INVALID_PARAMS`（`status` 白名单、`mode` 走配置校验、`size`/`background` 受理时归一化后比较，`background` 按逗号分隔列做元素精确匹配）
- 【历史记录】**提交参数快照与一键复用**：`task_history` 新增 `params` 列（v3 幂等迁移，旧库 `ALTER TABLE` 补列），API/CLI 受理时把归一化后的提交参数以 JSON 落库；`GET /tasks/{id}` 响应新增 `mode` / `size` / `backgrounds` / `rotate` / `params` 字段。前端历史列表新增**「复用参数」**：拉取详情后按 `GET /config` 分流内置尺寸/底色与自定义尺寸/RGB，一键回填模式、尺寸、底色、排版、纠偏角、美颜换装、输出格式等参数
- 【历史记录】**删除与清空**：新增 `DELETE /tasks/{id}`（返回 `deleted_outputs`）与 `DELETE /tasks`（返回 `deleted`），删除记录时**连带删除磁盘产物与上传原图**（`purge_task_files` 仅删除位于输出目录 / 上传目录内的文件，路径越界一律跳过）；前端行内「删除」与顶部「清空历史」均带二次确认
- 【历史记录】**原图对比**：新增 `GET /tasks/{id}/input`（返回上传原图字节流与按扩展名推断的 `Content-Type`，非上传目录内文件返回 `404 ARTIFACT_NOT_FOUND`）；预览面板新增**「对比原图」**开关，与处理结果并排展示
- 【存储】`photos-core::storage` 新增 `TaskFilter` 与 `count_tasks_filtered` / `list_tasks_filtered` / `list_all_tasks` / `delete_task` / `clear_tasks`；查询列序收敛为 `TASK_COLUMNS` 常量，SELECT 与行映射下标单点维护

### 🐛 Bug Fixes 问题修复
- 【前端】修复 `api.ts::submitTask` 请求体漏传 `output_format` / `jpg_quality` / `pdf` 的缺陷——此前后端已支持三项输出参数，但前端提交时未携带，导致参数面板选择的输出格式与 PDF 开关实际不生效

### 🔧 Dependencies 依赖更新
- `photos-core` 新增 `rusqlite` 动态查询所需用法（`rusqlite::types::Value` 与 `params_from_iter`），无新增第三方依赖

### 📚 Docs 文档更新
- `docs/05-API契约.md`：端点总表新增 `DELETE /tasks/{id}`、`GET /tasks/{id}/input`、`DELETE /tasks`；`GET /tasks` 补充筛选参数表，新增 3.5–3.7 三个端点小节（原 3.5–3.8 顺延为 3.8–3.11）；`GET /tasks/{id}` 响应补充 `mode`/`size`/`backgrounds`/`rotate`/`params` 与说明
- `docs/06-运行使用手册.md`：WebUI 章节补充历史筛选、复用参数、对比原图与删除/清空说明；API 端点表与数据章节同步新增/删除端点及连带清理规则
- `docs/07-能力增强.md`：历史记录管理（参数复用 / 前后对比 / 搜索筛选 / 清理）标记为已落地（2026.09.18.12）
- `docs/02-架构设计.md`：`task_history` 表结构补充 `params` 列，历史任务列表与存储边界同步筛选、复用、对比与连带清理说明

## [2026.09.18.11] - 0.1.0

### 📈 Improvements 性能/体验优化
- 【代码质量】清理全仓 clippy 编译警告，建立**零警告基线**（`cargo clippy --all-targets` 无任何 warning）：
  - **人脸解码参数聚合**：`decode_retinaface` / `decode_fused_mtcnn` / `decode_mtcnn` 的输入尺寸与 letterbox 逆变换参数聚合为 `DecodeTransform`，消除 `too_many_arguments`
  - **CLI 枚举瘦身**：`Commands::Process` 改为 `Box<ProcessArgs>`，消除 `large_enum_variant`（枚举由 ≥440 字节降至指针级）
  - **MT-CNN 类型解说**：拆分分数/回归返回类型抽取 `Reg4` / `ScoreReg` 别名，消除 `type_complexity`
  - **其余 lint 清理**：`question_mark`、`new_without_default`（`OrtEngine` 补 `Default`）、`let_unit_value`、`manual_pattern_char_comparison`、`useless_format`、`needless_range_loop`、`manual_range_contains`、`print_literal`、`needless_update`、`needless_borrows_for_generic_args`、`doc_lazy_continuation`

## [2026.09.18.10] - 0.1.0

### ✨ New Features 新增功能
- 【输出】新增配置段 `[output]`（`format` = `jpg` | `webp`、`jpg_quality` = 1..=100、`pdf` = 是否额外输出排版 PDF），越界质量在配置校验与请求校验两处均返回中文错误
- 【输出】新增 **WebP 产物**：`format = "webp"` 时证件照 / 效果图 / 排版图片改以 `.webp` 落盘（透明底仍固定 PNG，因其依赖 alpha 通道）。实现走 `image` 的纯 Rust VP8L 编码器（**无损**，体积通常大于同图 JPG），不引入任何 C/C++ 绑定
- 【输出】**JPG 压缩质量可调**：原先产物一律按 `image::save` 的默认编码落盘（质量固定），现改为显式 `JpegEncoder::new_with_quality`；质量越低产物体积越小
- 【输出】**排版结果导出 PDF**：`pdf = true` 时在排版图片之外额外输出 `task_{id}_layout_{相纸}.pdf`。PDF 由 `photos_core::output::layout_pdf` **手写最小单页文档**（不新增第三方依赖）：页面 `/MediaBox` 按相纸物理毫米换算为 pt，整页图像以 `/DCTDecode` 直接内嵌 JPEG（不重采样、不二次压缩），打印店可按原始物理尺寸直接出图
- 【重构】**产物落盘逻辑集中到 `photos_core::output::save_task_outputs`**（原先 CLI 与 API 各写一份命名规约）：CLI `photos process` 与 API 后台任务共用同一函数，命名规约与格式/质量/PDF 逻辑单点维护
- 【接口】CLI 新增 `--format <jpg|webp>`、`--quality <1..100>`、`--pdf`；API `params` 新增 `output_format` / `jpg_quality` / `pdf`（非法格式与越界质量返回 `400 INVALID_PARAMS`）；`GET /config` 新增 `output` 段返回三项默认值
- 【前端】参数面板新增**输出格式下拉**、**JPG 质量滑块**（选 webp 时自动禁用）与**排版 PDF 开关**（未选相纸时禁用），默认值取自 `GET /config` 的 `output` 段

### 🔧 Dependencies 依赖更新
- `image` 启用 `webp` feature（VP8L 无损编码器与 WebP 解码，纯 Rust）

### 📚 Docs 文档更新
- `docs/05-API契约.md`：`params` 新增输出三项字段，`GET /config` 响应补充 `output` 段
- `docs/06-运行使用手册.md`：配置段总览补充 `[output]`，`photos process` 选项表与产物命名表补充格式/质量/PDF
- `docs/07-能力增强.md`：输出格式（多格式 + 质量参数）与排版 PDF 导出标记为已落地（2026.09.18.10）
- `docs/01-PRD产品需求说明书.md` §4.1/§4.2 与 `docs/02-架构设计.md` §3/§4.1：补充多格式输出、排版 PDF 与 `[output]` 配置段
- `docs/examples/application.toml`：新增 `[output]` 段

## [2026.09.18.9] - 0.1.0

### ✨ New Features 新增功能
- 【姿态纠偏】**躯干垂直度校正**（`photos-core/src/vision/geometry.rs` 的 `torso_angle` / `fused_angle_with_torso`）：利用 MoveNet 髋部（缺失自动退回膝部）中点与肩中点连线相对竖直的倾角作为第三路姿态角，与头部/肩线按 **0.5 / 0.3 / 0.2** 融合，修复高低肩之外的侧身倾斜；`KeypointSet` 新增 `hips` / `knees` / `lower_mid` / `shoulder_mid`。半身像下半身关键点缺失时**保持原「头部 0.6 + 肩线 0.4」两路融合**，行为不变
- 【姿态纠偏】**侧脸检测告警**（`yaw_from_landmarks`）：由人脸 5 点关键点（左眼/右眼/鼻尖）估算头部偏转 yaw——鼻尖相对双眼中点的水平偏移 ÷ 双眼半间距（近似 `sin(yaw)`，自归一化，不依赖随偏转同时收缩的脸框宽度）；`|yaw| > 30°` 输出告警「疑似侧脸（估算偏转 xx°），建议提供正面照」，仅告警不阻断出图。关键点退化（如演示/桩数据双眼重合）时判为无法判断，不误告警

### 📚 Docs 文档更新
- `docs/01-PRD产品需求说明书.md` §4.5：补充三路融合权重与侧脸告警规则
- `docs/02-架构设计.md` Step 3：补充躯干垂直度三路融合与侧脸告警
- `docs/06-运行使用手册.md` §9 故障排查：补充侧脸告警与半身像双髋缺失的说明
- `docs/07-能力增强.md`：躯干垂直度校正与侧脸告警标记为已落地（2026.09.18.9）

## [2026.09.18.8] - 0.1.0

### ✨ New Features 新增功能
- 【换装】新增**光影合成**（`photos-core/src/vision/dressing.rs` 的 `shading_factors` / `shade_pixel`）：取原图衣服区域（人像解析 mask）像素亮度，相对区域均值归一化后高斯模糊平滑，再按强度 0.6 加权并钳制到 `0.6..=1.4`，逐像素调制贴合服装亮度后再 alpha 合成——换装结果承袭原图光照方向与衣物褶皱明暗，消除"贴图感"；原图衣服区无有效像素或亮度均值近 0（如黑色上衣）时自动跳过调制
- 光影合成对 `--dress`（单件服装图）、`--dress-style`（程序化正装）、`--dress-top/bottom/shoes`（分部位）三条换装路径一并生效，无需新增参数

### 📚 Docs 文档更新
- `docs/06-运行使用手册.md`：换装章节补充光影合成说明
- `docs/07-能力增强.md`：换装光影合成标记为已落地（2026.09.18.8）

## [2026.09.18.7] - 0.1.0

### ✨ New Features 新增功能
- 【批量】WebUI 新增**批量进度可视化**（`components/BatchPanel.tsx` + store 批量状态机 `batch: BatchItem[]`）：逐项维护「上传中 / 处理中 / 已完成 / 失败」，展示 `已完成/总数`、进度条与按已完成项平均耗时估算的预计剩余时间；多图提交改为逐个上传（`api.submitTask`），上传完成即开始轮询各自状态
- 【批量】**失败重试**：单张上传或处理失败不阻断其余，逐项记录中文失败原因；失败项可点「重试失败项（N）」按本次批量的原始参数（`lastPayload`）重新提交，原文件保留在浏览器内存中；处理失败的项重提为新任务，历史记录保留原失败任务

### 📚 Docs 文档更新
- `docs/06-运行使用手册.md`：桌面 WebUI 章节补充批量进度与失败重试说明，参数设置补充自定义尺寸/底色入口
- `docs/07-能力增强.md`：批量进度可视化与失败重试标记为已落地（2026.09.18.7）

## [2026.09.18.6] - 0.1.0

### ✨ New Features 新增功能
- 【尺寸】支持**一次性自定义尺寸**：`config::Config::resolve_size` 接受 `px:宽x高`（如 `px:295x413`，DPI 取 300 用于排版换算）与 `mm:宽x高@DPI`（如 `mm:35x45@300`，像素 = mm ÷ 25.4 × DPI 四舍五入），无需改 `application.toml`；越界（像素 0/超 10000、DPI 不在 72..2400、宽高非正）返回 `400 INVALID_PARAMS`
- 【底色】支持**一次性自定义 RGB 底色**：`config::Config::resolve_background` 接受 `#RRGGBB` 与 `rgb:R,G,B`（分量 0-255），非法取值返回 `400 INVALID_PARAMS`
- 【命名】自定义规格在受理时归一化为文件名安全 id 后落库与命名：`px:295x413` → `px_295x413`、`mm:35x45@300` → `mm_35x45_300`、`#ff0000` → `rgb-ff0000`（产物如 `task_{id}_px_295x413_rgb-ff0000.jpg`）；归一化 id 可再次提交（解析幂等）
- 【前端】参数面板新增**自定义底色取色器**与**自定义尺寸（毫米宽高 + DPI）**输入，启用后随 `backgrounds`/`size` 一并提交

### 📚 Docs 文档更新
- `docs/05-API契约.md`：`size`/`backgrounds` 字段约束补充自定义形式，任务列表说明补充归一化 id 规则
- `docs/06-运行使用手册.md`：尺寸/底色选项与示例补充自定义写法（CLI 底色以逗号分隔，故自定义色用 `#RRGGBB`）
- `docs/07-能力增强.md`：自定义 RGB 底色 + 自定义尺寸标记为已落地（2026.09.18.6）

## [2026.09.18.5] - 0.1.0

### ✨ New Features 新增功能
- 【模型下载】新增 **`POST /models/download`** 端点：一键下载模型（`ids` 缺省时下载全部「文件缺失」模型，已存在文件跳过视为成功）；单个模型失败**不阻断**其余，逐项返回中文原因；含未注册 id 返回 `400 INVALID_PARAMS`。下载为阻塞 IO，放入 `spawn_blocking` 执行，不占用异步运行时线程（`handlers::download_models`）
- 【前端】参数面板模型缺失提示由「请用 CLI 下载」升级为**「一键下载缺失模型（N）」按钮**：下载中显示等待提示，完成后自动刷新模型就绪状态，失败项以中文原因汇总提示

### 📚 Docs 文档更新
- `docs/05-API契约.md`：端点总表与详细契约新增 `POST /models/download`（请求体、逐项结果、状态码与耗时说明）
- `docs/06-运行使用手册.md`：一键下载章节补充 WebUI/API 下载方式
- `docs/07-能力增强.md`：前端一键下载模型标记为已落地（2026.09.18.5）

## [2026.09.18.4] - 0.1.0

### 🐛 Bug Fixes  问题修复
- 【MTCNN】修复 P-Net 推理输入布局错误：P-Net 由 NCHW `[1,3,H,W]` 改为 NHWC `[1,H,W,3]`（全卷积、空间维动态，与 R/ONet 一致），此前 speed 模式人脸检测无法命中的问题
- 【配置】`mtcnn_pnet/rnet/onet` 的 `input_dims` 元数据由 NCHW 修正为真实 NHWC（`[1,12,12,3]` / `[1,24,24,3]` / `[1,48,48,3]`），`application.toml`、`docs/examples/application.toml` 与内置默认值同步

### 📚 Docs 文档更新
- `docs/04-模型清单.md`：MTCNN 三级的输入 dims 与布局修正为 NHWC

## [2026.09.18.3] - 0.1.0

### ✨ New Features 新增功能
- 【美颜】新增**分区磨皮（五官保护）**：由人脸 5 关键点（左眼、右眼、鼻尖、左嘴角、右嘴角）推导「双眼含眉、鼻、嘴」4 个椭圆保护区（`vision::beauty::face_feature_regions`），生成高斯羽化的保护掩膜（`feature_protect_mask`），磨皮按 `1 - 掩膜` 逐像素加权（`apply_beauty_protected`），双眼/眉、鼻、嘴不再被糊掉；提亮与美白不受保护掩膜影响
- 【美颜】检测结果位于原图坐标系、美颜作用于纠偏后图像，流水线按同一仿射矩阵同步变换五官关键点后再生成掩膜（`pipeline::beauty_protect_mask`），无论是否触发纠偏，五官均被准确避让

### 📚 Docs 文档更新
- `docs/06-运行使用手册.md`：`--beauty` 参数说明补分区磨皮（五官保护）行为
- `docs/07-能力增强.md`：美颜分区磨皮标记为已落地（2026.09.18.3）

## [2026.09.18.2] - 0.1.0

### ✨ New Features 新增功能
- 【性能】新增**进程级推理引擎池** `crates/photos-api/src/engine_pool.rs`：`EnginePool` 按容量（取 `[server] max_concurrent_tasks`）缓存已装载模型的引擎，任务执行时经借用守卫 `EngineLease` 借出、结束自动归还；池空且未达上限时新建，达上限时等待归还（构造不持锁，装载期间不阻塞其他任务）
- 【性能】`EngineFactory` 改为「按输入尺寸借出引擎」：生产工厂 `production_engine_factory(&Config)` 池化后，连续任务与批量处理复用同一批 ONNX 会话（含按需加载的换装解析模型 `parsing_lip`），免去每张图重复装载模型；`engine_factory_from_env` 同步接收配置
- 【演示/测试】演示引擎工厂与测试注入改用 `EngineLease::owned`（一次性引擎，不入池），保持「按输入尺寸回放」语义不变

### 📚 Docs 文档更新
- `docs/02-架构设计.md`：资源限制章节补充推理引擎池说明（容量、常驻模型内存上限、不入池的一次性引擎）
- `docs/07-能力增强.md`：推理引擎复用标记为已落地（2026.09.18.2）

## [2026.09.18.1] - 0.1.0

### ✨ New Features 新增功能
- 【尺寸规格库】内置尺寸由 3 个扩展至 15 个：常规新增 `big_one_inch`（大一寸 33×48）、`small_two_inch`（小二寸 35×45）；签证新增 `us_visa`（美国 2×2 英寸 600×600）、`japan_visa`（日本 45×45）、`schengen_visa`（申根 35×45）、`uk_visa`（英国 35×45）；国内证件新增 `passport`（护照 33×48）、`hkmo_permit`（港澳通行证 33×48）、`driver_license`（驾驶证 22×32）、`social_security_card`（社保卡 26×32）、`residence_permit`（居住证 26×32）；考试报名新增 `exam_registration`（35×45）
- 【配置样例】`application.toml` 与 `docs/examples/application.toml` 同步新增全部尺寸段

### 📚 Docs 文档更新
- `docs/06-运行使用手册.md`：配置章节新增内置尺寸规格表（分类/id/毫米/像素）与自定义尺寸说明
- `docs/07-能力增强.md`：尺寸规格库扩展标记为已落地

## [2026.09.18.0] - 0.1.0

### ✨ New Features 新增功能
- 【抠图质量】新增边缘**去色边**（color decontamination）：对 `alpha` 处于 `26..250` 的半透明边缘按 `F = (C - B×(1-α))/α` 解混，还原被原背景污染的前景颜色，消除换底后的白边/黑边/环境色残留；背景色由「前景掩膜膨胀 2 像素邻域内的透明像素均值」估计（`vision/blend.rs`）
- 【抠图质量】新增**透明底 PNG 输出**：CLI `--transparent`、API `params.transparent`，产物 `task_{id}_{尺寸}_transparent.png`（RGBA，alpha 取羽化后掩膜），可二次合成任意背景
- 【抠图质量】新增**自定义背景图替换**：CLI `--bg-image <背景图>`、API `params.bg_image`，背景图按证件照尺寸 cover 等比铺满并居中裁切后与人像合成，额外出 `background=custombg` 产物；新增算子 `fit_cover` / `composite_with_image` / `to_rgba` 与 RGBA 裁剪 `crop_resize_rgba`
- 【前端】参数面板新增「透明底 PNG」开关与「自定义背景图」路径输入，预览区对 `transparent`/`custombg` 产物显示中文标签

### 🐛 Bug Fixes  问题修复
- 【裁剪缩放】新增 `crop_resize_rgba`，修复透明底路径缺少 RGBA 版裁剪缩放算子的问题

### 📚 Docs 文档更新
- `docs/05-API契约.md`：`params` 补 `transparent`/`bg_image` 字段契约与约束，`id_photo` 产物类型补充 `transparent`/`custombg` 说明
- `docs/06-运行使用手册.md`：CLI 参数表补 `--transparent`/`--bg-image`，产物命名规约与常用示例同步
- 新增 `docs/07-能力增强.md`：能力增强路线与优先级清单

## [2026.09.17.14]

### 🐛 Bug Fixes  问题修复
- 【配置校验】修复 MTCNN 拆分为三级联（`mtcnn_pnet/rnet/onet`）后 `validate()` 仍按单模型查注册表，导致 `face = "mtcnn"`（级联逻辑 id）被误判为未注册、默认配置加载失败的问题；现对级联逻辑 id 校验全部子模型已注册

## [2026.09.17.13]

### 📚 Docs 文档更新
- 新增 `docs/06-运行使用手册.md`：面向使用者的操作手册——环境要求与构建（含 `photos-core/ort` feature 必需性）、模型准备与一键下载、配置项与资源限制、CLI 全参数、桌面 WebUI、HTTP API 调用示例、数据产物路径与故障排查表

## [2026.09.17.12] - 0.1.0

### ✨ New Features 新增功能
- 【资源限制】新增 `[inference]` 段约束 ONNX Runtime 资源：`intra_threads`（单算子内并行线程）、`inter_threads`（算子间并行线程）、`memory_pattern`（内存复用池开关，关闭可降峰值内存）；`0` 表示交由 ORT 自动决定
- 【资源限制】新增 `[general] max_input_side`：输入图最大边长（px，`0` = 不限制），超限时等比预缩放后再进流水线，压降峰值内存与推理耗时；CLI/API 侧 demo 引擎构造同步按缩放后尺寸对齐
- 【资源限制】新增 `[server] max_concurrent_tasks`：API 最大并发处理任务数由配置驱动（原先硬编码 2），超出部分排队；`validate()` 拦截 0 值

### 🐛 Bug Fixes  问题修复
- 【姿态纠偏】修复 MoveNet/COCO 关键点左右语义导致的伪角缺陷：`left_*` 指人物自身左侧，面对面拍摄时位于图像右侧，按传入顺序求角得到 ≈±180°，使任何真实照片都触发「角度超限」降级、自动纠偏实质失效；现按图像 x 递增方向规范化，测量角回归真实倾角

### 📚 Docs 文档更新
- `docs/02-架构设计.md`：配置骨架补 `[inference]`/`[server]` 段，新增「资源限制（CPU / 内存）」说明表
- `docs/examples/application.toml`：补三处资源限制配置项与注释

## [2026.09.17.11]

### ✨ New Features 新增功能
- 【人脸检测】新增 `decode_mtcnn`：支持 P-Net heatmap+bbox 映射解码与融合终态 [boxes,scores,landmarks] 布局；pipeline 按 `suite.face==mtcnn` 分流，speed 模式走 MTCNN 解码（此前误用 RetinaFace）

### 🐛 Bug Fixes  问题修复
- 【推理后端】禁止无 `ort` 时静默 demo：`photos process` 无 `--demo` 且未编译 ort 时直接报错；`photos serve` 默认生产模式（无 ort 启动失败），演示需显式 `--demo`；桌面壳默认要求 ort，可用 `PHOTOS_DEMO=1` 显式演示并告警
- 【API】新增 `production_engine_factory` / `demo_engine_factory` / `engine_factory_from_env`，移除隐式 demo 回退

## [2026.09.17.10] - 0.1.0

### ✨ New Features 新增功能
- 【换装】新增多图分部位贴合：`GarmentSet`（上衣/下装/鞋三部位服装图），`fit_garment_parts` 按部位独立贴合——上衣覆盖 LIP 5/6/7/10、下装覆盖 8/9、鞋覆盖 18/19，未提供部位自动跳过；CLI 新增 `--dress-top`/`--dress-bottom`/`--dress-shoes`，API `params.dress.garments` 透传并落库

### 🐛 Bug Fixes  问题修复
- 【演示引擎】修复人像解析 stub 用「灰度编码 + Triangle 插值」生成类别图导致的类别污染：插值在类别边界产生假类别（如 0↔13 插值出 8/9、5↔8 插值出 6/7/9），污染部位 mask 包围盒使裤区贴合矩形异常偏小；改为在 473×473 画布坐标系直接生成类别（像素反算回原图坐标判定），与真实模型 one-hot logits 行为一致，全仓 136 测试全绿

### 📚 Docs 文档更新
- `docs/03-实施计划.md`：M4 换装升级说明（分部位贴合/参数/验证数据）
- `docs/05-API契约.md`：`params.dress.garments` 字段契约与校验规则

## [2026.09.17.9] - 0.1.0

### ✨ New Features 新增功能
- 【换装】新增全身套装样式 `suit_full_navy`/`suit_full_black`（西装 + 白衬衫 + 西裤 + 黑皮鞋一次覆盖全身）；新增 `full_clothes_mask` 全身服装类集（LIP 8/9/16/17/18/19 裤装/腿/鞋），`fit_garment` 复用于全身套装，上半身单件语义不变
- 【换装】演示引擎人形升级为四段（脸/上衣/裤子/鞋），`--demo --dress-style suit_full_navy` 可演示全身换装效果

### 📈 Improvements 性能/体验优化
-

### 📚 Docs 文档更新
- `docs/03-实施计划.md`：M4 换装升级说明（全身类集/样式/验证数据）
- `docs/05-API契约.md`：`params.dress.style` 枚举补充全身套装样式

### 🔧 Dependencies 依赖更新
-

## [2026.09.17.8]

### 🐛 Bug Fixes  问题修复
- 【模型路径】`resolve_model_path` 真正使用 `general.models_dir`：相对路径剥离注册表历史 `models/` 前缀后与 `models_dir` 拼接，避免 `models/models/…`，自定义 `models_dir` 生效
- 【模型路径】删除误放在 `crates/photos-core/models/` 的 balanced 三件套副本（与根目录 `models/` SHA256 相同，约 327MB）；模型仅保留项目根 `models/`（或 `models_dir` 配置目录）

### 📚 Docs 文档更新
- `docs/04-模型清单.md`：放置路径改为以 `models_dir` 为准，说明剥离前缀规则，禁止 crate 内重复 models 目录

## [2026.09.17.7] - 0.1.0

### ✨ New Features 新增功能
- 【M4 换装】新增 `vision/dressing.rs`：人像解析（LIP 20 类语义分割）+ 服装贴合——`decode_parsing`（argmax + letterbox 逆变换 + 最近邻还原，兼容 NCHW/NHWC）、`clothes_mask`（服装类 5 上衣/6 连衣裙/7 外套/10 连体裤）、`fit_garment`（按衣服包围盒等比缩放居中贴合 + 边缘羽化合成）、`formal_suit`（程序化正装：藏青/黑西装 + 白衬衫 V 领、白衬衫，无外部素材）
- 【M4 换装模型】注册 `parsing_lip` 模型（`models/parsing_lip.onnx`，`[1,3,473,473]`，SCNet LIP 20 类；独立于三模式套件，按需惰性装载，缺失自动下载）
- 【M4 换装链路】`ProcessRequest.dress` 新增 `Option<DressParams>`（enabled + garment 服装图 + style 正装样式）；pipeline 纠偏后、美颜前插入换装，证件照与效果图同步生效；演示引擎 stub 人形解析输出，`--demo` 可直接演示换装
- 【M4 换装 CLI】`--dress <服装图>`（用户服装图）或 `--dress-style <suit_navy|suit_black|shirt_white>`（程序化正装，缺省藏青）
- 【M4 换装 API】`params.dress` 透传（`garment_path`/`style` 至少其一，`garment_path` 优先；`style` 非法或两者皆缺返回 400）；`GET /tasks/{id}` 响应新增 `dress` 字段；任务记录完整保存换装参数
- 【M4 存储迁移】`task_history` 新增 `dress` 列，旧库启动时 `ALTER TABLE` 幂等补列

### 📚 Docs 文档更新
- `docs/04-模型清单.md`：新增 `parsing_lip` 条目（OOTDiffusion/humanparsing，占位 sha256）
- `docs/05-API契约.md`：`dress` 参数约束（样式枚举 + 400 错误码）+ 任务详情 `dress` 字段
- `docs/03-实施计划.md`：M4 换装落地说明（替换「推迟至后续里程碑」表述）
- `docs/examples/application.toml`：新增 `[models.parsing_lip]` 样例

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