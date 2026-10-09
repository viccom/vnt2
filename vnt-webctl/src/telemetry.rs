//! 遥测命令：info / peers / routes（计划 §4 Task 4）。
//!
//! 「复核钉死」的 Option 语义：
//! - HttpAppInfo 的 Option 字段在实例 Stopped 时全为 null——info 对
//!   非 running 状态只打「未运行」+ status，不打分节空表
//! - peers/routes 在实例未运行时 API 返回 code:-1 "VNT not running"——
//!   映射为「实例未运行」并退出码 0（信息性，不视为命令失败）

use anyhow::{Context, bail};

use crate::Cli;
use crate::http::{WebClient, WebError, query_escape};
use crate::render::{format_bytes, format_mode, opt, table};

/// get_peers/get_routes 对未运行实例的错误文案
/// （service_http.rs get_peers:2799 / get_routes:2986）。
const NOT_RUNNING_HINT: &str = "VNT not running";

fn get_str<'v>(data: &'v serde_json::Value, key: &str) -> Option<&'v str> {
    data.get(key).and_then(serde_json::Value::as_str)
}

fn get_bool_label(data: &serde_json::Value, key: &str) -> String {
    opt(data
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .map(|v| if v { "是" } else { "否" }))
}

/// 实例是否运行中（VntStatus 小写序列化，service_http.rs:49）。
fn is_running(data: &serde_json::Value) -> bool {
    get_str(data, "status") == Some("running")
}

/// info 渲染（纯函数，测试直接喂 HttpAppInfo JSON）。
///
/// 非 running 只打「未运行」+ status（复核钉死）；running 分节打印：
/// 身份 / 网络 / 服务器 / 特性开关 / 子网。
pub fn render_info(name: &str, data: &serde_json::Value) -> String {
    let status = get_str(data, "status").unwrap_or("unknown");
    if !is_running(data) {
        return format!("实例 {name} 未运行（status: {status}）\n");
    }
    let mut out = String::new();
    out.push_str(&format!("实例 {name}（status: {status}）\n"));

    out.push_str("[身份]\n");
    out.push_str(&format!("设备名: {}\n", opt(get_str(data, "name"))));
    out.push_str(&format!("设备 ID: {}\n", opt(get_str(data, "device_id"))));
    out.push_str(&format!("版本: {}\n", opt(get_str(data, "version"))));
    out.push_str(&format!(
        "组网编号: {}\n",
        opt(get_str(data, "network_code"))
    ));

    out.push_str("[网络]\n");
    out.push_str(&format!("虚拟 IP: {}\n", opt(get_str(data, "ip"))));
    out.push_str(&format!(
        "掩码长度: {}\n",
        opt(data.get("prefix_len").and_then(serde_json::Value::as_u64))
    ));
    out.push_str(&format!("网关: {}\n", opt(get_str(data, "gateway"))));
    out.push_str(&format!("NAT 类型: {}\n", opt(get_str(data, "nat_type"))));
    out.push_str(&format!(
        "MTU: {}\n",
        opt(data.get("mtu").and_then(serde_json::Value::as_u64))
    ));
    let public_v4s: Vec<&str> = data
        .get("public_ipv4s")
        .and_then(serde_json::Value::as_array)
        .map(|list| list.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    out.push_str(&format!("公网 IPv4: {}\n", public_v4s.join(", ")));
    out.push_str(&format!(
        "公网 IPv6: {}\n",
        opt(get_str(data, "public_ipv6"))
    ));
    out.push_str(&format!(
        "在线对端: {}（直连 {}，离线 {}）\n",
        data.get("online_client_num")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        data.get("direct_client_num")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        data.get("offline_client_num")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
    ));

    out.push_str("[服务器]\n");
    let servers = data
        .get("server_info")
        .and_then(serde_json::Value::as_array);
    match servers {
        Some(list) if !list.is_empty() => {
            let rows: Vec<Vec<String>> = list
                .iter()
                .map(|server| {
                    vec![
                        opt(get_str(server, "server")),
                        if server
                            .get("connected")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                        {
                            "已连接".to_string()
                        } else {
                            "未连接".to_string()
                        },
                        opt(server
                            .get("server_rtt")
                            .and_then(serde_json::Value::as_u64)
                            .map(|rtt| format!("{rtt} ms"))),
                        opt(get_str(server, "server_version")),
                    ]
                })
                .collect();
            out.push_str(&table(&["地址", "状态", "RTT", "版本"], &rows));
        }
        _ => out.push_str("（无服务器信息）\n"),
    }

    out.push_str("[特性开关]\n");
    for (key, label) in [
        ("fec", "FEC"),
        ("compress", "压缩"),
        ("encrypt", "加密"),
        ("rtx", "RTX"),
    ] {
        out.push_str(&format!("{label}: {}\n", get_bool_label(data, key)));
    }
    for (key, label) in [
        ("allow_ikev2", "IKEv2 互通"),
        ("allow_wireguard", "WireGuard 互通"),
    ] {
        out.push_str(&format!("{label}: {}\n", get_bool_label(data, key)));
    }

    out.push_str("[子网]\n");
    for (key, label) in [
        ("input", "宣告子网"),
        ("output", "出口子网"),
        ("automatic_input", "自动子网"),
    ] {
        let nets: Vec<&str> = data
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|list| list.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default();
        out.push_str(&format!("{label}: {}\n", nets.join(", ")));
    }
    out
}

/// peers 渲染（纯函数）：表格 ip/name/online/mode/tx/rx/loss；
/// 空列表打「无对端」。
pub fn render_peers(name: &str, data: &serde_json::Value) -> String {
    let Some(peers) = data.as_array() else {
        return format!("实例 {name} 对端列表响应不是数组\n");
    };
    if peers.is_empty() {
        return format!("实例 {name} 无对端\n");
    }
    let rows: Vec<Vec<String>> = peers
        .iter()
        .map(|peer| {
            let route = peer.get("route").filter(|v| !v.is_null());
            let traffic = peer.get("traffic").filter(|v| !v.is_null());
            let loss = peer
                .get("packet_loss")
                .and_then(|v| v.get("loss_rate"))
                .and_then(serde_json::Value::as_f64);
            vec![
                opt(get_str(peer, "ip")),
                opt(get_str(peer, "name")),
                if peer
                    .get("online")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    "在线".to_string()
                } else {
                    "离线".to_string()
                },
                opt(route.and_then(|r| get_str(r, "mode")).map(format_mode)),
                opt(traffic
                    .and_then(|t| t.get("tx_bytes"))
                    .and_then(serde_json::Value::as_u64)
                    .map(format_bytes)),
                opt(traffic
                    .and_then(|t| t.get("rx_bytes"))
                    .and_then(serde_json::Value::as_u64)
                    .map(format_bytes)),
                opt(loss.map(|rate| format!("{:.1}%", rate * 100.0))),
            ]
        })
        .collect();
    let mut out = format!("实例 {name} 对端（{} 个）\n", peers.len());
    out.push_str(&table(
        &["IP", "名称", "状态", "模式", "发送", "接收", "丢包"],
        &rows,
    ));
    out
}

