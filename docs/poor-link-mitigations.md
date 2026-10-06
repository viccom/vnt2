# vnt2 弱网链路对抗机制（代码级参考）

> 适用于 vnt-core 2.0.10（`enhanced` 分支，2026-10-06）。
> 本文所有机制均以源码函数/常量为锚（引用形如 `文件 · 函数`，不写行号以免漂移）。
> 配置键与 CLI 参数、桌面端开关、平台信封字段的对应关系见 §9。

弱网对抗分四层：**恢复**（把丢的包救回来：FEC、rtx）→ **择路**（换条好路走：评分自动选路、IPv6/多传输）→ **兜底**（直连不行走中继：服务器中继、节点中继、turn 指定）→ **调参**（链路本身：MTU、压缩）。择路底座（§2）始终在工作；其余手段按需开启。

---

## 1. 路径度量与自动择路（底座）

### 1.1 路由评分公式

`src/tunnel_core/p2p/route_table.rs · get_channel_score`：

```
score = weight × (10000 − loss) × 100 / (rtt × (10000 + loss × 10))
weight = 120（直连）| 125（直连 TCP）| 100（中继）| 105（中继 TCP）
loss = 丢包率万分比（上限 10000）；随后再除以 metric（中继跳数，直连=1）
```

要点（与直觉对照）：

- **丢包是压分主项**：20% 丢包使分子 ×0.8、分母 ×3.0，评分约为无损时的 1/3.75。
- **TCP 权重 +5 而非惩罚**：RTT/丢包相当时优先 TCP（稳定性偏好）；UDP 只有在质量明显更好时才胜出。
- **中继基础权重 100 低于直连 120**，但除以 metric：metric-2 中继评分减半。无损中继仍能压过 20% 丢包的直连——这是"直连劣化自动走中继"的数学基础。

算例：

| 路由 | rtt | 丢包 | 评分 |
|---|---|---|---|
| 直连 UDP | 18ms | 20% | ≈ 178 |
| 直连 TCP | 20ms | 0 | 625 |
| 中继 metric-2 | 30ms | 0 | ≈ 167 |

### 1.2 探测与回填

`src/tunnel_core/p2p/transport/task.rs · ping_all`：每 5s 一轮，对每个目的按评分降序取前 N 条路由探测（备用路由由探测上限保活），`PacketLossStats` 按序号记账 sent/received，得到万分比丢包率，经 `Route::from_with_loss` 回填评分。探测结果经 `/api/peers` 的 `route.rtt / route.loss_rate / packet_loss` 暴露。

### 1.3 已验证路由与半开防护

`route_table.rs · is_verified`：`rtt != DEFAULT_RTT`，即只有收到 Pong 实测回程的路由才算"已验证"。打洞/握手注册的路由携带 DEFAULT_RTT，可能是单向半开（对端 NAT 只进不出），承载流量会黑洞。因此**未实测直连让位于已验证的中继/服务器路由**（route_table 路径选择与 8aaf66e 语义），实测通过后才按评分竞争。

### 1.4 不可达负缓存

`RouteState.unreachable_paths`：以（目的，下一跳）为粒度的临时不可达记录，`DestinationUnreachable` 通知严格一跳（防畸形/过期通知递归）。命中抑制的路由在探测恢复前不参与择路。

### 1.5 多路由并存

`route_table: HashMap<Ipv4Addr, Vec<Route>>`：同一目的可同时持有 UDP、TCP、多 NAT 映射的多条路由，`try_send_raw` 按评分择优（可排除入境路由防回环）。链路质量波动时数据面自动在路由间迁移，无需配置。

---

## 2. FEC 前向纠错（`fec`）

### 2.1 编码（`src/fec/encoder.rs`）

