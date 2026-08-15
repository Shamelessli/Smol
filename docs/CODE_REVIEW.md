# Smol 代码审阅表

## 审阅信息

| 项目 | 内容 |
|---|---|
| 项目名称 | Smol — 本地媒体压缩器（Tauri 2 + React 19 + Rust） |
| 仓库 | `G:\document\smol` |
| 审阅基线 | `main` 分支，commit `7584b0b`（领先 origin/main 46 个提交） |
| 审阅范围 | `src-tauri/src/**`（Rust 后端）、`src/**`（前端）、构建脚本与配置 |
| 审阅重点 | 近 10 个提交引入的 ADB 设备导入/交付特性，以及既有压缩流水线 |
| 审阅人 | opencode（GLM-5.2） |
| 审阅日期 | 2026-08-16 |
| 是否改动源码 | 否（仅新增本审阅文档） |

---

## 一、总体评级

| 维度 | 评分（5 分制） | 说明 |
|---|---|---|
| 架构分层 | 5 | Rust 处理重脑力 / React 负责状态与视图，边界清晰，命令粒度恰当 |
| 正确性与健壮性 | 4 | “输出大于原始”守卫、`.part` 原子改名、错误 32 重试、磁盘满提示，工程化扎实 |
| 错误处理与用户反馈 | 5 | ADB 区分 未授权/离线/断流，逐文件失败上报，交付失败留存 `recovered` 副本 |
| 安全性 | 3.5 | 全本地处理（隐私优），但 `csp:null` + `assetProtocol:"**"` 偏宽松，见 S-2 |
| 测试覆盖 | 3.5 | 解析类（ls/devices/disk-full/replace-target）单测充分；压缩/ADB 通路无集成测试 |
| 并发与取消 | 4 | `ActiveJobPids` + `cancel_job` + `taskkill`，stderr 后台排空防死锁；批次无整体取消 |
| 代码风格 / 可读性 | 4 | 注释解释“为什么”而非“是什么”，命名一致，Zustand 选择器规约（HR-7）写入文档 |
| 可维护性 / DRY | 3 | 扩展名清单在 4 处重复且已出现不一致，见 M-3；`Other` 的 `dead_code` 注解失真 |
| 跨平台一致 | 3 | 显式面向 Windows（NSIS/taskkill/explorer/反斜杠路径），`#[cfg]` 守卫到位，非缺陷 |
| 构建与依赖 | 4 | `build.rs` 复制 sidecar、`postinstall` 自愈下载、哈希缺失注释诚实 |

**综合：4.0 / 5** — 成熟度高于多数个人项目；主要短板在配置宽松与前端重复。

---

## 二、模块逐项审阅

