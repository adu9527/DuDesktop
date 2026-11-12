# 分阶段开发计划

## 目标与执行规则

目标：长期可用的壁纸软件，AMD/NVIDIA/Intel 按能力兼容，低开销与高表现力均可由用户选择。

按验收推进，不用未经验证的工期代替可行性。每阶段更新本文件状态，提交可运行结果、验证记录和遗留问题；重要后端决策写简短 ADR。未完成内容不得写成已支持。

## S0：环境与框架（已完成）

- 固定 Rust/MSVC 工具链与依赖锁文件，提供构建检查脚本。
- 五个 crate：主程序、宿主、协议、Windows 集成、渲染边界。
- CLI、原生基础窗口、宿主开发握手、事件驱动帧需求模型。
- 完成编译、格式、Clippy、测试、握手及窗口生命周期自检。

本阶段不等于 M0 图形原型完成。S0 完成时未包含 GPU 探测、透明渲染、长期 IPC 与播放器；这些在 M0/M1 逐步补齐。

## M0：透明合成与调度地基（核心验收已通过，2026-11-11）

结论：原生 DirectComposition 路径满足透明合成、命中区域、形变、阴影与空闲停帧要求，M0 验收通过，可进入 M1。wgpu/egui 对比经 ADR 0001 调整出 M0 阻塞范围，转为引入复杂 UI 时再验证。

已完成：

- `deskshell probe`：DXGI 适配器/输出枚举，输出厂商、LUID、显存、分辨率、刷新率、DPI、旋转；进程启动统一 Per-Monitor V2 DPI 感知，修正探测时报告虚拟化分辨率的问题。
- `deskshell capsule`：D3D11 + DXGI composition swap chain + DirectComposition 预乘 alpha + Direct2D/DirectWrite；`WS_EX_NOREDIRECTIONBITMAP`，无黑底/闪烁；窗口区域按胶囊圆角裁剪。
- 胶囊 hover 形变：宽度在 360→408 DIP 间插值，窗口保持最大尺寸、区域同步收缩。
- 阴影层：共享 D3D11/DComp 设备的独立顶层窗口（`WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`），绘制柔和外阴影；截图确认无黑底，`WindowFromPoint` 与真实点击均确认不拦截。
- 命中与焦点：`WindowFromPoint` 五项（中心、窗口外、圆角外、未展开留白、阴影层）PASS；`--input-test` 真实点击阴影/留白不激活本程序，点击胶囊后前台窗口与键盘焦点均为胶囊。
- 事件驱动：仅动画期间使用 16 ms 定时器；稳定后停止定时器与双层 Present，600 ms 空闲窗口 Present 增量 0，PASS。
- DPI 与显示器：胶囊尺寸/区域在 96/120/144 DPI 下单元测试通过；`WM_DPICHANGED` 重建路径已接入；`--monitor N` 支持指定显示器，越界给出明确错误。
- 设备丢失（`DEVICE_REMOVED`/`DEVICE_RESET`）重建共享 GPU 上下文与双层交换链的路径已接入，尚未故障注入。
- 实测环境：Windows 11 build 26200，AMD Radeon 780M 驱动 32.0.12033.1030，单屏 2560×1440@144、125% DPI；双层动画 CPU 平均 debug 0.73–0.81 ms / release 0.81–0.83 ms，峰值 debug 0.95–1.44 ms / release 1.23–1.44 ms（小样本）。

M0 验收对照：

| 验收项 | 结果 | 证据 |
|---|---|---|
| 无黑底/闪烁 | PASS | 预乘 alpha 截图，双层内容与桌面正确叠加 |
| 跨进程穿透 | PASS | 窗口区域 + 阴影层 `WS_EX_TRANSPARENT`；`WindowFromPoint` 与真实点击验证 |
| 空闲停帧 | PASS | 稳定后 600 ms 内 Present 增量 0 |
| 后端选择与性能基线 | PASS | ADR 0001；release 双层动画约 0.8 ms CPU，空闲 0 Present |

范围调整与残留（不阻塞 M1）：

2026-11-11 代码审查加固：

- 窗口回调只记录事件；状态由 `run` 持有，在窗口全部销毁后释放，避免消息循环、同步尺寸消息及销毁回调中的可变引用重入。
- 图形错误保留设备失效/目标失效分类，覆盖 Direct2D `EndDraw`、表面创建与 `ResizeBuffers`；目标失效重建表面，设备移除重建双层设备资源，每次渲染事件仅重试一次。
- 自检失败记录为错误；计时退出、Escape 或关闭导致自检未完成时返回失败退出码。鼠标测试提前退出也恢复已保存的位置。
- `capsule-check` 加入合成错误恢复注入及提前退出反向回归。普通自检、恢复注入和串行真实鼠标点击均通过；恢复注入后仍为空闲 0 Present。
- 新增错误分类、自检未完成和首个失败保留的行为测试。`scripts/dev.ps1 check` 通过；本次 debug 绘制/提交墙钟平均约 0.86–0.97 ms、峰值 1.40–1.66 ms（小样本，非 CPU 执行时间/GPU 帧时间）。
- 两个同位置胶囊测试并行时，中心命中检查因相互遮挡而失败，并正确返回失败退出码；图形/鼠标验收应串行运行。
- 合成错误注入验证资源重建流程，不等同于真实驱动重置；真实设备移除、跨屏 DPI 与长期资源增长验证仍待执行。

- wgpu/egui：桌面壳层采用已验证的原生 DComp；egui 在 M3 引入设置/搜索等复杂 UI 时再做集成与 IME 验证；若 M5 需要复杂 GPU 效果，再发起独立 wgpu 互操作原型。
- 真实中文 IME 组合输入：当前仅验证中文文本渲染与键盘焦点，不在无输入控件的胶囊上伪造验收。
- 100%/150% DPI 与多显示器：布局单元测试与代码路径就绪；本机仅单屏 125%，物理矩阵待第二台机器/多屏环境。
- 设备丢失、真实 DPI 切换与 Explorer 重启等故障注入：在对应阶段执行。

原始实施顺序（保留供追溯）：
1. DXGI 枚举适配器/显示器，输出厂商、LUID、DPI、刷新率与可用能力。
2. 固定 wgpu/egui 候选版本，做最小 GPU 窗口；记录设备创建、丢失和重建路径。
3. 验证 DirectComposition 透明路径；必要时比较隔离原生适配的成本，形成后端 ADR。
4. 做胶囊形变和圆角阴影，验证可视区域/命中区域及跨进程穿透。
5. 接入事件唤醒/动画期限；输入停止后无周期提交，记录 CPU/GPU 帧时间。
6. 验证中文输入、键盘焦点、100%/150% DPI、多屏不同刷新率。

## M1：壁纸宿主与视频（核心验收已通过，2026-11-11）

结论：命名管道 IPC、事件驱动宿主监督、固定 libmpv 窗口嵌入播放、静态图与 4K 视频生命周期、错误/超时/断开处理均已实现并通过自动化验收。M1 使用独立预览窗口，WorkerW 桌面附着、图标隐藏与完整 Shell 恢复按 M2 推进。真实驱动重置、多显示器/多厂商矩阵、长期资源增长仍未验证。

已完成：

