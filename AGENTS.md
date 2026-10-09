# AGENTS.md — vnt2 客户端 monorepo

面向 Coding Agent 的仓库规范。所有命令、路径、约束均基于当前代码核实。

## 定位与分支模型

本仓库是 [vnt-dev/vnt](https://github.com/vnt-dev/vnt) 的增强 fork（客户端内核与多形态外壳）。

- `main`：官方上游纯镜像，只 `merge --ff-only origin/main`，**永不直接提交**
- `enhanced`：增强主线（fork 默认分支），官方更新以 merge 并入（已推送历史不 rebase），所有完成的特性合入此分支
- `dev/<feature>`：短命特性分支，从 `enhanced` 分出，完成后合回并删除

同步官方流程：`git fetch origin` → main 快进 → `enhanced` merge main → 全量门禁 → 推送。

## 构建与门禁

内核（vnt-core）：

```
cd vnt-core && cargo test --lib     # 内核单测
cd vnt-core && cargo clippy --all-targets
```

二进制（根 package `vnt2`，bin 与 feature 绑定）：

```
cargo build --release --features vnt-ipc,vnt-web --bins
```

- `vnt2_cli` / `vnt2_ctrl` 需要 feature `vnt-ipc`（cli 与其控制进程的 IPC）
- `vnt2_web` 需要 feature `vnt-web`（本地 HTTP 管理服务 + 内嵌 UI）
- 静态链接用 `--target x86_64-unknown-linux-musl`；已兼容无 64 位原子的 MIPS musl 目标，相关改动勿回退

前端（`vnt-web/ui/`，Vite + Vue）：

```
pnpm build:web    # 根目录执行 = pnpm --filter vnt-web-ui build
```

`vnt-web/build.rs` 会在 cargo 构建 vnt-web 时自动（重）构建 UI 并嵌入 `vnt-web/static/`：产物比源码新则跳过，找不到 pnpm 且无产物则报错；`VNT_WEB_SKIP_UI_BUILD=1` 可跳过。注意 ui 构建仅 `vite build`，**不含独立类型检查**，TS 错误需编辑器把关。

桌面端（Tauri）：`pnpm dev:desktop` / `pnpm build:desktop`（根目录），详见 `vnt-desktop/README.md`。

## 代码地图（Cargo workspace）

| crate / 路径 | 职责 |
|------|------|
| `vnt-core` | 内核：tun、加解密、打洞、FEC、NAT、路由、远程升级执行器（api / tunnel_core / nat / fec / crypto / port_mapping / upgrade …） |
| `vnt-web` | `vnt2_web` 后端：axum HTTP 服务（service_http.rs，/api/peers、/api/config、/api/start …）+ `ui/` 前端 |
| `vnt-ipc` | vnt2_cli ↔ vnt2_ctrl 的进程间通信 |
| `vnt-webctl` | vnt2_web 管理子命令 CLI 库（busybox 派发；HTTP 打本机服务，main_web.rs 顶部按首个裸词白名单分流） |
| `vnt-jni` | Android JNI 绑定 |
| `vnt-desktop/` | Tauri 桌面客户端（src-tauri） |
| 根 `src/` | 三个 bin 入口（main_cli / main_ctrl / main_web）+ 参数、订阅接入、web 服务装配 |

`vnt2_web` 本地 API 监听 `--addr` 指定地址（默认 `127.0.0.1:19099`），凭 `--token` 或环境变量 `VNT_WEB_TOKEN` 鉴权；**loopback 来源默认免 token**（`--auth-strict` / `VNT_WEB_AUTH_STRICT` 退回全量 Bearer，判定只信连接对端 ConnectInfo、不读 X-Forwarded-For）；`/api/peers` 的 `mode` 字段为连接模型（lan / udp-v4 / udp-v6 / tcp-v4 / tcp-v6 / relay）。管理子命令（busybox：`config add --subscription` 等）与全部接口清单见 [docs/web-api.md](docs/web-api.md)（事实源 `vnt-web/src/service_http.rs` 的 `api_router`，改路由/字段须同步该手册）。

## 硬约束（违反即错误）

1. **跨仓库协议镜像**：`vnt-core/src/protocol/control_message.rs` 及 proto 与服务端仓库 [vnt-dev/vnts](https://github.com/vnt-dev/vnts) 互为镜像，任何改动必须双侧同 commit 语义对齐，roundtrip 测试双侧锁定。新增 MsgType 依赖的服务器改动**必须先部署服务器，后分发客户端**（旧服务器 + 新客户端 = 断连）。
2. 未知 MsgType / 未知 proto 字段必须容忍：解码侧不因新增字段失败（prost 跳过未知字段），客户端不得假设服务器永远同版本。
3. **管理类指令扩展走 GenericCtrl(33/34)**：`vnt-core/proto/generic_ctrl.proto` + `src/protocol/generic_ctrl.rs`（转换层）+ `src/tunnel_core/server/generic_ctrl.rs`（dispatch 臂）三处成套增改，与服务端同 commit；扩展步骤与多轮交互模式见 vnts2 仓库 `docs/generic-ctrl-extension-guide.md`。不再为管理指令新占 MsgType。
4. 连接模型分类（`LinkModel`：Lan / PunchUdpV4 / PunchUdpV6 / PunchTcpV4 / PunchTcpV6 / Relay）在 vnt-core `route_table.rs` 的 `Route::link_model()`，`vnt-web` /api/peers 的 `mode` 字段与 vnts2 拓扑视图都消费它——改动分类逻辑时同步核对所有消费方。
5. P2P 状态周期上报（`P2pStatusReportMsg`，peers 全量覆盖 + links 明细）节拍与格式改动，须与 vnts2 侧 `update_p2p_peers` 的存储语义对齐。
6. 禁止提交运行产物与密钥；`vnt-web/ui` 构建产物只经 build.rs 进 `vnt-web/static`（gitignore）。

## 纪律

- TDD：新功能先写失败测试再实现；交付前 vnt-core `cargo test --lib` + `clippy` 0 警告全绿
- **发版版本号双 crate 同步**：节点对外版本统一取 vnt-core 的 `CARGO_PKG_VERSION`（注册信息、hello 上报、升级自检都用它）——发版必须同时 bump 根 vnt2 与 vnt-core 两个 crate，只 bump 一个会导致远程升级的版本自检必然失败（2026-10-08 实测）
- 提交：conventional commits（feat/fix/docs/refactor/test/chore），中文描述正文
- 注释写"为什么"而非"做什么"
- 精准修改：只碰必须碰的文件，不重构没坏的代码，不顺手改无关格式
- 大型功能在仓库或工作区留计划/实施记录，偏离计划的决定必须留痕

## Rust 约定

- tokio 异步；周期任务 `interval_at` + `MissedTickBehavior::Skip`（参照 vnt-core connection_manager.rs）
- **控制连接读循环内禁止 await 等待对端应答**——应答从同一循环进来，等待即死锁；需要应答的交互用待决表在独立任务中限时等待（参照 `GenericCtrlNotifier`），读循环内路径一律发后即弃（参照 `report_pending_upgrade_result` 注释，实测教训）
- 共享状态经 Context / Arc 句柄，克隆即共享
- 锁跨 await 不持有

## 前端约定（vnt-web/ui/）

- views：Dashboard / Config / ConfigEditor / Peers / Routes / WebAccess / About，新页面沿用现有结构与取数模式
- 消费后端新字段时兼容旧后端：字段缺失时回退既有推断逻辑（参照 PeersView 对 `mode` 为空时按 metric + 对端地址判断）
- 密码/令牌类输入一律 `autocomplete="new-password"`，防止浏览器密码管理器自动填充写入脏数据（曾因此把表单模式密码串进订阅配置，见 239540e）

## Windows 交叉编译（Linux 宿主，cargo-xwin 路线）

前置：clang + lld-link + cargo-xwin（`cargo binstall cargo-xwin`），rustup target `x86_64-pc-windows-msvc`。SDK 由 xwin 首次运行自动下载；链接期 LNK4099（缺 CRT 的 PDB）为无害噪音。

```
# ① CLI 三件套（vnt2_cli/ctrl 需 vnt-ipc，vnt2_web 需 vnt-web）
RUSTFLAGS="-C target-feature=+crt-static" cargo xwin build \
  --target x86_64-pc-windows-msvc --release --features vnt-ipc,vnt-web --bins

# ② 桌面版四步（顺序不可换）
pnpm --filter vnt-desktop build                       # 前端 → vnt-desktop/dist
cargo xwin build --target x86_64-pc-windows-msvc --release --features vnt-web --bin vnt2_web
cp target/x86_64-pc-windows-msvc/release/vnt2_web.exe \
   vnt-desktop/src-tauri/binaries/vnt2-web-x86_64-pc-windows-msvc.exe
cd vnt-desktop/src-tauri && \
RUSTFLAGS="-C target-feature=+crt-static" cargo xwin build \
  --target x86_64-pc-windows-msvc --release --features custom-protocol
```

- **custom-protocol 陷阱（2026-10-05 实测踩坑）**：tauri 以 `custom-protocol` feature 区分 dev/prod（tauri crate 的 build.rs：`dev = !custom_protocol`）。`tauri build` CLI 自动启用它；裸 cargo/xwin 构建必须显式 `--features custom-protocol`，否则程序能启动但窗口指向 devUrl（127.0.0.1:1420，vite dev server）且**不内嵌前端资产**——真机症状即"页面拒绝连接"
- **crt-static**：消除 `VCRUNTIME140.dll`/UCRT 动态依赖，干净系统零外部运行时（wintun.dll 与前端 UI 均编译期内嵌）
- **产物自检（无 Windows 环境时的机械验证）**：`strings vnt-desktop.exe | grep -c 'assets/index-'` 应 ≥1（前端资产已嵌入，grep 不到内容是正常的——tauri-codegen 压缩嵌入只留清单明文）；`objdump -p <exe> | grep 'DLL Name'` 不应出现 VCRUNTIME140 / api-ms-win-crt
- 分发形态：桌面版 = `vnt-desktop.exe` + `vnt2-web.exe`（sidecar 改名后与主程序同目录）
