//! 遥测渲染函数测试（计划 §4 Task 4；先红后绿）。
//!
//! 用 serde_json 构造最小 HttpAppInfo / HttpClientItem / HttpRouteItem
//! （镜像 vnt-web/src/service_http.rs:833/881/917 的序列化形态，
//! VntStatus 为小写，service_http.rs:49 `rename_all = "lowercase"`），
//! 直接喂给 telemetry 的渲染函数，不经 HTTP。

use vnt_webctl::telemetry;

/// 最小 Running 态 HttpAppInfo（全字段齐备）。
fn running_info() -> serde_json::Value {
    serde_json::json!({
        "name": "u18",
        "version": "1.2.13",
        "ip": "10.26.0.2",
        "prefix_len": 24,
        "gateway": "10.26.0.1",
        "device_id": "dev-abc",
        "status": "running",
        "current_config_name": "dongnet",
        "current_config_file": "dongnet.toml",
        "online_client_num": 3,
        "offline_client_num": 1,
        "direct_client_num": 2,
        "server_info": [
            {"server": "vnts2.metme.top:29872", "connected": true,
             "server_rtt": 12, "server_version": "2.0.10"}
        ],
        "nat_type": "FullCone",
        "public_ipv6": null,
        "public_ipv4s": ["183.241.98.173"],
        "network_code": "dongnet",
        "mtu": 1400,
        "fec": false,
        "compress": true,
        "encrypt": true,
        "rtx": false,
        "allow_ikev2": true,
        "allow_wireguard": false,
        "tunnel_listen_addrs": [],
        "input": [],
        "output": ["10.99.0.0/24"],
        "automatic_input": ["192.168.1.0/24"],
        "config_changed": false
    })
}

/// Stopped 态 HttpAppInfo：Option 字段全部 null（复核钉死）。
fn stopped_info() -> serde_json::Value {
    serde_json::json!({
        "name": "u18",
        "version": "1.2.13",
        "ip": null,
        "prefix_len": null,
        "gateway": null,
        "device_id": "dev-abc",
        "status": "stopped",
        "current_config_name": "dongnet",
        "current_config_file": "dongnet.toml",
        "online_client_num": 0,
        "offline_client_num": 0,
        "direct_client_num": 0,
        "server_info": [],
        "nat_type": null,
        "public_ipv6": null,
        "public_ipv4s": [],
        "network_code": "dongnet",
        "mtu": null,
        "fec": null,
        "compress": null,
        "encrypt": null,
        "rtx": null,
        "allow_ikev2": true,
        "allow_wireguard": false,
        "tunnel_listen_addrs": [],
        "input": [],
        "output": [],
        "automatic_input": [],
        "config_changed": false
    })
}

#[test]
fn info_stopped_renders_not_running_only() {
    let out = telemetry::render_info("foo", &stopped_info());
    // 空态只打「未运行」+ status，不打分节空表（复核钉死）
    assert!(out.contains("实例 foo 未运行"), "应含未运行提示: {out:?}");
    assert!(out.contains("stopped"), "应带状态: {out:?}");
    assert!(
        !out.contains("虚拟 IP") && !out.contains("服务器"),
        "Stopped 不打分节表: {out:?}"
    );
}

#[test]
fn info_running_renders_all_sections() {
    let out = telemetry::render_info("foo", &running_info());
    // 身份
    assert!(out.contains("u18"), "设备名: {out:?}");
    assert!(out.contains("dev-abc"), "设备 ID: {out:?}");
    assert!(out.contains("running"), "状态: {out:?}");
    // 网络
    assert!(out.contains("10.26.0.2"), "虚拟 IP: {out:?}");
    assert!(out.contains("10.26.0.1"), "网关: {out:?}");
    assert!(out.contains("FullCone"), "NAT 类型: {out:?}");
    assert!(out.contains("1400"), "MTU: {out:?}");
    assert!(out.contains("dongnet"), "组网编号: {out:?}");
    // 服务器
    assert!(out.contains("vnts2.metme.top:29872"), "服务器地址: {out:?}");
    assert!(out.contains("12"), "RTT: {out:?}");
    // 对端计数
    assert!(out.contains('3'), "在线数: {out:?}");
    assert!(out.contains('2'), "直连数: {out:?}");
    // 子网
    assert!(out.contains("10.99.0.0/24"), "出口子网: {out:?}");
    assert!(out.contains("192.168.1.0/24"), "自动子网: {out:?}");
}