- IPC：主进程命名管道服务端 + 宿主客户端；版本化信封（协议版本、请求 ID、宿主代次、配置版本）、256 KiB 帧上限、超时、取消、断开处理；DACL 仅当前用户与 SYSTEM，拒绝远程客户端。命令为完整期望状态，重连自动重放。
- 监督：子进程退出/管道断开事件驱动；500 ms/1 s/2 s/4 s 指数退避，最多 4 次；稳定 30 秒后清零；超过上限进入失败状态，无重启风暴；Job 对象 `KILL_ON_JOB_CLOSE` 与父进程句柄监视兜底；正常退出发送 `shutdown`。
- 宿主：主线程拥有预览窗口与每实例 mpv 上下文；管道读取、mpv 事件、父进程监视线程通过自动重置事件唤醒主循环；空闲无限等待。视频等待首帧时使用 250 ms 有界看门狗。
- 播放：mpv `v0.42.0-36-gd075ec2a2`、client API 2.6；`wid` 窗口嵌入；`vo=gpu` / D3D11 context；`hwdec=d3d11va` + `hwdec-codecs=all` + `hwdec-software-fallback=no`；`audio=no` / `ao=null`；静态图与视频同路径。
- 循环与错误：不使用 `loop-file=inf`（可能隐藏解码失败），宿主在 EOF 后重新加载；5 秒内无有效画面或 `hwdec-current` 非硬解时明确报错；配置海报时进入 `Fallback`，不静默软件解码。
- 验证入口：`scripts/fetch-mpv.ps1`、`scripts/fetch-test-media.ps1`、`scripts/dev.ps1 host-check`；故障注入覆盖非法协议、超限消息、无响应与断开。

实测环境与结果（Windows 11 build 26200，AMD Radeon 780M 驱动 32.0.12033.1030，单屏 2560×1440@144、125% DPI）：

- 4K 样本：H.264 High，3840×2160，30.00 fps，`yuv420p` 8 bit，27.55 Mbps；实际硬解 `d3d11va`；5 秒播放掉帧 0。
- 静态图片 CPU 0.00% 单核；暂停 2 秒 CPU 0.00% 单核、掉帧增量 0；停止释放后 2 秒 CPU 0.00% 单核。
- 杀死宿主后主进程命令 0 ms 返回 `NotReady`，监督器以新代次恢复；未重新下发命令即按期望状态恢复播放。
- 双实例同时播放，暂停/停止一个实例不影响另一个。
- 非法协议/断开只注入首代次时第二代次恢复；超限消息与无响应宿主有限失败；主进程正常退出与强杀后均无孤儿宿主。

M1 验收对照：

| 验收项 | 结果 | 证据 |
|---|---|---|
| 杀死宿主不影响主程序，有限退避恢复 | PASS | host-check：0 ms NotReady，新 PID/代次恢复，重放期望状态 |
| 主进程退出后无孤儿宿主 | PASS | 正常退出与强杀两种子进程测试 |
| 无效协议/过大消息/超时/断开 | PASS | 故障注入四类；协议/管道单元测试 |
| 4K 编码/位深/帧率/硬解/掉帧记录 | PASS | H.264 High 8-bit 30 fps，d3d11va，掉帧 0 |
| 错误文件与不支持编码有明确反馈且可恢复 | PASS | SourceNotFound / UnsupportedMedia / HardwareDecodeUnavailable，随后正常素材可播放 |
| 暂停/停止资源生命周期 | PASS | 暂停 `player_active=true` 且 0% CPU、停止 `player_active=false` 且状态清空、0% CPU（小样本） |
| 多实例独立控制 | PASS | 同屏双预览实例暂停/停止互不影响 |

2026-11-11 M1 审查加固（协议版本 3）：

- 管道：重叠 I/O 在超时/等待异常后必须取消并收割完成；无法证明内核已结束访问时泄漏 `OVERLAPPED`/缓冲区而不是悬垂释放。句柄与事件改为 RAII；读锁覆盖整帧，强制单读取者；部分帧超时或协议错误后连接标记失效，后续调用快速失败；写锁等待计入总截止时间；零字节写入不再忙等。新增空闲超时可复用、部分帧失效、并发读取不拆帧、写超时不卡死、30 次连接/释放句柄增长受限等测试。
- 实例：明确“暂停保留 mpv 上下文，停止释放播放器/解码/VO 资源”，并以 `InstanceStatus::player_active` 提供可观测证据；切换素材先释放旧播放器再建新播放器，旧素材迟到事件不会污染新状态；图片、视频、海报回退都有 5 秒绝对加载期限；海报失败使用独立标志，不再被主素材 `error_handled` 屏蔽；视频循环重载恢复加载期限；同一配置版本在部分实例失败后允许重试，同版本不同内容明确拒绝。
- 宿主：看门狗按绝对期限在每次唤醒后检查，不被 IPC/mpv 事件饿死；命令队列有界（64 条）并在溢出时明确断开；`PlayerChanged` 按实例合并；响应与快照经有界队列由独立写线程发送，主线程不因对端不读而阻塞；读取/写入/父进程监视线程创建失败均取消 I/O 并回收线程。
- 监督器：期望配置版本与实例列表原子更新，本地先校验素材存在再修改期望状态；宿主明确拒绝（InvalidRequest/SourceNotFound）时回滚到最近成功配置，瞬时失败保留期望状态供重连重放；连接建立后继续收敛期间新增的配置版本；旧响应/旧事件不得覆盖更新的 `applied_config_version`；新增 `set_instances` 后台路径与 `wait_applied` 事件驱动等待（Condvar，不轮询）。
- 验收结果（本机，Windows 11 build 26200，AMD Radeon 780M）：`scripts/dev.ps1 check` 通过；`host-check` 全部 PASS（含快速切源、停止资源释放、暂停保留上下文）；`capsule-check` 通过（空闲 600 ms Present 增量 0、恢复注入 PASS）。

遗留与未验证：

- 真实驱动设备移除/重置未做故障注入；M0 的合成错误注入不等同于驱动重置。
- 物理多显示器、100%/150% DPI 与多厂商（NVIDIA/Intel/混合显卡）矩阵未验证；本机仅单屏 125%。
- 仅测试 8-bit H.264 与不支持的 MPEG-4 Part 2；HEVC/10-bit/AV1/HDR 未测试。
- 24/72 小时资源增长与休眠恢复未验证。
- “无音频泄漏”由 `audio=no`/`ao=null` 结构性保证，未通过音频端点测量。
- 停止释放以 `player_active=false` 与该实例 CPU 采样为证据，未直接测量显存/句柄归零（宿主进程整体句柄趋势在 M6 长期项记录）。
- GPL libmpv 构建正式分发前需完成许可证合规与源码/许可提供方案。
- 桌面附着、图标恢复、Show Desktop/虚拟桌面属于 M2，见下一节。

## M2：桌面附着与恢复（核心实现完成，验收部分受阻，2026-11-11）

结论：WorkerW 兼容适配、显示器完整边界、稳定显示器身份、附着失效检测与重新附着、静态回退状态、图标状态查询/幂等恢复与 Shell 状态日志已实现并通过本机自动化自检。Explorer 真实重启、锁屏/解锁、休眠恢复、热插拔/DPI 变化与多屏矩阵尚未在本机实际触发，仍属未验证。

已完成：