| 模块 | 路径 | 评价 |
|---|---|---|
| 入口装配 | `src-tauri/src/lib.rs` | 清爽，`HwEncodersState`/`ActiveJobPids` 托管状态位置正确，命令注册完备 |
| ADB 适配 | `src-tauri/src/adb.rs` | 全仓最扎实者：`ls` 日期列歧义消解、单引号转义、`\r` 原位百分比扫描、bundled-adb 优先、stale-server 自愈；8 个单测覆盖关键解析 |
| 错误类型 | `src-tauri/src/error.rs` | `disk_full_hint` 中英双语匹配并单测；`Other` 上挂的 `#[allow(dead_code)]` 已失真（见 L-1） |
| 文件桥 | `src-tauri/src/fs_bridge.rs` | `replace_original` 先回收站后改名、错误 32 重试，安全网到位；`compute_replace_target` 有测试 |
| 探针 | `src-tauri/src/probe.rs` | ffprobe JSON 解析稳健，fps 比率解析正确；**路径解析与 sidecar 命名策略不一致**，见 M-1 |
| 缩略图 | `src-tauri/src/thumbs.rs` | 256px + LRU(500/100) + 亮度阈值跳过黑帧，设计周到；同 M-1 路径问题 |
| 视频压缩 | `commands/compress_video.rs` | `-progress pipe:1` 流式解析、stderr 排空防死锁、PID 注册可取消、`.part` 改名重试 |
| 音频压缩 | `commands/compress_audio.rs` | 与视频同构，扩展名按容器改写；缺少 `targetFileSize` 入参（有意） |
| 图像压缩 | `commands/compress_image.rs` | mozjpeg/oxipng/webp 原生编码，`spawn_blocking` 不阻塞 runtime；`effective_ext = input_ext.clone()` 冗余（见 L-2） |
| PDF 压缩 | `commands/compress_pdf.rs` | GS `pdfwrite` + `/ebook` 等预设，lossless 走 `/prepress` 且显式禁下采样；无进度（GS 限制），结束时补发 100% |
| FFmpeg 参数 | `encoders/ffmpeg_args.rs` | NVENC→QSV→AMF→libx264 回退链清晰；目标码率公式 `mb*8192/dur-128` 与 `estimate.ts` 一致 |
| 硬件探测 | `encoders/hw_detect.rs` | 启动一次、`create_no_window`、二进制缺失静默全 false；合理 |
| Sidecar 路径 | `encoders/mod.rs` | 自建 `ffmpeg_sidecar_path`/`gs_sidecar_path` 处理 dev 三元组后缀——并明确注释了 crate 原生 `ffmpeg_path()` 的缺陷 |
| 构建脚本 | `build.rs` | `OUT_DIR` 三级上溯定位 profile 目录复制 sidecar；`rerun-if-changed` 设置正确 |
| 状态层 | `src/store/jobs.ts`, `settings.ts`, `ui.ts` | Zustand，选择器规约（HR-7）防无限重渲染，聚合选择器循环内求和返回原始值；`persist` 用 localStorage |
| 压缩编排 | `src/hooks/useCompression.ts` | 每作业独立 Channel、`parallelJobs` 限流、设备作业分支交付再清理；类型转换略 hack（见 M-4） |
| 拖放 | `src/hooks/useDragDrop.ts` | Tauri `onDragDropEvent` + 取消竞态，文件夹展开一层（HR-6） |
| 快捷键 | `src/hooks/useShortcuts.ts` | Ctrl+Enter/O、Delete、Esc；**扩展名清单与权威源不一致且漏项**，见 M-3 |
| 设备浏览 | `src/components/device/DeviceBrowser.tsx` | 2s 轮询状态、`cancelled` 防过期响应、dir/file 双模式、按类全选；render 期 setState 用官方 prevOpen 模式 |
| 队列行 | `src/components/filelist/JobRow.tsx` | 失败/编码/探针三态分明，每行可覆盖预设/目标大小/交付模式；紧凑 DoneCard 由 `layout` 平滑过渡 |
| Tauri 桥 | `src/lib/tauri.ts` | 类型与 Rust 结构体一一对应，注释指向对应 `.rs` 文件，便于追踪 |
| 体积估算 | `src/lib/estimate.ts` | 各 preset 比率 + CRF floor + 上限源大小，并显式声明“~”粗略；与 `ffmpeg_args.rs` 码率表对齐 |

---

## 三、问题清单