/// routes 渲染（纯函数）：按目的 IP 分组，每组一张明细表
/// （addr/protocol/metric/rtt/mode）；空列表打「无路由」。
pub fn render_routes(name: &str, data: &serde_json::Value) -> String {
    let Some(items) = data.as_array() else {
        return format!("实例 {name} 路由表响应不是数组\n");
    };
    if items.is_empty() {
        return format!("实例 {name} 无路由\n");
    }
    let mut out = format!("实例 {name} 路由表\n");
    for item in items {
        let ip = opt(get_str(item, "ip"));
        out.push_str(&format!("目的 {ip}\n"));
        let rows: Vec<Vec<String>> = item
            .get("routes")
            .and_then(serde_json::Value::as_array)
            .map(|routes| {
                routes
                    .iter()
                    .map(|route| {
                        vec![
                            opt(get_str(route, "addr")),
                            opt(get_str(route, "protocol")),
                            opt(route.get("metric").and_then(serde_json::Value::as_u64)),
                            opt(route
                                .get("rtt")
                                .and_then(serde_json::Value::as_u64)
                                .map(|rtt| format!("{rtt} ms"))),
                            opt(get_str(route, "mode").map(format_mode)),
                        ]
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push_str(&table(&["地址", "协议", "跃点", "RTT", "模式"], &rows));
    }
    out
}

/// 拉取遥测端点；「VNT not running」映射为 None（信息性分支）。
fn fetch(client: &WebClient, path: &str) -> anyhow::Result<Option<serde_json::Value>> {
    match client.get(path) {
        Ok(data) => Ok(Some(data)),
        Err(WebError::Api(msg)) if msg.contains(NOT_RUNNING_HINT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn info(client: &WebClient, name: &str) -> anyhow::Result<()> {
    let file_name = crate::config::resolve_instance_name(client, name)?;
    let data = client
        .get(&format!("/api/info?file_name={}", query_escape(&file_name)))
        .context("查询实例信息失败")?;
    print!("{}", render_info(name, &data));
    Ok(())
}

pub fn peers(client: &WebClient, name: &str) -> anyhow::Result<()> {
    let file_name = crate::config::resolve_instance_name(client, name)?;
    let data = fetch(
        client,
        &format!("/api/peers?file_name={}", query_escape(&file_name)),
    )?;
    match data {
        Some(data) => print!("{}", render_peers(name, &data)),
        // 信息性退出码 0（复核钉死：未运行不视为命令失败）
        None => println!("实例 {name} 未运行"),
    }
    Ok(())
}

pub fn routes(client: &WebClient, name: &str) -> anyhow::Result<()> {
    let file_name = crate::config::resolve_instance_name(client, name)?;
    let data = fetch(
        client,
        &format!("/api/routes?file_name={}", query_escape(&file_name)),
    )?;
    match data {
        Some(data) => print!("{}", render_routes(name, &data)),
        None => println!("实例 {name} 未运行"),
    }
    Ok(())
}

pub fn dispatch(cli: &Cli, client: &WebClient) -> anyhow::Result<()> {
    match &cli.command {
        crate::Command::Info { name } => info(client, name),
        crate::Command::Peers { name } => peers(client, name),
        crate::Command::Routes { name } => routes(client, name),
        _ => bail!("非遥测子命令"),
    }
}