- `shell-win/src/desktop.rs`：`Progman 0x052C` 与 WorkerW 层级隔离在单一模块；`AttachError` 区分 Explorer 不可用、消息派发失败、SetParent 失败、父子校验失败与显示器不可用。附着后校验 `GetParent`，并通过 `WS_CHILD` 样式保证 `GetParent` 语义。
- 显示器：使用 `MONITORINFOEXW.rcMonitor` 完整边界而非工作区；设备名（如 `\\.\DISPLAY2`）作为稳定身份，索引仅作回退；负坐标通过父窗口屏幕原点换算；`WM_DISPLAYCHANGE`/`WM_DPICHANGED` 触发重新解析与重附着。
- Explorer 重建：父窗口被销毁会连带销毁子窗口，宿主检测到窗口失效后重建窗口与播放器（旧 `wid` 不可复用），再重新附着并恢复素材；附着期间 2 秒低频恢复看门狗兜底，系统事件（会话变化、电源广播）即时触发复检。
- 静态回退：附着失败时窗口保持隐藏、释放不可见解码资源，系统静态壁纸继续显示；`AttachmentState::Fallback { reason }` 上报可重试原因，成功重附着后自动恢复播放。
- 图标状态：查询 `SHELLDLL_DefView` 下 `SysListView32` 实际可见性；`0x7402` 为切换命令，设置前必须查询，状态一致时返回“未发送命令”。本阶段不默认隐藏图标；`desktop-check --restore-icons` 仅用于显式恢复可见。
- Shell 状态日志：`ShellStateJournal` 先落盘后修改，记录键、原值、应用值、会话所有者与恢复状态；同键重复记录保留首次原值；原子写入；损坏日志改名保留后从空日志继续。
- `desktop --desktop` 实际将静态图附着到桌面壁纸层播放并正常退出；退出时解除附着并销毁窗口，不修改系统壁纸配置。

M2 验收对照：

| 验收项 | 结果 | 证据 |
|---|---|---|
| WorkerW 探测/父子校验/附着 | PASS | `desktop-check`：宿主探测、附着、父子关系校验、完整边界 2560×1440 |
| 附着失效检测与重新附着 | PASS（模拟） | `desktop-check` 解除父窗口后检测失效并重新附着；非真实 Explorer 重启 |
| 显示器完整边界（非工作区） | PASS | 窗口屏幕矩形等于 `rcMonitor`；本机 2560×1440 |
| 稳定显示器身份 | PASS | 设备名与索引解析一致；索引仅回退 |
| 系统静态壁纸回退 | PASS（代码路径） | 附着失败时隐藏窗口并释放解码资源；未在真实 Explorer 缺失下强制复现 |
| 图标状态查询与幂等恢复 | PASS | 两次查询一致；设置当前值不发命令；`--restore-icons` 日志先于修改 |
| 正常退出不破坏桌面 | PASS | `wallpaper --desktop` 退出后窗口销毁、无残留实例 |
| Explorer 真实重启恢复 | 未验证 | 需手工结束/重启 explorer.exe，观察重附着与状态日志 |
| 锁屏/解锁、休眠恢复 | 未验证 | 代码已接入 `WM_WTSSESSION_CHANGE` 与电源广播；需真实会话操作 |
| 热插拔、分辨率/DPI 变化 | 未验证 | 代码已接入显示变化事件；本机单屏 125% |
| Show Desktop、任务视图、虚拟桌面 | 未验证 | 需实际交互确认 |
| 多屏/100%/150% DPI 矩阵 | 未验证 | 本机仅单屏 2560×1440@125% |
| 宿主崩溃/主进程崩溃后桌面可用 | 部分验证 | 进程退出即销毁自有窗口；未做截图对比确认 |

手工验收步骤（待执行，执行后更新本表）：

1. Explorer 重启：`deskshell wallpaper --file testdata\4k-pattern.png --desktop --seconds 60`，运行中在任务管理器重启 Windows 资源管理器；预期 2 秒内重新附着并恢复播放，日志记录新宿主窗口；结束后系统壁纸恢复。
2. 锁屏/解锁：运行同一命令并 Win+L，解锁后确认仍覆盖完整屏幕且无黑屏。
3. 休眠恢复：运行中休眠并唤醒，确认显示恢复。
4. 分辨率/DPI：运行中修改显示比例或分辨率，确认窗口按新边界重排。
5. Show Desktop/任务视图/虚拟桌面：确认壁纸保持在图标层下、不被其他应用永久覆盖或穿透异常。

遗留与未验证：

- 本机仅 AMD Radeon 780M、单屏 125% DPI；NVIDIA/Intel/混合显卡与多屏未覆盖。
- `0x052C` 与 WorkerW 层级属于非公开约定；Windows 大版本升级前需复测。
- 图标隐藏的首次引导与用户选择流程属于 M3；本阶段只提供查询、幂等设置与恢复入口。
- Dock/岛体候选窗口层级验证随 M3 实现进行。
- 恢复日志在“Job 立即终止宿主”场景下由主进程下次启动读取；该路径将在 M3/M4 与真实图标修改一起验证。

## M3：Dock 与文件灵动岛（核心实现与自动化验收通过，交互/环境项未完全验证，2026-11-11）

结论：主程序入口、应用发现/启动、Dock、应用库、桌面文件索引/监听/图标、灵动岛基础操作与首次引导代码已完成。`check`、`app-check --launch --probes`、`app --self-test` 通过；真实鼠标点击应用库、热键临时置顶与 Escape 撤销已实测。中文 IME、首次隐藏图标引导、边缘唤起、OLE 拖拽与多屏/DPI 矩阵尚未完成或未验证，不把本阶段写成全面完成。

已完成：

- 主程序 `deskshell app`：单实例互斥、隐藏主窗口、托盘图标、全局热键（Ctrl+Alt+D Dock、Ctrl+Alt+I 灵动岛）、宿主监督协同、退出清理；GUI 模式隐藏控制台窗口，CLI 仍保留控制台输出。
- 应用发现 `shell-win/src/apps.rs`：`shell:AppsFolder` 为主（305 项），合并开始菜单 `.lnk`（33 项）并按名称去重；身份使用 AppsFolder 父相对解析名 / `.lnk` 规范路径 / 手动添加路径；42 个 AUMID 条目；名称过滤卸载项；图标经 `IShellItemImageFactory` 提取并按 48px 预取（样张 5/5、预取 12 个）。
- 启动：AUMID 优先 `IApplicationActivationManager`，失败回退 Shell 默认动作；传统应用与 `.lnk` 走 `ShellExecuteEx`。实测 Notepad（Windows 11 商店转发进程）与计算器 UWP 均启动成功，并在确认是本次新建进程后终止。
- Dock：固定项与应用库分离；固定项可增量固定/移除/左移/右移（右键菜单），上限 9 个；应用库为顶部独立面板，显示前 8 项；固定项为空时仍保留应用库、资源管理器、回收站三个入口。
- 交互：鱼眼悬停动画可关闭；静态 700 ms Present 增量 0；圆角窗口区域裁剪、`WindowFromPoint` 命中与透明区域穿透自检；左/右/上/下/Enter/Escape/Space 键盘路径；唤起时临时 TOPMOST，失焦或 Escape 撤销并尝试恢复前台窗口，不永久置顶。
- 文件索引 `shell-win/src/deskfiles.rs`：用户桌面（含重定向）+ 公共桌面，按完整路径去重身份；跳过隐藏/系统文件与 `desktop.ini`；分类仅影响展示；OneDrive 占位属性识别且不主动请求缩略图；权限错误逐条上报。
- 监听 `shell-win/src/watcher.rs`：每个根目录重叠 `ReadDirectoryChangesW`、150 ms 去抖合并、溢出标记、停止时 `CancelIoEx` + 收割、失败后 2 秒快照回扫；监听只作提示，扫描快照保证最终一致。
- 文件图标/缩略图：仅加载可见前 8 行，单请求 1.5 秒超时并隔离；超时线程达到 2 个后停止继续请求并提示降级；缓存仅保留当前可见行。
- 灵动岛：折叠摘要（项数/分类数）、展开 8 行文件、分类色块/缩略图、右侧分类与大小、底部“打开/定位/复制路径/移入回收站”按钮；删除前确认，失败保留条目并在状态行反馈；回收站虚拟入口。
- 配置：`%APPDATA%\DeskShell\config.toml` 版本化 TOML，原子保存，损坏文件改名保留，新版本只读不覆盖；`--config` 支持验收与便携测试。
- 首次引导：文件入口就绪后询问是否隐藏桌面图标；修改前写 `ShellStateJournal`，托盘始终提供“恢复桌面图标”；本阶段未自动执行该引导。

