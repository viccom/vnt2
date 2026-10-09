# vnt2_web 子命令 CLI 实施计划（busybox 派发 + 独立 crate vnt-webctl）

> 目标：`vnt2_web` 单二进制同时提供服务模式与 HTTP 管理子命令；鉴权为
> 设计「loopback 免 token、非 loopback 强制 token」默认。**风险 5 为已识别的
> 剩余权衡，须用户点选后再开工**（见 §5）。

**日期**：2026-10-09 ｜ **TDD**：每任务先红后绿、一任务一提交 ｜
**范围**：仅 vnt2 仓库；协议零改动、vnts2 不涉入

## 1. 三个已核实的论断（方案据此构建，不复述推理）

1. **鉴权中间件只裹 `/api/*`**（`token_auth_middleware` 经 `from_fn_with_state`
   挂到 `api` router 再 `.merge(api.layer(...))`，[service_http.rs:1267]）——
   静态 UI 与 `/` 本就公开，免 token 变更只影响 API 面。
2. **服务模式派生入口共 4 处，且 none 以裸子命令词打头**：
   ncpe systemd（`--addr ...`）、u18 nohup（`--addr/--token`）、桌面 sidecar
   （`Command::new` 传 `--desktop_config`）、`web_service::dispatch`（`--service`）。
   派发走「argv[1] 白名单精确匹配」，裸 flag 与 `--xxx` 永不误入 CLI 分支。
3. **`service_http.rs` 已存在 main 直接复用的公共 surface**（lib.rs
   `pub use {VntService, generate_access_token, run_http_server}`）——
   不新建重复入口，`vnt-webctl` 与 `vnt-web` 共用 service_http 的全套路由与鉴权。

## 2. 架构

```
┌ vnt2_web（root crate，mode 派发 ~25 行）
│  argv[1] ∈ {config,start,stop,restart,status,info,peers,routes,help}
│   → 走 vnt-webctl::run_cli(args[1..])  → HTTP 打本机服务
│  否则 → 现有 Args::parse() + run_server（零改动路径）
│
├ vnt-webctl（新 workspace 成员）
│  clap derive 子命令 + reqwest（default-features=false）薄封装 + 输出
│
├ vnt-web（服务模式不动 + loopback 免 token）
│  run_http_server 监听后；鉴权短路由来源 IP 判定
```

## 3. loopback 免 token 的鉴权设计（回答用户问题，含两条安全论断）

**Q：仅 127.0.0.1 操作能否跳过 token？**

**能，但要清楚三处连带**，不是无脑删鉴权：

- **论断 A**：token 的存在是为「绑定非 loopback 地址」场景设的（`--addr 0.0.0.0:...`
  或被代理暴露时，管理面须口令）。服务**默认绑 127.0.0.1**（main_web.rs:128），
  loopback 场景 token 只挡住本机未授权用户。
- **论断 B**：X-Forwarded-For 不可信——免 token 判定**只能用连接对端地址**
  （`ConnectInfo<SocketAddr>`），绝不读请求头；代理转发时真实来源被代换，不能放行。
- **论断 C**：多用户机器上本机任意进程即可全权管理（启动/停止/读取含密码的
  配置文件）——Linux 的等价物是「socket 绑 127.0.0.1 即信任本机」（与 docker
  daemon 的 unix socket 口径一致），需用户明确接受。

**论断 D（复核新得，决定性）**：`vnt2_web` 唯一的 `start_http` 调用点在
`src/main_web.rs:146`——**桌面版 Rust 侧并不调 `start_http`**（rg 零命中），
它经 sidecar 子进程 `vnt2_web --desktop_config` 再入 main_web 的服务模式。
`run_http_server`（lib.rs 公共导出）**无任何生产调用方**。因此鉴权改造
收敛到 `http_router` + `start_http` 一处，三形态（服务/桌面 sidecar/CLI）零分叉。

**设计（默认启用，留退出开关；复核修订：token 不选型、AuthConfig 服务端组装）**：