- 算法：Reed-Solomon 擦除码（`reed-solomon-erasure` crate）。
- 常量：`BATCH_SIZE=10`、`REDUNDANCY_RATE=0.2`、`MIN_PARITY=1`、`BATCH_TIMEOUT_MS=20`、`BATCH_CHANNEL_SIZE=1024`、单组 shard 上限 `MAX_NUM=50`（decoder）。
- 按目的分批（`DestBatchState`）：**数据包由调用方立即发送（零首包延迟）**；批满 10 包或距首包 20ms 截止后，由 worker 生成校验包异步发送。空闲后的首包重新起 20ms 窗口（worker 5ms tick 扫描截止时间，空闲期不吞新批窗口）。
- 校验通道满时丢弃整批校验并 WARN，**不阻塞数据面**（数据早已发出）。

### 2.2 线路格式与安全

FEC 外层包使用 `MsgType::Quic(17)` 标记；内层是**已完成 AEAD 加密（或 QUIC 加密）的完整 NetPacket** + 2 字节长度前缀（用于裁剪 RS 等长 shard 的尾部填充）。外层只做认证、不做加密（`authenticate_fec_in_place` / `verify_fec_in_place`）——未经认证的 shard 直接丢弃，不能创建或污染 FEC 组。安全性与普通包等价（内层密文）。

### 2.3 解码（`src/fec/decoder.rs`）

解码器**无条件常驻**（`src/core/mod.rs`），与发送端开关无关。按（来源 IP，group_id）维护组状态：任意 shard 先过外层认证；数据齐即交付、校验齐则 RS 恢复缺失 shard（按长度前缀裁剪）后交付。组完成即清理；带组号重合防护（发送端重启后组号与旧完成组重合时，旧组超时后不再拦截新包）。

### 2.4 方向性与中继穿越

`fec` 开关只门控**本端发送方向**（`change_runtime.rs · apply_policy_change` 中 `config.fec.then_some(fec_encoder)`），接收端无需开启。中继转发发生在 FEC 解码**之前**（`p2p/inbound.rs` "帮忙转发"分支先于 `is_fec()` 处理），FEC 外层包被中继节点原样转发；服务器 Turn 中继同样不触碰 payload——**经中继的两跳路径同样受源端 FEC 保护**。

### 2.5 与 rtx 的叠加

编码发生在 QUIC 加密之后（内层可以是 QUIC 密文），FEC 与 rtx 可同时启用，互不冲突。

### 2.6 效果与边界

- 恢复数学（i.i.d. 随机丢包 p）：批 10+2 校验时组内丢 >2 不可恢复，p=20% 时残余约 5-7%；稀疏流量（如 1s 一次的 ping）单包成组 +1 校验，数据与校验**同时**丢才失败，p=20% 时残余约 4%。
- **突发相关丢包打折**：短于 20ms 窗口的连发丢可能同时命中数据与校验。
- 带宽代价：批量流量 +20%；稀疏小包流量 1:1（绝对量小）。
- 实测锚点（2026-10-06，u18↔ncpe 跨网线路，裸链路 23% 随机丢）：隧道丢包 25%→13%，线路指纹为「数据包 + 20ms 后 +10B 校验包」成对出现。

---

## 3. QUIC 优化传输（`rtx`）

### 3.1 架构（`src/enhanced_tunnel/quic_over/`）

- quinn Endpoint 挂 `ExtendedQuicSocket`——QUIC 包经 vnt 隧道基础设施收发（P2P 隧道或服务器中继均可承载），因此 QUIC 通道端到端跨节点。
- 拥塞控制 BBR；keepalive 5s / 空闲超时 10s；TLS 证书由 password 确定性生成 + 指纹校验（`FingerprintVerifier`），无 CA 依赖。
- 端点与 IpStack **常驻启动**（`quic_over/boot.rs · quic_tunnel_start`），rtx 开关只决定 TUN 出站新流量是否进入栈（`EnhancedQuicOutbound::outbound` 中 `policy.load().rtx` 门控），已建连接的回包不受开关影响——**热可逆**（开关信封/本地变更即时生效，无需重启）。

### 3.2 TCP 语义