M3 验收对照：

| 验收项 | 结果 | 证据 |
|---|---|---|
| 应用身份唯一、传统/UWP 启动 | PASS | 338 项身份无重复；Notepad 与计算器实际启动并关闭 |
| 固定项可持久保留/排序/移除 | 代码完成，菜单未人工点选 | 右键菜单 + 配置原子保存；未做真实点击回归 |
| 公共/重定向桌面可访问 | PASS（公共+用户） | 62 项：用户 35、公共 27；重定向/OneDrive 环境未覆盖 |
| 监听与快照校正 | PASS（基础）/未故障注入 | 新建文件通知 PASS；溢出/监听失败回扫代码路径未注入 |
| 文件分类不移动文件 | PASS | 分类仅用于展示，无移动/重命名调用 |
| 打开/定位/复制路径/回收站 | PASS（复制+回收）、打开/定位部分 | 剪贴板路径可读回、探针移入回收站；真实桌面写入一次通过，复测被安全策略拒绝后以临时根目录等价验证 |
| 中文、键盘、鼠标、穿透、焦点 | 部分 PASS | 中文渲染与截图 PASS；真实鼠标点击应用库 PASS；键盘导航/焦点恢复为代码与自检；IME 未验证 |
| 主界面不被扫描/缩略图/IPC 卡住 | PASS（结构+自检） | 发现/索引/图标/宿主均为后台线程 + PostMessage；UI 自检在加载完成后完成；未做帧级延迟测量 |
| 静态/关闭动画后无持续渲染 | PASS | 700 ms Present 增量 0 |
| 首次引导隐藏图标 | 代码完成，未执行 | 避免改变当前用户桌面；手工步骤待执行 |
| OLE 拖拽 | 未实现（明确延期） | 计划在 M4/M5 随文件面板完善，不提供假拖拽入口 |

实测环境：Windows 11 build 26200，AMD Radeon 780M 驱动 32.0.12033.1030，单屏 `\\.\DISPLAY2` 2560×1440@125%。可视化证据：`artifacts/m3-app-dock2.png`（Dock）、`artifacts/m3-island-expanded.png`（展开岛）、`artifacts/m3-library4.png`（应用库）。

2026-11-11 实测记录：

- `scripts/dev.ps1 check` 通过（fmt、Clippy `-D warnings`、workspace 单元测试）。
- `scripts/dev.ps1 app-check`：应用 338、图标 48px 5/5、桌面 62、监听 PASS、Notepad/计算器 PASS、复制路径 PASS、回收站 PASS（真实桌面被安全策略拒绝时回退临时根目录，输出明确警告）。
- `scripts/dev.ps1 app-self-test`：应用 338、Dock 3/6 项命中、岛体 8 行、文件图标 8、圆角裁剪、2560×1440 边界、700 ms Present 0，PASS。
- 热键唤起：Dock 扩展样式从 `0x200020` 变为 `0x200028`（TOPMOST），Escape 后恢复。
- 单实例：第二实例 1025 ms 内以 0 退出，第一实例继续运行。
- 真实鼠标点击 Dock 第 4 项打开应用库，日志记录 `dip=(200.8,32.0) dpi=120 hit=Item(3)`。

未验证/阻塞：

- 真实中文 IME 组合输入与候选框：当前没有文本输入控件；中文渲染与键盘路径已验证。
- 首次隐藏桌面图标引导：未对当前用户桌面执行；需按手工步骤验证“先落盘、用户选择、托盘恢复”。
- 边缘唤起：本阶段只实现全局热键；边缘检测仍未实现，继续延期（不伪造入口）。
- OLE 拖拽：未实现，按范围延期，不提供假入口。
- OneDrive/重定向桌面、权限拒绝根目录、监听溢出/失败注入：本机环境未覆盖。
- 100%/150% DPI、多屏、混合 DPI、NVIDIA/Intel/混合显卡：未覆盖。
- 慢 Shell 扩展导致超时：实现了超时隔离与降级上限，但未构造真实挂起扩展。
- 托盘菜单、固定/取消固定/排序菜单：代码完成，未做真实菜单点选回归。
- 无障碍（UIA）与真实键盘焦点恢复：未验证。
- 空闲整机 CPU、长期句柄/内存增长：M4/M6 测量。

手工验收步骤（待执行，执行后更新本表）：

1. 右键应用库条目，选择“固定到 Dock”，重启主程序确认固定项保留；再次右键选择“左移/右移/从 Dock 移除”。
2. 托盘菜单逐项点击：显示/隐藏 Dock、显示灵动岛、应用库、暂停/恢复壁纸、恢复桌面、恢复桌面图标、退出。
3. 首次引导：删除或重命名测试配置中的 `onboarding.desktop_icons_asked=false`，启动后确认询问；选择“是”后确认桌面图标隐藏、`shell-state.json` 先落盘，再从托盘恢复。
4. 中文 IME：在后续加入搜索/重命名输入控件后，验证组合输入、候选框位置与 Escape 取消。
5. 慢 Shell 扩展：用可控的缩略图处理器或注入挂起接口验证 1.5 秒超时与两次后降级。

## M4：用户性能控制与试用版（核心验收通过，交互项待回归，2026-11-11）

结论：配置 v2、预设/自定义、逐屏覆盖、自动规则迟滞、Eco 静态呈现、托盘设置、面板阴影、开机启动、安装/卸载与试用包已实现。`check`、`host-check`（含 Eco 静态模式）、`app-self-test`（含阴影创建/穿透/释放）、release 桌面附着 5 分钟正式测量（整机归一 **0.0078%**）与隔离安装/升级/卸载验收均通过。真实托盘菜单逐项点选、真实全屏/电池/RDP 规则链路、多屏/多厂商/DPI 与背景模糊仍未验收，因此不把 M4 写成全部完成；这些项目已如实列出。

已完成：

