use crate::context::nat::MyNatInfo;
use crate::protocol::control_message::P2pStatusReportMsg;
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

/// 组装上报消息：direct 对端集合 + 本机 NAT 是否锥形（未探测按 false）。
/// 空列表照常上报——服务端为全量覆盖语义，空列表即清边
/// （不沿用 v1 up_status 的空跳过，那会导致对端全部断开后旧边残留）。
pub(crate) fn build_p2p_status_report(
    table: &RouteTable,
    nat_info: &MyNatInfo,
) -> P2pStatusReportMsg {
    P2pStatusReportMsg {
        peers: direct_peer_destinations(table),
        nat_cone: nat_info
            .get()
            .is_some_and(|info| info.nat_type == NatType::Cone),
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
    use crate::protocol::control_message::P2pStatusReportMsg;
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

    // T9: 上报包 NetPacket 头正确（msg_type=27、gateway_flag、ttl=1）+ payload 可解码
    #[test]
    fn report_packet_encoding() {
        let msg = P2pStatusReportMsg {
            peers: vec![Ipv4Addr::new(10, 26, 0, 2), Ipv4Addr::new(10, 26, 0, 3)],
            nat_cone: true,
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
