# vnt2_web HTTP API 手册

`vnt2_web` 进程的本地管理 API。事实源为 `vnt-web/src/service_http.rs` 的 `api_router`（含各 handler 与响应结构体）；改动路由或字段时**必须同步更新本手册**。

## 通用约定

- **监听**：默认 `127.0.0.1:19099`，`--addr` 覆盖。
- **鉴权**：全部 `/api/*` 经 `Authorization: Bearer <token>` 中间件；token 来自启动日志中的 `?token=...`、`--token` 参数或 `VNT_WEB_TOKEN` 环境变量。校验失败返回 HTTP 401。
- **响应包裹**：`{"code": 0, "msg": "success", "data": ...}`；失败 `code = -1`，`msg` 为中文原因，`data` 为 `null`。HTTP 状态码一般恒为 200（鉴权失败除外），业务成败看 `code`。
- **多实例模型**：vnt2_web 可同时管理多份配置，每份配置文件对应一个实例。除标注「无参」的接口外，其余均以 `file_name`（配置文件名，如 `default.toml`）定位实例；query 参数或 JSON body 传入。
- **运行时接口前提**：遥测类接口（info/peers/routes）要求实例处于 `Running`，否则报「VNT not running」。

## 一、元信息

| 方法 | 路径 | 参数 | 说明 |
|---|---|---|---|
| GET | `/api/version` | 无 | 程序版本号（`vnt2 <CARGO_PKG_VERSION>` 同源） |
| GET | `/api/runtime` | 无 | 运行时形态字符串（独立进程 / Windows 服务模式） |

## 二、实例生命周期

| 方法 | 路径 | 参数 | 说明 |
|---|---|---|---|
| GET | `/api/instances` | 无 | 全部实例摘要：`[{file_name, config_name, status}]` |
| POST | `/api/start` | body `{file_name}` | 启动实例（异步）。返回前先置 `Starting` 并清旧日志，保证前端首轮询不误读上次结果 |
| POST | `/api/stop` | body `{file_name}` | 停止实例：中断启动任务、摘除运行时管理器、断开订阅控制连接 |
| POST | `/api/restart` | body `{file_name}` | 重启实例 |
| DELETE | `/api/instance` | query `file_name` | 移除已停止实例的卡片及日志缓冲（运行中实例拒绝；用于清理启动失败残留） |
| GET | `/api/start/status` | query `file_name` | 启动状态轮询：`{status}`。实例不存在时返回 `Stopped`（前端据此自然终止轮询） |
| GET | `/api/instance/logs` | query `file_name` | 最近 50 条日志 `[{level, message, time}]`，`time` 格式 `HH:MM:SS`，停止后仍可查 |
| GET | `/api/instance/config` | query `file_name` | 运行中实例的**生效配置** TOML 文本（本地配置与服务端下发合并后的只读快照；读快照而非实时锁，避免阻塞监测循环） |
| GET | `/api/notifications` | 无 | 平台通知聚合（跨实例，无 `file_name`）：`[{file_name, level, title, body, received_at}]`，按 `received_at` 倒序、并列按实例名稳定排序。来源 GenericCtrl notify 环形缓冲（每实例最近 50 条，**会话级：实例停止即丢失**）；前端角标轮询（15s）展示 |

`status` 取值（`VntStatus`）：`Stopped` / `Starting` / `Running`。

## 三、运行时遥测

| 方法 | 路径 | 参数 | 说明 |
|---|---|---|---|
| GET | `/api/info` | query `file_name` | 实例综合信息，见下方 `HttpAppInfo` |
| GET | `/api/peers` | query `file_name` | 对端设备列表，见下方 `HttpClientItem` |
| GET | `/api/routes` | query `file_name` | 路由表：`[{ip, routes: [HttpRouteDetail]}]`，每目的 IP 的候选路由明细 |

**HttpAppInfo**（`/api/info`）主要字段：