```rust
// vnt-web/src/service_http.rs 新增
pub struct AuthConfig {
    token: String,        // 服务端组装的真 token（桌面随机/显式/--token 统一入口）
    strict: bool,         // --auth-strict 或 VNT_WEB_AUTH_STRICT
}
// token_auth_middleware 判定次序：
//   1. Bearer 比对通过 → 放行（任何来源，含 loopback；向后兼容）
//   2. !strict && connect_info.ip().is_loopback() → 放行（免 token 场景）
//   3. 否则 401
```

只改 `token_auth_middleware` + `AuthConfig` + `ConnectInfo` 注入 + `Args` 加
`--auth-strict`；**`start_http` 签名随之变更**（`token: String` → `AuthConfig`），
唯一调用方 main_web.rs:146 同步适配；静态路由与其余 22 端点不动。

### 复核钉死的两个已知陷阱

1. **ConnectInfo 注入须改 `axum::serve` 调用形态**：`start_http` 里
   `axum::serve(listener, app)` → `axum::serve(listener,
   app.into_make_service_with_connect_info::<SocketAddr>())`，否则 middleware 的
   `ConnectInfo` extractor 拿不到连接对端。axum 0.8 该 API 无需额外 feature。
2. **`start_http` 与 `run_http_server` 签名同步**：`run_http_server`（lib.rs
   公共导出，无生产调用）也接受 `AuthConfig` 透传，保持 public API 一致；
   否则 lib 与 bin 的鉴权语义分叉。

## 4. 任务分解（每任务一提交，先红后绿）

### Task 1：vnt-webctl 骨架与 workspace 接线

**Files**：`vnt-webctl/Cargo.toml`（new）、`vnt-webctl/src/lib.rs`（new）、
`Cargo.toml`（workspace.members 追加 `vnt-webctl`）、
`vnt-webctl/tests/cli_parse.rs`（new）

- clap derive 定义：`config { add --from-file|--subscription, list, show, delete }`、
  `start|stop|restart|status <name>`、`info|peers|routes <name>`
- `pub fn run_cli(args: impl Iterator<Item=String>) -> anyhow::Result<i32>`——
  返回退出码，由 main_web 映射（`code:0/-1` → 0/1），库自身不 `exit()`
- reqwest 阻塞客户端（CLI 短命进程，不需要 async 运行时）+
  `default-features = false`（http-only；vnt-web 的 axum 已把 hyper 带进树）
- 测试：非法子命令/缺名/缺参的解析错误（先红）

**复核钉死**：vnt2 root bin `vnt2_web` 的 Cargo 依赖须追加
`vnt-webctl = { path = "vnt-webctl", optional = true }` 且 `vnt-web` feature
扩展为 `vnt-web = ["dep:vnt-web", "dep:vnt-webctl"]`——这是 root Cargo.toml 的
**唯一一处共享改动**，vnt2_cli/vnt2_ctrl 不受影响（required-features 不含它）。

### Task 2：config 三命令 + 订阅流程

**Files**：`vnt-webctl/src/config.rs`（new）

- `config add <名> --from-file <路径>`：读文件 → POST /api/config
- `config add <名> --subscription <码>`：POST /api/subscription/preview →
  打印 server/network_code/device_id/revision → 生成含 `subscription=` 的
  TOML（模板见下）→ POST /api/config；**不连接 vnts2、不落远端配置**
- `config list / show`：GET /api/config/list、/api/config
- `config delete`：需 `--yes`（无 TTY 交互场景；带确认输出）
- 测试：订阅 TOML 模板字符串生成（纯函数）、preview 响应字段映射（先红）

```toml
config_name = "<名>"
subscription = "<码>"
```