- 配置 v2（`crates/deskshell/src/config.rs`）：`%APPDATA%\DeskShell\config.toml`；写入前把旧文件复制为 `.prev`，主文件损坏时优先从 `.prev` 恢复并把损坏文件改名保留；v1 → v2 迁移前写 `config.v1-<时间>.bak`；保存时递归合并未知字段/表，不丢弃旧版本或未来版本字段；高于程序版本只读；原子临时文件 + `fsync` + rename。
- 画质模型（`crates/deskshell/src/policy.rs`，纯逻辑）：`QualityProfile` 保存用户意图（Eco/Balanced/Ultra/Custom、自定义效果、自动规则、逐显示器覆盖）；`EffectivePolicy` 是某一显示器、某一时刻的实际生效值，含播放意图、UI 动画、阴影、模糊标记、降档原因与“等待迟滞/冷却”说明。自动规则只降档，不写回用户意图。
- 自动规则：全屏、遮挡、电池/节能、远程会话、指定前台进程；只作用于壁纸所属显示器。进入迟滞 1.5 秒、恢复迟滞 30 秒、冷却 10 秒，确定性单元测试覆盖降档/恢复/冷却/优先级/逐屏隔离；所有规则可单独关闭，关闭后撤销采样定时器。
- 平台信号（`crates/shell-win/src/system_state.rs`）：前台窗口矩形与显示器 `rcMonitor`/`rcWork` 比较（2 px 容差）、DWM cloaked、最小化、Shell 类名排除、进程名、电源状态与远程会话；不读取内容，不做窗口截图。
- 协议 v4：`RenderOptions.playback` 区分 `animate` / `static`；宿主新增 `PlaybackState::Static`、`InstanceStatus.render` 与 `video_position_ms`。Eco/自动降档时视频停在当前帧，`player_active` 保持 true，播放位置不推进；用户暂停仍是 `paused=true` 且优先于自动策略。
- 效果开关：UI 动画（Dock 鱼眼/悬停）、面板阴影、壁纸播放。阴影是独立分层窗口（`render/src/panel/shadow.rs`），关闭时销毁窗口与 DIB；自检验证创建、点击穿透与关闭后释放。模糊仍未交付，托盘菜单以置灰说明“需 M5 渲染管线”，不提供假开关。
- 托盘设置：暂停/恢复、重新应用、画质子菜单、效果开关、自动规则、逐显示器覆盖、素材选择（图片/视频/海报）、状态与故障、开机启动、恢复桌面、恢复图标、退出。菜单支持子菜单与置灰原因项；实际生效策略、宿主状态、附着、硬解、掉帧与错误在状态框显示。
- 素材与显示器：文件对话框选择图片/视频；切换素材清除旧海报；逐屏覆盖按稳定显示器身份（`\\.\DISPLAYx`）保存，不按枚举索引持久化。
- 开机启动（`shell-win/src/autostart.rs`）：HKCU Run 用户级值，托盘可开关，不修改系统级启动项；`deskshell autostart --status|--enable|--disable` 提供可脚本化验收入口。
- 安装试用版（`scripts/install.ps1` / `scripts/uninstall.ps1`）：默认安装到 `%LOCALAPPDATA%\Programs\DeskShell`，不覆盖用户配置；卸载时先运行程序恢复它拥有的 Shell 修改，再删文件/快捷方式与匹配的 HKCU Run 值；用户配置默认保留，`-RemoveUserData` 显式删除。`scripts/package-trial.ps1` 生成含两个 exe、libmpv、安装/卸载脚本和 INSTALL.txt 的 zip，试用包内安装脚本从同目录取二进制；包内明确标注内部试用与 GPL 分发材料未完成。
- 性能验收（`perf-check`）：启动真实 `deskshell app` 进程树（含宿主），测量 `GetProcessTimes` 的 kernel+user CPU 与墙钟，输出单核百分比、整机归一百分比、逐进程 CPU、DWM 时间（若系统允许读取）、退出后遗留宿主与 JSON 报告；缺图片/缺 libmpv/已有实例均非零退出，不静默跳过。性能测试配置预先设置 `onboarding.desktop_icons_asked=true`，不会弹首次引导或修改桌面图标。
- 安装验收（`scripts/install-check.ps1` / `scripts/dev.ps1 install-check`）：在 `artifacts\install-test` 使用隔离 APPDATA/LOCALAPPDATA 完成安装、文件检查、开机启动启用/关闭与恢复、安装目录程序启动/退出、卸载与无残留进程检查；不触碰真实用户配置，也不把临时 HKCU 值留在系统中。

M4 验收对照：

| 验收项 | 结果 | 证据 |
|---|---|---|
| 配置保存/迁移/回滚 | PASS（单元测试） | v1→v2 备份、未知字段递归保留、损坏后 `.prev` 恢复、新版本只读、上一配置备份 |
| 用户意图不被自动降档覆盖 | PASS（单元测试） | `QualityProfile` 与 `EffectivePolicy` 分离；自动规则只影响输出 |
| 自动规则迟滞/冷却/逐屏 | PASS（确定性单测） | 进入/恢复/冷却/条件优先级/逐屏覆盖/规则关闭 |
| Eco 无持续解码/推进 | PASS（实机） | host-check：静态模式 2 秒播放位置推进 0 ms、CPU 0.00% 单核、播放器保留 |
| 效果独立开关且真实释放 | 部分 PASS | UI 动画关闭即停 16 ms 定时器并归位；阴影自检验证关闭后窗口/位图释放；壁纸播放关闭由宿主静态模式停止推进；模糊未交付（M5） |
| 托盘设置与状态 | 代码完成，菜单未真实逐项点选 | 子菜单/勾选/置灰原因；状态框读取真实宿主快照；真实菜单点击回归待执行 |
| 开机启动 | PASS | `autostart --enable/--disable` 实际写入/删除 HKCU Run，值校验通过并已在验收后恢复 |
| 5 分钟 Eco 桌面稳态 <0.5% 整机归一 | PASS | 300 秒：进程树 CPU 0.375 s，单核 0.125%，整机归一 0.0078%；2 进程、无遗留宿主 |
| 暂停/停止/恢复资源行为 | PASS（M1 回归） | `player_active`、暂停/停止 CPU、停止后无残留状态 |
| 安装/升级/卸载 | PASS（隔离目录） | 安装、覆盖安装保留配置、启动/退出无孤儿宿主、卸载删除文件与进程；试用包内脚本同样验证 |
| 不覆盖用户配置 | PASS（单元测试+实跑） | 安装脚本不写配置；重装后隔离配置仍在；保存合并未知字段并保留 `.prev` |

实测环境：Windows 11 build 26200，AMD Radeon 780M 驱动 32.0.12033.1030，单屏 `\\.\DISPLAY2` 2560×1440@125%，16 逻辑处理器。release 构建；性能报告写入 `artifacts/`。

2026-11-11 实测记录：

