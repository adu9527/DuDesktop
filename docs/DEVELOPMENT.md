# 开发环境

## 本机准备结果

- 已有 Visual Studio Community 2026（18.10），已检测到 `Microsoft.VisualStudio.Component.VC.Tools.x86.x64`。
- 已有 Windows SDK；实际 MSVC 编译、链接与 Win32 窗口运行已验证。
- 已有 Rust 1.96.0；项目通过 `rust-toolchain.toml` 固定版本，包含 rustfmt、Clippy 和 x64 MSVC target。
- 已将 `%USERPROFILE%\.cargo\bin` 补入用户 PATH；已有终端需重开，开发脚本会临时补齐当前进程 PATH。
- Cargo 依赖版本由根目录 `Cargo.lock` 固定，作为应用仓库文件保留。
- 尚未安装 mpv 开发包、RenderDoc/PIX，也未固定 wgpu：分别在 M1/M0 按原型结论引入；当前构建不需要它们、CMake 或额外 GPU SDK。

## 新机器准备

1. 安装 Visual Studio/Build Tools 的“使用 C++ 的桌面开发”，包含 x64 MSVC 和 Windows SDK。
2. 从 https://rustup.rs 安装 Rust，选择 `x86_64-pc-windows-msvc`。
3. 在仓库执行以下命令（rustup 会读取工具链文件）：

```powershell
rustup show
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo run -p deskshell -- doctor
cargo run -p deskshell -- host-check
cargo run -p deskshell -- window
```

先构建整个 workspace，再执行 `host-check`，因为 `cargo run -p deskshell` 不会自动编译另一个二进制包。也可直接使用 README 中的开发脚本。

## 已验证与边界

当前直接依赖：`windows 0.61` 用于原生 Win32 类型和调用；`serde` 用于协议数据序列化；`serde_json` 用于开发握手消息。精确解析版本以 `Cargo.lock` 为准；没有引入 GUI/GPU/播放器大依赖，后续按阶段验证再加入。

- workspace 编译、格式、Clippy（警告视为错误）、单元测试通过。
- 两个有意义的测试覆盖：不兼容协议拒绝、动画结束后恢复无持续帧需求。
- 主进程启动真实子进程并验证 JSON 握手，含 5 秒超时和退出码检查。
- 基础窗口已验证 HWND 创建、关闭请求与正常进程退出；不代表透明、DPI、焦点或 GPU 呈现验收完成。
- 当前 stdout 握手仅为开发自检，不是正式 IPC；M1 替换为版本化、有界、可取消的命名管道会话。
- GPU 能力结构目前只是契约，不返回虚构探测结果；AMD/Intel 尚无实机验证。

## 排障

- `cargo` 不存在：重开终端或使用 `scripts/dev.ps1`，确认用户 PATH。
- 链接器/系统库缺失：用 VS Installer 补齐 C++ x64 工具和 Windows SDK。
- 窗口测试：当前是控制台程序创建 GUI 窗口，不能依赖 `WaitForInputIdle` 判断初始化；观察窗口/枚举 HWND 后发送关闭请求。
- PowerShell 5.1 如中文输出乱码，可使用 Windows Terminal 的 UTF-8 配置；不为此修改系统全局代码页。
- 原生库在确定版本后放入显式运行时目录并记录校验值，避免依赖随机系统 DLL。
