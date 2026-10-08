use crate::context::nat::MyNatInfo;
use crate::protocol::control_message::{P2pLink, P2pRelayLink, P2pStatusReportMsg};
use crate::protocol::ip_packet_protocol::{HEAD_LENGTH, MsgType, NetPacket};
use crate::protocol::transmission::TransmissionBytes;
use crate::tunnel_core::p2p::route_table::{Route, RouteTable};
use rustp2p_core::nat::NatType;
use std::io;
use std::net::Ipv4Addr;

/// 目的地址口径的 P2P 直连对端集合（对齐 v1 route_table_p2p 语义）：
/// 某目的地址存在 metric==1（is_direct）路由即视为直连对端；
/// 仅经其转发的 relay 目的、以及中转下一跳本身不混入。
pub(crate) fn direct_peer_destinations(table: &RouteTable) -> Vec<Ipv4Addr> {
    let mut peers: Vec<Ipv4Addr> = table
        .route_table()
        .into_iter()
        .filter(|(_, routes)| routes.iter().any(Route::is_direct))
        .map(|(ip, _)| ip)
        .collect();
    peers.sort_unstable();
    peers.dedup();
    peers
}

/// 直连链路明细：每个直连对端取评分最高的 direct 路由，按 Route::link_model
/// 分类（lan / udp-v4 / udp-v6 / tcp-v4 / tcp-v6）并附 rtt，供拓扑展示。
pub(crate) fn direct_peer_links(table: &RouteTable) -> Vec<P2pLink> {
    let mut links: Vec<P2pLink> = table
        .route_table()
        .into_iter()
        .filter_map(|(ip, routes)| {
            routes
                .iter()
                .filter(|route| route.is_direct())
                .max_by_key(|route| route.score())
                .map(|best| P2pLink {
                    peer: ip,
                    model: best.link_model().as_str().to_string(),
                    rtt: best.rtt(),
                })
        })
        .collect();
    links.sort_unstable_by_key(|link| link.peer);
    links
}

/// 已验证节点中继路由明细（目的、中转节点、两跳实测 rtt），供拓扑展示
/// 区分节点中继与服务器中继。未验证（半开风险）与 owner 缺失（隧道已回收）
/// 的条目在 RouteTable::verified_relay_routes 内已过滤。
pub(crate) fn relay_peer_links(table: &RouteTable) -> Vec<P2pRelayLink> {
    let mut links: Vec<P2pRelayLink> = table
        .verified_relay_routes()
        .into_iter()
        .map(|(dest, via, route)| P2pRelayLink {
            dest,
            via,
            rtt: route.rtt(),
        })
        .collect();
    links.sort_unstable_by_key(|link| link.dest);
    links
}

/// 组装上报消息：直连链路明细（peers 与其严格镜像）、已验证节点中继路由、
/// 本机 NAT 是否锥形（未探测按 false）。空列表照常上报——服务端为全量覆盖
/// 语义，空列表即清边（不沿用 v1 up_status 的空跳过，那会导致对端全部
/// 断开后旧边残留）。
pub(crate) fn build_p2p_status_report(
    table: &RouteTable,
    nat_info: &MyNatInfo,
) -> P2pStatusReportMsg {
    let links = direct_peer_links(table);
    let relay_links = relay_peer_links(table);
    let peers = direct_peer_destinations(table);
    P2pStatusReportMsg {
        peers,
        nat_cone: nat_info
            .get()
            .is_some_and(|info| info.nat_type == NatType::Cone),
        links,
        relay_links,
        // Task 2 接入端到端流量快照；本任务（协议层）先空列表占位
        traffic: Vec::new(),
    }
}