| ID | 严重度 | 位置 | 描述 | 建议 |
|---|---|---|---|---|
| M-1 | 中 | `probe.rs:29`、`thumbs.rs:189` | 使用 `ffmpeg_sidecar::ffprobe::ffprobe_path()` 与 `ffmpeg_sidecar::paths::ffmpeg_path()`，而 `encoders/mod.rs:17` 自评“该函数返回 `ffmpeg[.exe]`，会错过三元组后缀的 sidecar”。在 `tauri dev` 下 sidecar 名为 `ffmpeg-x86_64-pc-windows-msvc.exe`，crate 函数可能找不到 → `probe_media` 对非 PDF 返回“Could not read media file”，视频缩略图/音频波形静默失败（均优雅降级，但功能不可用）。 | 新增 `ffprobe_sidecar_path()` 复用 `env!("TARGET_TRIPLE")` 逻辑（与 `ffmpeg_sidecar_path` 对称），在 `probe.rs` 与 `thumbs.rs` 改用之；补一个 dev 模式冒烟测试 |
| M-2 | 中 | `tauri.conf.json:27-33` | `"csp": null` 关闭内容安全策略；`assetProtocol.scope: ["**"]` 允许 webview 经 asset 协议读取任意本地路径。本地应用仍可被诱导加载意外资源。 | 显式 CSP（至少限制 `img-src`/`media-src`/`connect-src` 为 `self` 与 `asset:`/`tauri:`）；将 scope 收敛到工作目录 + `Documents\Smol` + 临时目录 |
| M-3 | 中 | `hooks/useShortcuts.ts:10-13` | 扩展名清单重复声明且与权威源 `lib/kinds.ts` 不一致：`VIDEO_EXTS` 漏 `wmv`，`AUDIO_EXTS` 漏 `opus`、`wma`。导致 `Ctrl+O` 文件选择对话框不会显示 `.wmv/.opus/.wma`，而拖放路径却接受这些类型——同一应用内行为不一致。 | 从 `kinds.ts` 导出权威数组并复用；`Dropzone.tsx:22-26` 同份重复一并消除 |
| M-4 | 低 | `hooks/useCompression.ts:101-118` | `compressFn as unknown as CallableFunction` 绕过类型：向 `compressAudio`/`compressPdf` 也传 `targetFileSize`，Rust 端 serde 忽略多余字段故运行时无害，但类型契约被悄悄打破。 | 拆分为按 `kind` 分别调用，或让 `compressAudio`/`compress_pdf` 显式接收并忽略该参数（命令签名对齐） |
| M-5 | 低 | `src/lib/kinds.ts` 注释 vs `src-tauri/src/fs_bridge.rs:23` | 扩展名在 TS（`kinds.ts`、`Dropzone.tsx`、`useShortcuts.ts`）与 Rust（`SUPPORTED_EXT`）共 4 处维护，注释仅提示“keep in sync”。 | 用脚本生成（由单一源 `kinds.json` 产出 `.ts` 与 `.rs` 常量），消除人工同步 |
| L-1 | 低 | `src-tauri/src/error.rs:13` | `#[allow(dead_code)] // Phase 4` 标注在 `Other(String)` 上，但该变体在 `adb.rs`/`compress_*.rs`/`thumbs.rs` 等被大量使用——注解已失真。 | 删除该 `#[allow]` |
| L-2 | 极低 | `commands/compress_image.rs:103` | `let effective_ext = input_ext.clone();` 后从未修改，可直接用 `input_ext` | 删除多余 clone |
| L-3 | 极低 | `package.json:23` + `lib.rs:27` | `tauri-plugin-store` 在 Cargo 与 JS 依赖中声明并 `init()`，但前端 `settings.ts` 用 `localStorage` 持久化，无任何 `@tauri-apps/plugin-store` 导入（全仓 grep 0 命中）。 | 确认是否计划用于跨进程/文件级存储；若否，移除该插件依赖与 `init` 以减小攻击面与产物体积 |
| L-4 | 极低 | `hooks/useShortcuts.ts:59` | `e.ctrlKey && e.key === "o"` 仅匹配小写；CapsLock/Shift 下 `e.key` 为 `"O"` 不触发。 | 改用 `e.key.toLowerCase() === "o"` |
| L-5 | 极低 | `src-tauri/src/adb.rs:344` | 注释/提交信息提到 `ls -aF`，实际执行 `ls -la`。-aF 版本用于显示类型指示符助解析，当前代码靠 `perms` 前缀判 `is_dir` 亦可，二者非冲突但注释与实现不符。 | 统一注释或回归 `-aF` |
| L-6 | 极低 | `commands/compress_image.rs:193-194` | 异步函数内有一段 “wait... wait, this is just async fn” 的自我对话注释残留 | 清理 |

---

## 四、优点

- **全本地处理**是核心卖点，隐私层面可信；代码未引入任何云端调用。
- **质量守卫**贯穿压缩路径：输出 ≥ 输入即判“Already optimal”并保留原件；`.part` 临时文件 + 错误 32 重试 + 改名前先 `remove`；删除原件走 Recycle Bin（`trash` crate）而非硬删。
- **ADB 通路工程化深度**高：bundled-adb 优先（避免多版本 adb server 抢占导致设备可见性抖动）、stale-server 自愈、`unauthorized` 分别提示、逐文件批拉取失败不上送整批错误、交付失败留存 `recovered` 副本并在错误信息中给出路径。
- **进度流式**统一用 Tauri `Channel`，每作业独立通道、`jobId` 精确路由；ADB 端则原位扫描 `\r` 重绘的 `[ NN%]` 字节流，而非逐行，避免漏报。
- **前端性能规约**（HR-7 等）写入注释并严格践行：选择器返回原始值或既有引用，规避了 Zustand 常见无限重渲染。
- **注释讲“为什么”**：`.part` 扩展名位置、ls 日期列歧义、`ffmpeg_sidecar` 路径缺陷、render 期 setState 用 prevOpen 模式——均给出了问题与对策的因果，显著降低维护门槛。

---

## 五、结论与建议

整体是一座**完成度高、工程化扎实**的本地压缩器。近期 ADB 特性在解析、错误、自愈、进度上的处理超出个人项目平均水平，审阅人无阻塞性异议。

建议按以下顺序处理（均与本次审阅一致，不涉及源码改动由本审阅人承担）：

1. **M-1（中）**：补 `ffprobe_sidecar_path()`，消除 dev 模式探针/缩略图静默失效。
2. **M-3（中）**：以 `kinds.ts` 为唯一源复用扩展名清单，修复 `Ctrl+O` 漏显 `.wmv/.opus/.wma`。
3. **M-2（中）**：收敛 CSP 与 asset scope。
4. **M-5 / L 系**：清理重复与失真注解、移除疑似未用的 store 插件。

> 本审阅仅基于静态阅读，未运行 `pnpm tauri dev`/测试或接入真实 Android 设备；M-1 的实际触发情况建议在 dev 环境以一例非 PDF 媒体验证。