**复核钉死（file_name 规范化须与 API 服务端对齐）**：服务端 `save_config`
在 `normalize_config_file_name`（service_http.rs:1892）里**无扩展名自动补
`.toml`、非 .toml 拒绝**；且 CLI 传入的 `<名>` 直接作为 `file_name` 发请求。
CLI 侧须做**与 normalize 相同的预校验**（无 `..`/`/`/`\`，无扩展名补 `.toml`），
否则 `vnt2_web config add dongnet --subscription ...` 与服务端补名后的磁盘文件
`dongnet.toml` 在后续 `start dongnet` 时对不上（`is_valid_file_name` 只挡路径
穿越，不挡扩展名不一致）。预校验与 normalize 同逻辑、同错误文案。

### Task 3：实例生命周期命令

**Files**：`vnt-webctl/src/lifecycle.rs`（new）

- `start <名>`：POST /api/start 后**轮询** GET /api/start/status 到
  Running/Stopped（超时 30s 报错退出码 1；打印每轮状态行）
- `stop|restart <名>`：POST 对应端点
- `status <名>`：GET /api/start/status + /api/info 摘要
- 测试：轮询退出条件表（Stopped→OK、Starting→继续、超时→错）（先红）

**复核钉死（start 的幂等与竞态）**：`/api/start` 在实例已 `Running` 或
`Starting` 时的行为须先确认（service_http.rs `begin_vnt_start` 分支）——
CLI 须把「已在运行」视为成功并打印当前状态，不能误判为失败；轮询须容忍
`Starting` 期间 API 偶发 `code:-1`（实例启动竞争窗口），对非终态
`code:-1` 重试而非立即失败（至多 3 次再报错）。

### Task 4：遥测命令 + 输出层

**Files**：`vnt-webctl/src/telemetry.rs`（new）、`vnt-webctl/src/render.rs`（new）

- `info`：HttpAppInfo 全字段表格式化（对齐 docs/web-api.md 字段名）
- `peers`：HttpClientItem 行表（ip/name/online/mode/traffic/packet_loss）
- `routes`：HttpRouteDetail 分组（每 IP 一段）
- `render.rs`：字段对齐、单位换算（bytes）、空态中文提示
- 测试：bytes→KB/MB 换算、空列表空态文案、mode 七种映射完整性（先红）

**复核钉死（遥测字段的 Option 语义）**：HttpAppInfo 大量字段为 `Option`
（ip/gateway/nat_type/mtu/fec/compress/rtx/encrypt），实例 `Stopped` 时全部
为 `null` 而非缺省——render 层对 `Stopped` 实例须输出「未运行」而非空表；
`peers`/`routes` 在实例未运行时 API 返回 `code:-1 "VNT not running"`，
CLI 须将其映射为「实例未运行」并退出码 0（信息性），不视为命令失败。

### Task 5：HTTP 客户端与错误映射

**Files**：`vnt-webctl/src/http.rs`（new）

- `struct WebClient { base: String, token: Option<String> }`，reqwest::blocking
- `call(method, path, body) -> Result<serde_json::Value, WebError>`：
  `{code:0}`→data；`{code:-1,msg}`→`WebError::Api(msg)`；HTTP 非 2xx→
  `WebError::Http`；连接失败→`WebError::Connect`（输出「服务未运行或无法连接
  （须以 --token 固定令牌启动，或本机 loopback 免 token 模式）」）
- **token 来源优先序**：`--token` > `VNT_WEB_TOKEN` env > 无（loopback 免 token）
- 测试：三类响应到 WebError 的映射、token 注入/省略路径（先红）

**复核钉死（base 地址构造）**：CLI 默认 base 为 `http://127.0.0.1:19099`——
与服务模式默认绑定一致；`--addr` 覆盖时须做 `host:port` 解析与 scheme 补全
（用户传 `19099` 或 `127.0.0.1` 均能工作），禁止用户传 `http://...` 后
再拼 path 造成双斜杠（用 `url::Url::join` 或手工 trim_end_matches('/')）。

### Task 6：main_web 派发接线

**Files**：`src/main_web.rs`（~25 行）、`Cargo.toml`（vnt2 加 vnt-webctl 依赖 +
vnt-web feature 扩展）

- `fn main()` 在 `Args::parse()` 前：`argv[1]` 白名单命中 →
  `vnt_webctl::run_cli(std::env::args().skip(1))` → `process::exit(code)`；
  否则落现有 `run()`
- **四条服务模式派生回归**（非测试，逐条人工跑）：
  - `vnt2_web --addr 127.0.0.1:19099 --token X` → 服务模式
  - `vnt2_web --desktop_config <path>` → 服务模式（桌面 sidecar 路径）
  - `vnt2_web config list` → CLI 模式
  - `vnt2_web serve`（**故意不注册**；须被误派发到 CLI 并报「未知子命令」
    而非静默服务模式，防歧义）