#[test]
fn info_running_none_fields_render_dash() {
    // mtu 为 null 时打 "-"（opt 语义），不打 "null" 或空串
    let mut info = running_info();
    info["mtu"] = serde_json::Value::Null;
    let out = telemetry::render_info("foo", &info);
    assert!(!out.contains("null"), "不得出现字面 null: {out:?}");
    assert!(out.contains('-'), "None 字段须打 -: {out:?}");
}

// ---- peers ----

#[test]
fn peers_empty_renders_hint() {
    let out = telemetry::render_peers("foo", &serde_json::json!([]));
    assert!(out.contains("无对端"), "空列表空态文案: {out:?}");
}

#[test]
fn peers_renders_table_rows() {
    let peers = serde_json::json!([
        {
            "ip": "10.26.0.3",
            "name": "ncpe",
            "online": true,
            "route": {"addr": "183.241.98.173:1000", "protocol": "UDP",
                      "metric": 1, "rtt": 12, "loss_rate": 0, "mode": "udp-v4"},
            "version": "1.2.13",
            "client_type": "u18",
            "last_connected_time": 0,
            "key_equal": 1,
            "nat_info": null,
            "packet_loss": {"sent": 100, "received": 98, "loss_rate": 0.02},
            "traffic": {"tx_bytes": 1536, "rx_bytes": 2048},
            "advertised_subnets": []
        },
        {
            "ip": "10.26.0.4",
            "name": null,
            "online": false,
            "route": null,
            "version": "1.2.12",
            "client_type": "u18",
            "last_connected_time": 0,
            "key_equal": 1,
            "nat_info": null,
            "packet_loss": null,
            "traffic": null,
            "advertised_subnets": []
        }
    ]);
    let out = telemetry::render_peers("foo", &peers);
    assert!(out.contains("10.26.0.3"), "对端 IP: {out:?}");
    assert!(out.contains("ncpe"), "对端名: {out:?}");
    assert!(out.contains("打洞直连"), "mode 映射: {out:?}");
    assert!(out.contains("1.5 KB"), "tx 换算: {out:?}");
    assert!(out.contains("2.0 KB"), "rx 换算: {out:?}");
    // 离线且全 null 的行：mode/tx/rx 打 "-"
    assert!(out.contains("10.26.0.4"), "离线行: {out:?}");
    assert!(!out.contains("null"), "不得出现字面 null: {out:?}");
}

// ---- routes ----

#[test]
fn routes_empty_renders_hint() {
    let out = telemetry::render_routes("foo", &serde_json::json!([]));
    assert!(out.contains("无路由"), "空路由空态文案: {out:?}");
}

#[test]
fn routes_grouped_by_ip() {
    let routes = serde_json::json!([
        {
            "ip": "10.26.0.3",
            "routes": [
                {"addr": "183.241.98.173:1000", "protocol": "UDP",
                 "metric": 1, "rtt": 12, "loss_rate": 0, "mode": "udp-v4"},
                {"addr": "vnts2.metme.top:29872", "protocol": "WS",
                 "metric": 2, "rtt": 30, "loss_rate": 0, "mode": "relay"}
            ]
        }
    ]);
    let out = telemetry::render_routes("foo", &routes);
    assert!(out.contains("10.26.0.3"), "分组 IP: {out:?}");
    assert!(out.contains("183.241.98.173:1000"), "路由地址: {out:?}");
    assert!(out.contains("打洞直连"), "打洞 mode: {out:?}");
    assert!(out.contains("服务器中继"), "中继 mode: {out:?}");
    assert!(out.contains("12"), "RTT: {out:?}");
}
