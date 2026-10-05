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
| `vnt-core` | 内核：tun、加解密、打洞、FEC、NAT、路由（api / tunnel_core / nat / fec / crypto / port_mapping …） |
| `vnt-web` | `vnt2_web` 后端：axum HTTP 服务（service_http.rs，/api/peers、/api/config、/api/start …）+ `ui/` 前端 |
| `vnt-ipc` | vnt2_cli ↔ vnt2_ctrl 的进程间通信 |
| `vnt-jni` | Android JNI 绑定 |
| `vnt-desktop/` | Tauri 桌面客户端（src-tauri） |
| 根 `src/` | 三个 bin 入口（main_cli / main_ctrl / main_web）+ 参数、订阅接入、web 服务装配 |

`vnt2_web` 本地 API 监听 `--addr` 指定地址（默认 `127.0.0.1:19099`），凭 `--token` 或环境变量 `VNT_WEB_TOKEN` 鉴权；`/api/peers` 的 `mode` 字段为连接模型（lan / udp-v4 / udp-v6 / tcp-v4 / tcp-v6 / relay）。

## 硬约束（违反即错误）

1. **跨仓库协议镜像**：`vnt-core/src/protocol/control_message.rs` 及 proto 与服务端仓库 [vnt-dev/vnts](https://github.com/vnt-dev/vnts) 互为镜像，任何改动必须双侧同 commit 语义对齐，roundtrip 测试双侧锁定。新增 MsgType 依赖的服务器改动**必须先部署服务器，后分发客户端**（旧服务器 + 新客户端 = 断连）。
2. 未知 MsgType / 未知 proto 字段必须容忍：解码侧不因新增字段失败（prost 跳过未知字段），客户端不得假设服务器永远同版本。
3. 连接模型分类（`LinkModel`：Lan / PunchUdpV4 / PunchUdpV6 / PunchTcpV4 / PunchTcpV6 / Relay）在 vnt-core `route_table.rs` 的 `Route::link_model()`，`vnt-web` /api/peers 的 `mode` 字段与 vnts2 拓扑视图都消费它——改动分类逻辑时同步核对所有消费方。
4. P2P 状态周期上报（`P2pStatusReportMsg`，peers 全量覆盖 + links 明细）节拍与格式改动，须与 vnts2 侧 `update_p2p_peers` 的存储语义对齐。
5. 禁止提交运行产物与密钥；`vnt-web/ui` 构建产物只经 build.rs 进 `vnt-web/static`（gitignore）。

## 纪律

- TDD：新功能先写失败测试再实现；交付前 vnt-core `cargo test --lib` + `clippy` 0 警告全绿
- 提交：conventional commits（feat/fix/docs/refactor/test/chore），中文描述正文
- 注释写"为什么"而非"做什么"
- 精准修改：只碰必须碰的文件，不重构没坏的代码，不顺手改无关格式
- 大型功能在仓库或工作区留计划/实施记录，偏离计划的决定必须留痕

## Rust 约定

- tokio 异步；周期任务 `interval_at` + `MissedTickBehavior::Skip`（参照 vnt-core connection_manager.rs）
- 共享状态经 Context / Arc 句柄，克隆即共享
- 锁跨 await 不持有

## 前端约定（vnt-web/ui/）

- views：Dashboard / Config / ConfigEditor / Peers / Routes / WebAccess / About，新页面沿用现有结构与取数模式
- 消费后端新字段时兼容旧后端：字段缺失时回退既有推断逻辑（参照 PeersView 对 `mode` 为空时按 metric + 对端地址判断）
- 密码/令牌类输入一律 `autocomplete="new-password"`，防止浏览器密码管理器自动填充写入脏数据（曾因此把表单模式密码串进订阅配置，见 239540e）