**复核钉死（派发边界与 windows service 的序）**：`Args::parse()` 是 clap
derive，**遇未知首参直接报错退出**——所以 argv[1] 白名单检查必须发生在
`Args::parse()` 之前，否则 `vnt2_web config list` 会被 clap 先报
「unexpected argument 'config'」；windows `--service` 路径在 `run()` 里
`cfg(windows)` 分支优先于 tokio runtime（main_web.rs:93–97），派发检查须
在 `Args::parse()` 前、即整个 `run()` 的最顶端，不影响 `--service` 路径
（`--service` 不是 argv[1] 白名单词）。

### Task 7：loopback 免 token（服务端，唯一动 service_http 的改动）

**Files**：`vnt-web/src/service_http.rs`（middleware + AuthConfig + 3 测试）、
`src/main_web.rs`（Args 加 `--auth-strict` + start_http 调用适配）、
`vnt-web/src/lib.rs`（run_http_server 签名同步）

- `AuthConfig { token, strict }`（见 §3 设计）
- `token_auth_middleware` 改为 `from_fn_with_state(AuthConfig, ...)` +
  `ConnectInfo<SocketAddr>` extractor；判定次序：Bearer 通过 → 放行；
  `!strict && is_loopback` → 放行；否则 401
- `start_http` / `run_http_server` 签名 `token: String` → `AuthConfig`；
  main_web.rs:146 组装 `AuthConfig { token, strict: args.auth_strict }`
- 测试（vnt-web 内）：loopback 无 token 放行、loopback + strict 拒绝、
  非 loopback 无 token 拒绝（先红，三个分支锁死）

**复核钉死（AuthConfig 与 strict 的构造位置）**：`strict` 的解析
（`--auth-strict` flag 或 `VNT_WEB_AUTH_STRICT` env 任一为真）在 main_web
的 `Args` 里完成（clap `#[arg(long, env = "VNT_WEB_AUTH_STRICT")]`），
`AuthConfig` 只做值承载、不做 env/flag 判断——保持 vnt-web 库无 CLI 概念。

### Task 8：门禁、出包、文档

- 门禁：vnt-core 300、vnt-web 37+（新增 middleware 测试）、vnt-webctl 全部、
  cargo clippy/fmt 双侧、pnpm build
- 出包：四平台重打（vnt2_web 体积预期 +100–200KB，其余 bin 不变；字节级对比验证）
- 文档：docs/web-api.md 补「vnt2_web 子命令」一节；README 增强清单 +1 条；
  AGENTS.md 补命令表
- E2E 冒烟（手动清单，非脚本）：起服务（免 token loopback）→
  `config add --subscription` 拉 metme 真实订阅 → start → info 有虚拟 IP →
  stop → config delete

**复核钉死（metme 侧 token 前提）**：E2E 冒烟用的 metme vnts2（公网平台）
**与 vnt2_web 本地服务是两个独立进程**，前者只提供订阅码/升级推送，不涉及
本特性；CLI 打的永远是**本机** `vnt2_web`。冒烟时若本机已有运行中的
`vnt2_web`（开发机上的调试实例），先确认其 `VNT_WEB_TOKEN` 或改用
`--auth-strict` 关掉的实例，避免免 token 模式被既有实例的 token 意外拦截。

## 5. 风险 5：用户已确认的两项（按推荐执行）

1. **loopback 免 token 默认启用** + `--auth-strict`（env `VNT_WEB_AUTH_STRICT`）
   退出——接受论断 C 的本机信任语义
2. **破坏性命令**：只 `config delete` 强制 `--yes`；`stop/restart` 输出
   已停止实例名即可

## 6. 明确不做的（YAGNI）

- 不做独立 `vnt2_webctl` bin（用户已选子命令方案）
- 不做 shell 补全、彩色输出、交互式确认（`--yes` 覆盖脚本场景）
- 不动 vnts2、不动协议、不动桌面 sidecar 调用方式
- 不为前端加 CLI 入口（web UI 已有全部功能）
- 不引入 `vnt_webctl` 独立 bin 出包（复用 `vnt2_web` 分发）