- `scripts/dev.ps1 check` 通过（fmt、Clippy `-D warnings`、workspace 单元测试）。
- `scripts/dev.ps1 host-check` 通过，新增 Eco 静态模式：2 秒播放位置推进 0 ms、CPU 0.00% 单核、播放器上下文保留、恢复播放 PASS。
- `scripts/dev.ps1 app-self-test` 通过：Dock 3 项、岛体 8 行、文件图标 8、阴影创建/点击穿透/关闭释放、700 ms Present 0。
- `perf-check --seconds 60 --warmup 20`（release，桌面附着）：进程树 CPU 0.094 s / 60 s，单核 0.156%，整机归一 **0.0098%**，阈值 0.5%，PASS；报告 `artifacts/m4-perf-release-desktop-smoke.json`。
- 同参数预览窗口模式：整机归一 0.0000%（低于 100 ns 计时分辨率），报告 `artifacts/m4-perf-release-smoke.json`。
- 5 分钟正式测量（release，桌面附着，预热 20 秒）：墙钟 300000 ms，进程树 CPU 0.375 s，单核 0.125%，整机归一 **0.0078%**，阈值 0.5%，PASS；宿主 0.281 s + 主进程 0.094 s，2 个进程，遗留宿主 0，报告 `artifacts/m4-perf-300s-desktop.json`。
- `perf-check` 的 DWM CPU 时间在本机不可读（受保护进程/权限不足），报告明确记录该限制；未用 DWM 读数冒充通过证据。GPU 时间当前未采集（M5 引入）。
- 安装验收：`scripts/install-check.ps1` PASS——安装/覆盖安装保留隔离配置、HKCU Run 启用/关闭校验、安装目录程序 `app --seconds 3` 正常退出且无孤儿宿主、卸载删除文件与进程。
- 试用包：`scripts/package-trial.ps1 -Version 0.1.0-m4` 生成 `artifacts/DeskShell-trial-0.1.0-m4.zip`（约 49.7 MB）；用包内 `install.ps1` 安装后运行 `doctor`（协议 4）通过，包内 `uninstall.ps1` 删除安装目录。

未验证/阻塞：

- 模糊未交付（M5 Render Graph）；HDR 未立项验收。
- 自动规则的全屏/遮挡/电池/RDP/指定应用仅做纯函数测试与平台信号代码；未在真实全屏游戏、RDP、电池切换下观测完整链路。
- 真实托盘/子菜单逐项点选与文件对话框选择待执行；开机启动的注册表写入/删除已通过 CLI 实测。
- 100%/150% DPI、多屏、NVIDIA/Intel/混合显卡未覆盖；逐屏覆盖只用单屏数据验证。
- 监测面板不存在，也没有高频遥测；状态对话框按需读取快照，不存在“关闭面板后仍在采样”的路径。自动规则启用时仅有 2 秒低频采样；全部规则关闭后定时器停止。
- 面板阴影为静态软阴影，不是背景模糊；真实背景模糊仍需 M5 的合成/采样验证。

手工验收步骤（部分已执行）：

1. 真实点选托盘“画质”子菜单切换 Eco/Balanced/Ultra/Custom，检查状态框中的预设、实际呈现与原因同步变化；关闭“壁纸播放”后检查视频停在当前帧。（菜单真实点选待执行）
2. 逐个开关自动规则；在 10 秒内反复切换全屏窗口，确认不会出现快速抖动（进入 1.5 s、恢复 30 s）。（待执行）
3. 托盘切换“开机启动”，用 `reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v DeskShell` 检查值；再次关闭确认删除。（已用 `autostart --enable/--disable` 实测并恢复；托盘点击待回归）
4. 使用 `scripts/install.ps1 -InstallDir artifacts\install-test -SourceDir target\release -NoShortcut -NoStart` 安装，运行安装目录内程序，再用 `scripts/uninstall.ps1 -InstallDir artifacts\install-test` 卸载；确认目录删除、HKCU 值清理、桌面恢复。（已由 `scripts/install-check.ps1` 隔离执行并通过）
5. 修改配置文件加入未知表与未知字段，重启程序并修改画质，确认未知字段仍在且 `.prev` 存在；手动写坏主文件，确认从 `.prev` 恢复。（已由 config 单元测试覆盖；真实用户配置未做破坏性测试）

## M5：高画质增强（核心验收通过，视频互操作原型通过，2026-11-11）

结论：图片效果（调色、暗角、可调模糊、Bloom、粒子、渐变）已通过原生 D3D11 + WIC + HLSL 管线交付，接入宿主与托盘；静态效果只渲染一帧，粒子动画只在 Animate 时按 30 fps 唤醒，切换 Static 立即停帧，关闭全部效果回到 mpv 图片路径。Render Graph 的节点裁剪、帧缓存、纹理预算与降级有确定性单测。视频互操作独立原型已实际跑通固定 libmpv 的 `MPV_RENDER_API_TYPE_D3D11`，但未接入日常播放路径；GPU 时间戳在本机 AMD 驱动上恒为 0，因此报告不可用而不伪造数值。多厂商、真实设备重置、HDR、长稳与真实托盘点选仍未验证。

已完成：

- ADR 0007：选择原生 D3D11（交换链 + WIC + 运行时 HLSL），不引入 wgpu；说明复审条件与视频互操作边界。
- `crates/render/src/graph.rs`：最小实用 Render Graph。节点静态声明输入/输出、时间依赖、分辨率与 pass 数；按效果参数裁剪；`FrameCache` 对相同计划/素材/尺寸/时间片跳过重绘；`TexturePool` 记账与预算；超出预算先降级 Bloom，基础缓冲仍超限则拒绝。单测覆盖逐项开关的 pass 数、静态缓存、动画时间片、Bloom 降级、预算拒绝与纹理池复用。
- `crates/render/src/effects/`：WIC 解码（BGRA8，尺寸/内存上限）、内嵌 HLSL、`D3DCompile` 运行时编译、全屏三角、分离模糊、Bloom 半分辨率链路、粒子/渐变程序化效果、DXGI flip 交换链、一次诊断回读 PNG。静态效果只 Present 一次；动画由宿主期限驱动。
- 宿主 `instance.rs`：图片 + 效果启用时走 `RenderPath::GpuEffects`，否则保持 mpv 图片路径；效果开关切换会重建/释放管线；粒子在 `Animate` 且壁纸可见时按 33 ms 调度，`Static`/暂停后停止并清空期限；效果管线失败记录 `effects_error` 并回退 mpv 图片路径，不阻止图片显示；停止/切源/Explorer 重建/关机释放效果管线。
- 协议 v5：`RenderOptions.effects`；`InstanceStatus.render_path/effects/effects_active/effects_error/effect_passes/present_count/gpu_time_us`。配置 v3：`quality.effects`，v2→v3 迁移前保留 `.v2-<时间>.bak`；效果强度 0..=100，归一化后持久化。
- 策略：Eco 关闭图片效果；Balanced 默认轻量调色/暗角；Ultra 增加模糊/Bloom/渐变；Custom 使用用户逐项值。自动降档关闭 Bloom/模糊/粒子，远程会话关闭全部图片效果，且不写回用户意图。托盘“画质 → 图片效果”为每个效果提供关闭/低/中/高；首次修改自动转为 Custom。状态框显示效果、pass、Present、GPU 时间与错误/说明。
- 视频互操作 `wallpaper-host --interop-check`：固定 libmpv 以 `MPV_RENDER_API_TYPE_D3D11` 在调用方设备上创建 render 上下文，把视频帧直接写入调用方 `ID3D11Texture2D`，结束时仅做 1 次诊断回读保存 PNG；正常路径 0 次逐帧 GPU→CPU→GPU 拷贝。

M5 验收对照：