IpStack（用户态 TCP/IP 栈）代理 TCP 连接：新建连接的 SYN 进入栈，由栈对端（对端节点的 IpStack）完成代理；已存在的非栈内连接交回普通转发路径（`has_tcp_connection` 判断，解析失败不静默吞包）。应用层 TCP 看到的是零丢包链路——丢包由 QUIC 重传掩盖。

### 3.3 UDP/ICMP 语义

同样进入 IpStack 经 QUIC 承载（可靠流）：ping 的丢包会变成 0，但被恢复的包呈 +重传延迟（表现为抖动上升）。**实时流量不建议依赖 rtx 抗丢包，那是 FEC 的职责。**

### 3.4 服务器中继路径

QUIC 内层包标记 `MsgType::Quic(17)`，服务端入站对该类型免普通 AEAD 解密、直接进 enhanced 通道分派（`tunnel_core/server/inbound.rs · process_inner_packet`）——无直连时 rtx 依然端到端可用。

### 3.5 效果与代价

- **收益**：高丢包线路上 TCP 吞吐大幅恢复（裸 TCP 在 20% 丢包下吞吐塌方，QUIC 流承载后由重传+BBR 兜住）；web/文件/远程桌面体感改善最明显。
- **代价**：QUIC 流队头阻塞（一条流重传会拖慢同连接其他数据）；实时流量延迟方差上升；IpStack 代理是较新的组件，故障面较宽。

---

## 4. 打洞与多传输

### 4.1 打洞模型集

双方经服务器交换 `PunchInfo`（NAT 类型、公网映射、本地地址、IPv6）与各自的 punch_model 集合（IPv4Udp / IPv6Udp / IPv4Tcp / IPv6Tcp），按交集并发打洞。**双栈环境优先尝试 IPv6**——IPv6 常绕开 IPv4 跨网互联拥塞（单端无 IPv6 则自动不可用）。

### 4.2 `punch_model` 按目标限制

`target,mode[,mode...]` 逐目标限制允许的打洞方式子集，用于排除注定失败的方式、收敛无效打洞循环。当前打洞为轮内 50ms 并发、周期性重试，**尚无指数退避**（已知优化项）。

### 4.3 `peer_address` 静态直连

`ip:端口`、`tcp://`、`udp://`、`dynamic://`（DNS TXT / HTTP(S) 地址列表）——对有公网地址或端口映射的固定节点钉死直连路径，绕过打洞不确定性。

### 4.4 STUN

`udp_stun` / `tcp_stun` 列表指定 NAT 检测与公网映射发现服务（内置默认含多家，可替换为自建以提高可用性）。

### 4.5 防御

对端地址为本机接口地址的隧道建立请求拒绝（NAT 反射包防护）；未验证路由不抢占数据面（见 §1.3）。

---

## 5. 中继体系

### 5.1 服务器中继

`MsgType::Turn` 经 vnts2 转发（TTL 递减、目的查 sender_map、多服务器联邦间 `forward_with_best_route`）。服务端只见密文（启用加密时）。直连不可用时的默认兜底。

### 5.2 节点中继（gossip 两跳）

`NodeAnnouncement` 在 P2P 隧道间泛洪（不经服务器），学习 metric-2 中继路由（只信 source_metric==1 的宣告）；经 Ping/Pong TTL 实测验证后参与评分竞争。客户端 30s 周期上报 `relay_links`（仅已验证路由），供拓扑展示"节点中继(经 via)"。

### 5.3 `turn` 指定中转

`target,turn_ip`：对指定目标网段优先经指定节点中转。用于已知某节点路径更优（如双 ISP / 就近机房节点）、或固定绕开劣化段。

---

## 6. 链路参数

### 6.1 MTU

核心下限 `MIN_MTU=1280`（`src/core/mod.rs`，低于该值的快照在应用时被拒绝）。PPPoE（1492）、多层隧道叠加场景适当调小可避免分片丢包；无线链路小包误码率也更低。桌面平台支持热更新（tun `set_mtu`）；移动平台 MTU 由宿主 VPN 接口决定。