| 字段 | 类型 | 说明 |
|---|---|---|
| `name` / `device_id` | string | 设备名 / 设备 ID |
| `version` | string | 内核版本 |
| `ip` / `prefix_len` / `gateway` | ipv4 | 本机虚拟 IP、掩码长度、网关 |
| `status` | VntStatus | 实例状态 |
| `current_config_name` / `current_config_file` | string? | 当前运行所用配置 |
| `online_client_num` / `offline_client_num` / `direct_client_num` | int | 在线/离线/直连对端计数 |
| `server_info` | `[{server, connected, server_rtt?, server_version?}]` | 各服务器连接状态 |
| `nat_type` | string? | NAT 类型（Debug 格式串） |
| `public_ipv4s` / `public_ipv6` | ipv4[] / ipv6? | 公网地址（STUN 探测） |
| `network_code` | string? | 组网编号 |
| `mtu` / `fec` / `compress` / `rtx` / `encrypt` | int?/bool? | 关键开关；`encrypt` = 是否设置了组网密码 |
| `allow_ikev2` / `allow_wireguard` | bool | 服务端协议互通开关 |
| `tunnel_listen_addrs` | `[{protocol, address}]` | P2P 隧道监听地址 |
| `input` / `output` / `automatic_input` | 子网列表 | 本机宣告子网 / 出口子网 / 自动探测的本地子网 |
| `config_changed` | bool | 启动后配置文件是否被修改（与启动时快照对比） |

**HttpClientItem**（`/api/peers` 元素）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `ip` | ipv4 | 对端虚拟 IP |
| `name` | string? | 对端设备名 |
| `online` | bool | 是否在线 |
| `route` | HttpRouteDetail? | 当前生效路由（含 `mode` 连接模型） |
| `version` / `client_type` | string | 对端内核版本 / 客户端类型 |
| `last_connected_time` | i64 | 最近连接时间戳 |
| `key_equal` | i32 | 密码一致性标记 |
| `nat_info` | `{nat_type, public_ips, ipv6?}`? | 对端 NAT 信息 |
| `packet_loss` | `{sent, received, loss_rate}`? | 收发与丢包率 |
| `traffic` | `{tx_bytes, rx_bytes}`? | 服务器中继口径流量（节点侧端到端流量见 vnts2 平台） |
| `advertised_subnets` | 子网列表 | 对端宣告的子网 |

**HttpRouteDetail**：`{addr, protocol, metric, rtt, loss_rate, mode}`；`mode` 为连接模型：`lan` / `udp-v4` / `udp-v6` / `tcp-v4` / `tcp-v6` / `relay`（与内核 `LinkModel` 口径一致）。

## 四、配置文件管理

| 方法 | 路径 | 参数 | 说明 |
|---|---|---|---|
| GET | `/api/config/list` | 无 | 已保存配置摘要 `[{file_name, config_name}]` |
| GET | `/api/config` | query `file_name` | 读配置文件 TOML 文本 |
| POST | `/api/config` | body `{file_name?, config}` | 保存配置。`file_name` 省略时按时间戳生成新文件。校验：非订阅配置走完整 `StartConfig::validate`；含 `subscription` 的只校验 TOML 与链接格式（远端拉取留给启动流程）。切换订阅时清除该文件的订阅会话状态 |
| DELETE | `/api/config` | query `file_name` | 删除配置文件（实例运行中或非 `Stopped` 时拒绝） |

## 五、订阅模式（vnts2 托管配置）

| 方法 | 路径 | 参数 | 说明 |
|---|---|---|---|
| POST | `/api/subscription/preview` | body `{subscription}` | 解析订阅链接并拉取，返回生效配置预览：`{server[], cert_mode, network_code, device_id, revision, device_name?, ip?, config}`（`config` 为合并服务端下发后的 TOML）。**只读远端，不落盘、不启动**，供「本地将保存」预览 |
| GET | `/api/subscription/status` | query `file_name` | 订阅会话状态，见下方 `SubscriptionStatusResponse` |
| POST | `/api/subscription/overrides/clear` | body `{file_name, ...}` | 清除本地覆盖项，恢复为服务端下发的原始配置 |
| POST | `/api/subscription/detach` | body `{file_name}` | 解除订阅，转为普通本地配置 |

**SubscriptionStatusResponse**（`/api/subscription/status`）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `file_name` / `network_code` / `device_id` | string | 订阅归属标识 |
| `target_revision` / `acknowledged_revision` / `applied_revision` | u64 | 服务端目标 / 已确认 / 已应用修订号（三者差值即推送滞留情况） |
| `local_overrides` | string[] | 本地覆盖的字段名列表 |
| `failed_revision` / `last_error` | u64? / string? | 最近应用失败的修订与原因 |
| `config_sync_verified` | bool | 运行配置是否已验证与服务端修订一致 |

## 附：与 vnts2 管理 API 的区别

`vnt2_web` 是**节点本地**的单机管理面（操作本机实例）；vnts2 平台的 Web API 是**网络级**管理面（设备/拓扑/订阅签发，`{code:200}` + JWT）。两套接口互不通用，排障时先确认打的是哪一侧。