| 验收项 | 结果 | 证据 |
|---|---|---|
| 高画质有可见提升并保存参考图 | PASS | `artifacts/m5-effects-reference.png`（2560×1440，SHA-256 `A8CEE223…C3BBF`）；调色、暗角、模糊、Bloom 肉眼可见 |
| 逐项关闭真实减少 pass | PASS | host-check：静态效果 9 pass → 关闭 Bloom 5 pass → 关闭全部效果 0 并回退 mpv；粒子组合 3 pass |
| 关闭效果回归基础路径并回收资源 | PASS | 关闭全部效果后 `render_path=mpv`、`player_active=true`、`effect_passes=0`、`present_count=0` |
| 静态效果停帧 | PASS | 稳定后 1.2 s `present_count` 增量 0；宿主 CPU 0.00% 单核 |
| 粒子动画可停帧 | PASS | Animate 1 s 内 21 次 Present；切换 Static 后 1.2 s 增量 0，CPU 0.00% 单核 |
| GPU/CPU/墙钟分开 | 部分 PASS | 已实现 timestamp disjoint/timestamp 查询；本机 AMD 780M 返回 Frequency 100 MHz 但 start/end 恒 0，状态报告 `gpu_time_us=None`，不用 0 冒充 GPU 时间；CPU 与墙钟单独记录 |
| 视频互操作原型 | PASS（原型） | 多次 3 s 运行 85–87 帧；首帧 219–265 ms；平均 33.6–34.4 ms、p95 34.4–34.6 ms、最大 150–201 ms；硬解 `d3d11va`；同设备 BGRA8 1280×720；每帧拷贝 0；诊断回读 1 次；`artifacts/m5-video-interop.png` |
| 视频效果接入日常播放 | 未交付 | 原型只验证 render API 与同设备纹理，未接入 mpv 播放生命周期；不提供假开关 |
| 设备失效恢复 | 未验证 | 效果管线失败会回退 mpv；真实设备移除/重置未注入 |
| HDR | 未验收 | 仍为独立立项 |
| NVIDIA/Intel/混合显卡、多屏/100%/150% DPI | 未验证 | 本机仅 AMD 780M、单屏 2560×1440@125% |

2026-11-11 实测记录：

- `scripts/dev.ps1 check`：见下方“实测状态”，包含新增 Render Graph/效果/协议/配置单测。
- `scripts/dev.ps1 host-check` PASS：新增效果场景（9→5→0 pass、静态/动画停帧、回退 mpv），M1 视频/Eco/故障注入全部回归通过。
- `scripts/dev.ps1 effects-check` PASS：生成 `artifacts/m5-effects-reference.png` 与 `artifacts/m5-effects-particles.png`，非空 PNG 并记录 SHA-256（静态 `A8CEE223…C3BBF`，粒子 `E7CF7F97…E148337`）；两张图均已人工检查，无黑屏、无通道错乱、无上下颠倒，粒子光点清晰可见。
- `scripts/dev.ps1 interop-check` PASS：上述视频互操作原型，输出 `artifacts/m5-video-interop.png`；该 PNG 已人工检查为正确彩条测试帧。多次运行 85–87 帧/3 s，平均 33.6–34.4 ms，p95 34.4–34.6 ms。
- 运行时统计口径：debug 构建；host-check 的 CPU 为宿主进程 `GetProcessTimes` 单核百分比；视频互操作的平均/分位数为单帧 `render_frame` 墙钟，包含 mpv 按目标帧时间阻塞的等待，不等同 GPU 执行时间。

未验证/阻塞与后续：

- GPU 时间：AMD 780M 驱动时间戳恒 0；需要换驱动/厂商或使用 GPUView/ETW 等外部工具另行测量，不能把当前 0 当作性能结论。
- 视频互操作尚未接入日常播放；接入前需验证设备恢复、纹理格式组合、暂停/停止、EOF/循环与显存趋势，并重新评估是否值得替换已验证的 mpv 直接播放路径。
- 真实托盘菜单逐项点选（图片效果子菜单）待执行；当前通过策略/协议/单测与 host-check 验证链路。
- 多屏、100%/150% DPI、NVIDIA/Intel/混合显卡、HDR、24/72 小时资源趋势未验证。
- 背景模糊（窗口背板）仍不在图片效果范围内；当前交付的是图片后处理模糊，不能用面板阴影冒充。

## M6：长期使用版本（实现与本地候选包完成，长期/多厂商验收受阻，2026-11-11）

结论：版本化 CLI、结构化滚动日志与诊断导出、受管理素材导入与缺失回退、安装升级备份/回滚、GPL 许可材料、可追溯候选包与兼容性报告脚本已实现并通过本机端到端验收。`soak-check` 的 72 秒冒烟通过（14 次采样、2 次宿主强杀、0 错误、0 孤儿、句柄差 -7、私有内存 +0.1 MiB）。24/72 小时真实长稳、NVIDIA/Intel/混合显卡、多屏、100%/150% DPI、HDR 与真实设备失效仍未完成，因此不把本阶段写成“长期使用版正式完成”，也不伪造发布签名或兼容性结论。

已完成：

- 版本化 CLI（`crates/deskshell/src/control.rs`）：单实例内命名管道控制通道，当前用户专用、拒绝远程；请求转发到 UI 线程执行。命令：`status`、`pause`、`resume`、`set-wallpaper`、`preset`、`restore-desktop`、`quit`、`diagnostics export`、`assets export`。`--json` 输出稳定字段；退出码 0 成功、2 参数错误、3 应用未运行、4 命令拒绝、5 超时/传输失败。
- 结构化诊断（`crates/deskshell/src/diagnostics.rs`）：`%LOCALAPPDATA%\DeskShell\logs\deskshell.log`，JSON 行，单文件 2 MiB、保留 5 个滚动文件；记录启动、配置警告、宿主连接/失败、策略变化、控制命令/响应与退出。`diagnostics export` 输出应用状态、宿主快照、系统版本、DXGI 适配器/显示器与最近 500 行日志的 JSON（原子写入），由用户主动触发。
- 素材迁移（`crates/deskshell/src/config.rs`、`app/control_ui.rs`）：`--managed` 复制到 `%LOCALAPPDATA%\DeskShell\assets`，保留 `original_source`；默认 `--reference` 引用原文件。`last_good_source` 在当前素材缺失时回退；配置 `.prev`/迁移 `.bak` 规则继续有效。`assets export` 导出配置快照、当前素材与 `manifest.json`；导入通过 `set-wallpaper --managed` 完成。
- 安装、升级与回滚：`scripts/install.ps1` 在升级前把旧 exe/宿主/libmpv 备份到 `InstallDir\previous`；`scripts/rollback.ps1` 停止占用进程并恢复上一版本；`uninstall.ps1` 删除文件/快捷方式并恢复本程序拥有的 Shell 修改。安装脚本拒绝在程序运行时替换（`-Force` 才会先停止）。
- 打包与许可：`scripts/package-release.ps1` 使用 `--release --locked`，校验固定 libmpv SHA-256，复制 `licenses/GPL-2.0.txt`、`licenses/THIRD-PARTY-NOTICES.txt`、安装/卸载/回滚脚本，生成含工具链、协议/配置版本、全部文件 SHA-256 的 `manifest.json` 与 `SHA256SUMS`。许可文件包含 GPLv2 文本、上游 release/commit、二进制校验值与三年源码提供方式；不是 TODO。
- 兼容矩阵：`scripts/compat-report.ps1` 输出 OS build、CPU、GPU/驱动、显示器、`probe`/`doctor`；`docs/COMPATIBILITY.md` 记录 AMD 780M 样本与所有未验证项。
- 稳定性工具：`scripts/soak-check.ps1` 启动真实应用，按间隔采样应用与宿主的工作集/私有内存/句柄/线程/CPU，循环切换图片效果、视频、暂停/恢复、恢复桌面与宿主强杀，结束时检查孤儿宿主并写 JSON 报告。