### 6.2 LZ4 压缩（`compress`）

`src/compression/lz4_compression.rs`：payload ≥ 256B 才尝试；压缩后不小于原文则放弃（零收益零开销）。**省带宽不抗丢包**，适合窄上行 + 可压缩流量（文本、RDP）；视频/已压缩数据自动直通。

---

## 7. 可观测性

- `/api/peers`：每对端 `route.rtt / route.loss_rate（万分比）/ metric / mode` 与 `packet_loss.sent / received / loss_rate`（探测记账）。
- `/api/info`：`fec / rtx / compress / encrypt` 生效状态（2026-10-06 起热应用后实时正确，见提交 a986913）。
- vnts2 拓扑页：直连 / 打洞直连（按协议族细分）/ 节点中继(经 via) / 服务器中继兜底 四类边。
- 日志关键字：`打洞成功`、`drop tunnel`、`tcp punch error`、`订阅配置更新到达 revision=N`、`已应用策略类配置变更`。

---

## 8. 选型指南

| 症状 | 首选 | 次选 | 说明 |
|---|---|---|---|
| 随机逐包丢、RTT 稳（无线/跨网拥塞） | `fec`（实时流量） | `rtx`（TCP 吞吐） | FEC 抗随机丢最有效；突发丢打折 |
| UDP 高丢、TCP 正常（UDP 被 QoS） | TCP 隧道自动接管 | `punch_model` 限 UDP | 评分含丢包项，自动切换 |
| 打洞半开、直连时断时续 | 中继自动兜底（内置） | `turn` 固定中转 | 未验证直连不抢占已验证中继 |
| 晚高峰 IPv4 跨网拥塞 | IPv6（双端支持时） | 多服务器 / `turn` 绕行 | IPv6 常绕开 IPv4 互联拥塞 |
| 上行带宽窄 | `compress` | MTU 调优 | 高熵流量无收益 |
| 分片导致的丢包（PPPoE/叠加隧道） | MTU 调小 | — | 核心下限 1280 |
| 打洞循环噪声大 | `punch_model` 收敛 | `peer_address` 钉死 | 当前无退避机制 |

**组合**：FEC 与 rtx 可叠加（FEC 外层包 QUIC 内层）；随机丢包线路先 FEC，TCP 吞吐仍不满意再加 rtx。压缩与两者正交。

**生效方式**（信封/本地配置变更后）：

- 策略类**热应用**（无需重启，秒级生效）：`fec`、`rtx`、`compress`、`turn`、`punch_model`、`peer_address`、`udp_stun`、`tcp_stun`、`no_punch`、`no_nat`、`auto_sync_subnet` 等。
- 重建类（组网实例内部重建，订阅连接不断，隧道闪断数秒）：`password`、`cert_mode`、`outbound_interface`、`tunnel_addr/port`、`device_mode`、`port_mapping`、`subnet_mapping`、`output`。
- 注意：`password`（加密）为 P2P 包加密密码，**需全网一致**方可互通，建议经平台网络级统一下发，勿单节点单独变更。

## 9. 配置入口与优先级

三个入口写的是同一份配置键：

1. **平台信封**（vnts2 设备配置，高级表单三态）：`开/关` 写入 TOML 键随信封下发；`未设置` 则键不出现在信封中。**信封中出现的键在运行期覆盖客户端本地值**（`merge_present_config`，按出现键逐字段覆盖；身份/虚拟 IP/订阅始终服务端所有）。
2. **本地 TOML**（`fec = true` 等顶层键）：可写全部字段；**无文件监听，改动需重启实例生效**。信封未管理的键（平台三态"未设置"）本地值稳定生效；信封管理的键以平台为准。
3. **CLI 参数**（`--fec` / `--rtx` / `-z` / `-p` 等）：与 TOML 键一一对应，启动时生效。

生效即时性：热应用类经信封下发**秒级生效**且 `/api/info` 同步反映（a986913 修复后）；本地文件修改一律重启后生效。