/// 上报包封装：与 SubnetSyncReq 同模式（ttl=1、gateway_flag、不设 src/dest，
/// 服务器按连接会话归属设备）。
pub(crate) fn p2p_report_packet(
    msg: &P2pStatusReportMsg,
) -> io::Result<NetPacket<TransmissionBytes>> {
    let payload = msg.encode();
    let mut packet = NetPacket::new(TransmissionBytes::zeroed(HEAD_LENGTH + payload.len()))?;
    packet.set_ttl(1);
    packet.set_msg_type(MsgType::P2pStatusReport);
    packet.set_gateway_flag(true);
    packet.set_payload(&payload)?;
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::nat::MyNatInfo;
    use crate::protocol::control_message::{P2pStatusReportMsg, P2pTraffic};
    use crate::protocol::ip_packet_protocol::MsgType;
    use rustp2p_core::nat::{NatInfo, NatType};
    use rustp2p_core::route_table::{Protocol, RouteKey};
    use std::net::Ipv4Addr;

    fn nat_info(nat_type: NatType) -> NatInfo {
        let local = Ipv4Addr::new(192, 168, 1, 2);
        NatInfo {
            nat_type,
            public_ips: vec![],
            public_udp_ports: vec![],
            mapping_tcp_addr: vec![],
            mapping_udp_addr: vec![],
            public_port_range: 0,
            local_ipv4: local,
            local_ipv4s: vec![local],
            ipv6: None,
            local_udp_ports: vec![],
            local_tcp_port: 0,
            public_tcp_port: 0,
            stun_mapped_ports: vec![],
        }
    }

    // T8: 摘要只吐 direct 对端（metric==1），relay 目的与中转下一跳不混入
    #[test]
    fn direct_routes_only() {
        let table = RouteTable::new();
        let direct_peer = Ipv4Addr::new(10, 26, 0, 3);
        let relay_dest = Ipv4Addr::new(10, 26, 0, 4);
        let key = RouteKey::new(
            Protocol::UDP,
            "127.0.0.1:1991".parse().unwrap(),
            "127.0.0.1:2991".parse().unwrap(),
        );

        table.add_route(direct_peer, Route::from(key, 1, 12), false);
        table.add_gossip_relay_route(relay_dest, Route::from(key, 2, 51));

        let peers = direct_peer_destinations(&table);
        assert_eq!(peers, vec![direct_peer]);
    }

    // T9: 上报包 NetPacket 头正确（msg_type=27、gateway_flag、ttl=1）+ payload 可解码（含 links/relay_links）
    #[test]
    fn report_packet_encoding() {
        let msg = P2pStatusReportMsg {
            peers: vec![Ipv4Addr::new(10, 26, 0, 2), Ipv4Addr::new(10, 26, 0, 3)],
            nat_cone: true,
            links: vec![
                P2pLink {
                    peer: Ipv4Addr::new(10, 26, 0, 2),
                    model: "lan".to_string(),
                    rtt: 30,
                },
                P2pLink {
                    peer: Ipv4Addr::new(10, 26, 0, 3),
                    model: "tcp-v4".to_string(),
                    rtt: 12,
                },
            ],
            relay_links: vec![P2pRelayLink {
                dest: Ipv4Addr::new(10, 26, 0, 9),
                via: Ipv4Addr::new(10, 26, 0, 2),
                rtt: 55,
            }],
            // T1: 顺带锁定 u64 大值 varint 无损（端到端累计可远超 u32）
            traffic: vec![P2pTraffic {
                peer: Ipv4Addr::new(10, 26, 0, 3),
                tx_bytes: 1u64 << 40,
                rx_bytes: 5,
            }],
        };
        let packet = p2p_report_packet(&msg).unwrap();
        assert_eq!(packet.msg_type().unwrap(), MsgType::P2pStatusReport);
        assert!(packet.is_gateway());
        assert_eq!(packet.ttl(), 1);
        assert_eq!(
            P2pStatusReportMsg::from_slice(packet.payload()).unwrap(),
            msg
        );
    }

    // T1: 旧客户端 wire（字段 5 不出现）→ traffic 解码为空列表（边不显示流量）
    #[test]
    fn decode_legacy_report_without_traffic() {
        let legacy = P2pStatusReportMsg {
            peers: vec![Ipv4Addr::new(10, 26, 0, 9)],
            nat_cone: false,
            links: vec![],
            relay_links: vec![],
            traffic: vec![],
        }
        .encode();
        let msg = P2pStatusReportMsg::from_slice(&legacy).unwrap();
        assert!(msg.traffic.is_empty());
    }

    // T21: 直连链路明细——按最高分 direct 路由分类，与 peers 镜像，relay 目的不混入
    #[test]
    fn direct_peer_links_detail() {
        let table = RouteTable::new();
        let lan_peer = Ipv4Addr::new(10, 26, 0, 2);
        let punch_peer = Ipv4Addr::new(10, 26, 0, 3);
        let relay_dest = Ipv4Addr::new(10, 26, 0, 4);
        let lan_key = RouteKey::new(
            Protocol::UDP,
            "127.0.0.1:1991".parse().unwrap(),
            "192.168.1.3:5000".parse().unwrap(),
        );
        let punch_udp_key = RouteKey::new(
            Protocol::UDP,
            "127.0.0.1:1991".parse().unwrap(),
            "183.241.98.173:5000".parse().unwrap(),
        );
        let punch_tcp_key = RouteKey::new(
            Protocol::TCP,
            "127.0.0.1:1991".parse().unwrap(),
            "183.241.98.173:5000".parse().unwrap(),
        );
        let relay_key = RouteKey::new(
            Protocol::UDP,
            "127.0.0.1:1991".parse().unwrap(),
            "127.0.0.1:2991".parse().unwrap(),
        );

        table.add_route(lan_peer, Route::from(lan_key, 1, 30), false);
        // 同一对端两条 direct 路由：低 rtt 的 TCP 路由评分更高，应胜出
        table.add_route(punch_peer, Route::from(punch_udp_key, 1, 30), false);
        table.add_route(punch_peer, Route::from(punch_tcp_key, 1, 10), false);
        table.add_gossip_relay_route(relay_dest, Route::from(relay_key, 2, 51));

        let links = direct_peer_links(&table);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].peer, lan_peer);
        assert_eq!(links[0].model, "lan");
        assert_eq!(links[0].rtt, 30);
        assert_eq!(links[1].peer, punch_peer);
        assert_eq!(links[1].model, "tcp-v4");
        assert_eq!(links[1].rtt, 10);

        let msg = build_p2p_status_report(&table, &MyNatInfo::default());
        assert_eq!(msg.peers, vec![lan_peer, punch_peer]);
        assert_eq!(
            msg.links.iter().map(|l| l.peer).collect::<Vec<_>>(),
            msg.peers
        );
    }

    // T23: relay_peer_links 只报已验证的中继路由（via=物理隧道属主）；
    // 未验证的公告路由（半开风险）与 owner 缺失（隧道已回收）的条目不报
    #[test]
    fn relay_links_report_only_verified_routes() {
        let table = RouteTable::new();
        let relay = Ipv4Addr::new(10, 26, 0, 2);
        let unverified_dest = Ipv4Addr::new(10, 26, 0, 4);
        let verified_dest = Ipv4Addr::new(10, 26, 0, 5);
        let orphan_dest = Ipv4Addr::new(10, 26, 0, 6);
        let key = RouteKey::new(
            Protocol::UDP,
            "127.0.0.1:1991".parse().unwrap(),
            "10.0.0.2:5000".parse().unwrap(),
        );
        let orphan_key = RouteKey::new(
            Protocol::UDP,
            "127.0.0.1:1991".parse().unwrap(),
            "10.0.0.3:5000".parse().unwrap(),
        );

        // relay 节点是 key 的物理属主
        table.add_owner_route(relay, key);
        // 未验证：仅公告学到的 metric-2 路由（rtt=DEFAULT_RTT）
        table.add_gossip_relay_route(unverified_dest, Route::from_default_rt(key, 2));
        // 已验证：Pong 实测过 rtt 的 metric-2 路由
        table.add_gossip_relay_route(verified_dest, Route::from(key, 2, 55));
        // owner 缺失：orphan_key 无属主登记（隧道已回收）
        table.add_gossip_relay_route(orphan_dest, Route::from(orphan_key, 2, 33));

        let links = relay_peer_links(&table);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].dest, verified_dest);
        assert_eq!(links[0].via, relay);
        assert_eq!(links[0].rtt, 55);
    }

    // T22: 空 links 编码即旧客户端 wire 格式（字段 3 不出现），解码回退 links 为空
    #[test]
    fn decode_legacy_report_without_links() {
        let legacy = P2pStatusReportMsg {
            peers: vec![Ipv4Addr::new(10, 26, 0, 9)],
            nat_cone: false,
            links: vec![],
            relay_links: vec![],
            traffic: vec![],
        }
        .encode();
        let msg = P2pStatusReportMsg::from_slice(&legacy).unwrap();
        assert_eq!(msg.peers, vec![Ipv4Addr::new(10, 26, 0, 9)]);
        assert!(msg.links.is_empty());
    }

    // T10: NAT 锥形/对称状态正确进消息；未探测（None）按 false
    #[test]
    fn nat_cone_flag_propagates() {
        let table = RouteTable::new();

        let nat_state = MyNatInfo::default();
        nat_state.replace_nat_info(nat_info(NatType::Cone));
        assert!(build_p2p_status_report(&table, &nat_state).nat_cone);

        nat_state.replace_nat_info(nat_info(NatType::Symmetric));
        assert!(!build_p2p_status_report(&table, &nat_state).nat_cone);

        let undetected = MyNatInfo::default();
        assert!(!build_p2p_status_report(&table, &undetected).nat_cone);
    }
}