M6 验收对照：

| 验收项 | 结果 | 证据 |
|---|---|---|
| 版本化 CLI 与稳定退出码 | PASS | `scripts/dev.ps1 cli-check`：status/pause/resume/set-wallpaper/preset/quit/诊断导出、非法参数 2、未运行 3、效果 9 pass、无孤儿宿主 |
| 配置/素材迁移与缺失回退 | PASS（本机） | v2→v3 单测；`--managed` 实际复制到 LOCALAPPDATA assets 并保留原始路径；缺失 source 回退到 `last_good_source` 并写回配置 |
| 诊断日志与导出 | PASS | 滚动 JSON 日志实际生成；导出包含 app/host/compat/system/log_lines，CLI 校验 JSON |
| 安装/升级/卸载 | PASS（隔离目录） | 安装、覆盖安装保留配置并生成 `previous`、卸载删除文件；GPL 文本/第三方说明随包 |
| 回滚 | PASS（隔离目录） | 将安装目录 exe 替换为 debug 后执行 `rollback.ps1`，SHA-256 恢复为 release 值 `66F4B9DD…712443` |
| 发布包与依赖可追溯 | PASS（本地候选） | `artifacts/DeskShell-0.2.0-m6.zip`（49,982,912 字节，SHA-256 `E3052149…33130B8`，manifest 标记 `git_dirty=true`）；`manifest.json` 记录 rustc/cargo/git commit/dirty、协议 5、配置 3、libmpv 校验值 `0586211E…F545` 与全部文件哈希 |
| GPL 许可与源码提供 | PASS（材料） | `licenses/GPL-2.0.txt`、`licenses/THIRD-PARTY-NOTICES.txt`，包含上游源码地址与三年提供声明 |
| 兼容性矩阵 | 部分 | AMD 780M 样本与未验证清单；NVIDIA/Intel/混合显卡、多屏、100%/150% DPI 未覆盖 |
| 24 小时初验 / 72 小时发布压力 | 未完成 | `soak-check` 72 秒冒烟 PASS；24/72 小时必须真实运行并审查趋势，不能用短测替代 |
| 无持续资源增长、无孤儿、无重启风暴 | 仅短时 | 72 秒：句柄差 -7、私有内存 +0.1 MiB、2 次宿主强杀后恢复、0 孤儿；长稳未证明 |
| 升级/回滚与配置恢复 | 部分 | 手动安装/回滚通过；无在线更新器，无“更新开关”，不提供假自动更新 |
| 签名与正式发布 | 阻塞 | 无代码签名证书/发布凭据；只产出本地候选包，不伪造签名或兼容性 |

未验证/阻塞与后续：

- 24/72 小时长稳、休眠/锁屏/热插拔循环、真实驱动设备移除、显存趋势与功耗未完成。
- NVIDIA/Intel/混合显卡、多屏、100%/150% DPI、HDR/10-bit/HEVC/AV1 未覆盖。
- 在线更新通道未实现；当前为手动安装，安装前需退出程序，回滚使用上一版本备份。稳定/实验通道边界只体现在版本号与候选包说明中，不声称已有自动更新或灰度能力。
- 安装/回滚只在隔离目录验证；真实用户目录升级仍需在发布前按 `docs/COMPATIBILITY.md` 流程复测。
- GPU 时间在 AMD 780M 上仍不可用；长稳资源报告不包含 GPU 时间与每进程显存。

## 每阶段共用检查

UI 审查补充：Dock/岛体共用面板已完成第一轮排版、语义色、Fluent 系统图标与命中/位图安全加固，验证与残留问题见 [UI_REVIEW.md](UI_REVIEW.md)。这不替代搜索/滚动、独立设置页、UIA 和多屏交互验收。

2026-11-12 第二轮：完整应用/文件列表、原生搜索/分类、滚动条/翻页、可见图标后台加载、独立原生设置窗口、Dock 名称提示/选择轮廓及系统主题/DPI 适配路径已实现。`check`、扩展 `app-self-test`、`host-check` 和 `capsule-check` 通过。中文 IME、实际主题/高对比度切换及物理多屏 DPI 仍待实测；设置窗当前为原生控件风格，详细边界见 UI_REVIEW。

2026-11-12 第三轮视觉重构：Dock、灵动岛、应用库与设置窗统一为暗/亮玻璃材质与自绘卡片；设置窗不再使用系统按钮控件，命令仍复用主程序。Dock 图标、分类胶囊、搜索框、悬停提示与滚轮/键盘导航同步重做。`check` 与 `app-self-test` 通过，截图见 `artifacts/ui-v2-*.png` 与 [UI_REVIEW.md](UI_REVIEW.md)。真实背板模糊、滚动条拖动、实际系统主题/高对比度、中文 IME 和物理多屏 DPI 未验收。

2026-11-12 第四轮桌面控件：新增 `render::widgets` 测试控件（时钟/天气/音乐/壁纸卡片），通过环境变量加载动漫静态图与 4K 视频测试播放列表，托盘入口与 `scripts/run-wallpaper-test.ps1` 一并提供。控件默认不创建，天气/音乐为演示数据；只有壁纸切换与暂停/恢复修改真实状态，关闭控件即释放定时器和窗口。`check` 与 `app-self-test` 通过，验证与截图见 [UI_REVIEW.md](UI_REVIEW.md)。真实鼠标拖动、托盘重开、多屏 DPI 与壁纸可见性仍需人工复测。

2026-11-12 第五轮壁纸铺满与桌面任务栏：协议 v6 增加 `fit`（cover/contain/stretch/tile），静态图关闭效果时仍走 GPU UV 映射，实测 cover 无黑边；新增 `shell-win::running` 运行应用枚举与前台切换，Dock 支持运行项/系统入口/三种样式/数量上限；灵动岛拆为运行、工具、文件三页签；小部件改为桌面收纳、时钟、真实天气与 QQ 音乐（SMTC），支持拖动、右边缘调宽、关闭禁用与配置持久化。Dock/灵动岛曾改为常驻置顶（第六轮按用户要求改回热键唤起）。`check`、`app-self-test`、`host-check` 全部通过。

2026-11-12 第六轮参考图对齐与快捷控制：Dock 改为白色玻璃托盘，图标 96px 提取并统一圆角，最右新增设置入口；Dock/灵动岛取消常驻置顶，灵动岛启动隐藏、热键呼出、失焦收起，宽度缩到 440 DIP、最多 5 行、工具 2×2；新增快捷控制小部件（真实音量/静音、WLAN SSID、蓝牙状态与开关、四个系统入口），配置 v6 迁移为仅快捷控制默认开启。`dev.ps1 check`、`app-self-test`、`host-check` 全部通过；音量 37%→75% 与设置入口实测可用。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 host-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 cli-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 effects-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 interop-check
```

长稳与发布相关检查（按需串行执行）：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 soak-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 compat-report
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 package-release
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dev.ps1 install-check
```

额外执行该阶段真正相关的图形/恢复/性能验证；代码检查通过不等于视觉与硬件验收通过。
