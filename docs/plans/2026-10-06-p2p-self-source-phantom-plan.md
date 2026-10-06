# P2P 自源幻影隧道拒绝（打洞环路安全网）

- 日期：2026-10-06
- 状态：实施中
- 仓库：vnt2（vnt-core），vnts2 无改动
- 关联：vnts2-dev `docs/plans/2026-10-04-p2p-topology-plan.md`（路由/打洞体系的上游背景）

## 1. 现象

r5900（Windows，移动宽带，CGNAT）持续 ping ncpe（电信专线，Cone NAT），
每 10~100 秒出现一次丢包窗口。实测复现：0.3s 间隔连续 ping 300 包丢 66 包
（34%），全部集中在一个连续 19.8 秒的窗口内。

## 2. 根因链（全部有日志/运行时证据）

1. r5900 与 ncpe 之间存在另一套 p2p 软件的站点间隧道（ncpe 侧 tun0=
   192.168.123.1/24 + `MASQUERADE` iptables 规则），r5900 因此拥有到
   10.83.0.0/16（ncpe 内网）的路由。
2. r5900 的 vnt 打洞同时尝试 ncpe 的多个候选地址。发往 10.83.40.196（ncpe
   内网地址）的打洞包经该隧道转发，在 ncpe 侧被 `MASQUERADE` 改写源地址为
   ncpe 本机出口 IP——ncpe 收到"源地址是自己"的打洞包（日志反复出现
   `peer_addr: 10.83.40.196:36634`）。
3. PunchReq 处理器只做了 `network_contains`（虚拟网段）环路防护，
   10.83.40.196 不在 10.99.0.0/24 内 → 幻影隧道注册、PunchRes 回复、
   owner 路由以 metric=1 置顶。
4. 该路径数据面不可用（conntrack 反向仅对 punch 交换那一条流成立，后续
   数据新建的流不匹配隧道对端），打洞重传又不断刷新 owner 路由 → 真实
   流量整窗黑洞，直至隧道 20s 读空闲超时回收（`TUNNEL_READ_TIMEOUT`，
   task.rs）→ 路由过期回退中继 → 下一次打洞再命中迷宫路径，循环。
5. 佐证：u18↔ncpe 无此问题——u18 是 r5900 内的 Hyper-V 虚拟机，没有到
   10.83.x 的路由，唯一路径是 ncpe 公网映射 `udp://124.65.218.86:48114`，
   实测 ping/pong 保活 798 发/798 收、零丢包、rtt 17ms 连续 66 分钟。
   **vnt 的保活机制（ping_all 5s + route TTL 10s）工作正常，无需新增心跳。**

## 3. 方案

把既有 `network_contains` 环路防护扩展到**本机接口地址**：隧道分发入口
（transport/task.rs 的 tunnel_dispatch loop）对每个新隧道校验对端 IP，
命中本机 IPv4（NatInfo.local_ipv4 / local_ipv4s）则拒绝建立并告警。

选择单一分发入口而非 PunchReq/PunchRes 处理器内各自校验：所有隧道
（打洞/直连/其他）都经此创建，包在进入处理器之前就被丢弃——不会注册
路由、不会回复 PunchRes、对端打洞自然超时并落到真实公网路径。

判定谓词（可单测）：

```rust
fn is_self_sourced_tunnel(nat_info: &MyNatInfo, route_key: &RouteKey) -> bool
```

- 仅 IPv4；loopback 显式放行（同机多实例经 127.0.0.1 peer_address 互联是
  合法用法，且现有 loopback 测试依赖之）
- NatInfo 尚未就绪（启动初期 NAT 探测未完成）时跳过校验（fail-open，
  20s 读超时仍兜底）

## 4. 明确不做（及原因，偏离记录）

- **不新增隧道心跳**：实测保活链路（ping_all→Pong→add_probe_route 刷新）
  在公网直连路径上完全正常（798/798）。此前"20s 空闲回收=缺心跳"是误判
  ——被回收的隧道本就是无回包的坏路径（幻影/防火墙拦截），回收是正确行为。
- **不做打洞成功前双向验证**：本次幻影在 punch 交换阶段是双向可通的
  （conntrack 反向），双向验证拦不住它；且会破坏新旧版本互操作。
- **不改 TUNNEL_READ_TIMEOUT**：20s 对"坏路径快速回退"是合理值。

## 5. 预期效果

- ncpe 拒绝迷宫路径 → r5900 打洞落到 ncpe 公网映射 → 建立真直连
  （与 u18↔ncpe 同类，预计 rtt ~17-25ms、零丢包），由既有 ping/pong 保活。
- u18↔ncpe 公网直连不受影响（源地址非本机）。
- 同机多实例经 loopback 互联不受影响。

## 6. 测试

1. 谓词单测（task.rs tests）：本机 IP → true；对端 IP → false；loopback →
   false；NatInfo None → false。
2. 既有 4 节点 loopback 测试回归（127.0.0.1 peer_address 路径不受影响）。
3. `cargo test --lib` 全量 + clippy 0 警告 + fmt。

## 7. 部署与验证标准

- 部署：ncpe（systemd）、u18（本机）。r5900 为 Windows 桌面版，出构建
  产物由用户择机更新（ncpe 侧拒绝已足以切断环路，r5900 不更新也能恢复）。
- 验证：
  - ncpe journal 出现"忽略本机地址隧道"告警（r5900 打洞重试期间）
  - ncpe peers API 中 r5900 出现真实公网路由（183.241.98.173 端口）
    且 loss_rate=0
  - ncpe↔r5900 连续 ping ≥5 分钟零丢包
  - u18↔ncpe 维持 798/798 式零丢包直连

## 8. 已知取舍

同机多 vnt 实例共享同一出口 IP 时，实例间 P2P 直连会被本检查降级为中继
（源 IP 均为本机接口 IP）。该拓扑下本就应使用 loopback/内网地址互联，
降级可接受。
