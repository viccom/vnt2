use crate::context::SharedNetworkAddr;
use crate::protocol::client_message::SharedNodeIdentity;
use crate::protocol::control_message::{RegRequestMsg, RegistrationMode};
use crate::tls::verifier::CertValidationMode;
use anyhow::bail;
use rand::seq::SliceRandom;
use rustp2p_core::socket::LocalInterface;
use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct ConnectRegConfig {
    pub server_addr: ProtocolAddress,
    pub cert_mode: CertValidationMode,
    pub network_code: String,
    pub device_id: String,
    pub identity: SharedNodeIdentity,
    pub ip: SharedNetworkAddr,
    pub key_sign: Option<String>,
    pub ip_variable: bool,
    pub allow_ikev2: bool,
    pub allow_wireguard: bool,
    pub default_interface: Option<LocalInterface>,
    pub managed: Option<crate::context::config::ManagedRegistration>,
    pub client_instance_id: Arc<Vec<u8>>,
    /// wss 握手路径（空串归一为 "/"），仅 Wss 协议生效
    pub wss_path: String,
    /// wss 握手附加 header（"K: V" 解析后的键值对），
    /// 用于穿越要求 cookie 寻址的 HTTP 反代（如沙盒 webview 网关）
    pub wss_headers: Vec<(String, String)>,
}
#[derive(Debug, Clone)]
pub(crate) struct ConnectConfig {
    pub protocol_type: ProtocolType,
    pub server_addr: SocketAddr,
    pub server_domain: String,
    pub cert_mode: CertValidationMode,
    pub default_interface: Option<LocalInterface>,
    pub wss_path: String,
    pub wss_headers: Vec<(String, String)>,
}
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub enum ProtocolType {
    Quic,
    #[default]
    TlsTcp,
    Wss,
    Dynamic,
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProtocolAddress {
    pub protocol_type: ProtocolType,
    pub address: String,
    /// wss 地址自带的路径（`wss://host:port/path` 中的 path，含 query），
    /// 作为 wss 握手路径的默认值；显式 wss_path 配置优先。其余协议恒为 None。
    pub path: Option<String>,
}
impl Default for ProtocolAddress {
    fn default() -> Self {
        Self {
            protocol_type: ProtocolType::default(),
            address: "127.0.0.1:29872".to_string(),
            path: None,
        }
    }
}
impl FromStr for ProtocolAddress {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (protocol_type, server_addr) = parse_server(s)?;
        // wss 地址支持 host:port/path 形式：路径剥离保存（穿越路径前缀反代），
        // 其余协议不分割（地址中不该出现路径，出现也按 host 原样保留、由
        // split_host_port/DNS 报错）
        let (address, path) = if protocol_type == ProtocolType::Wss {
            match server_addr.split_once('/') {
                Some((host, path)) => (host.to_string(), Some(format!("/{path}"))),
                None => (server_addr, None),
            }
        } else {
            (server_addr, None)
        };
        Ok(Self {
            protocol_type,
            address,
            path,
        })
    }
}
impl fmt::Display for ProtocolAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.protocol_type {
            ProtocolType::Quic => "quic://",
            ProtocolType::TlsTcp => "tcp://",
            ProtocolType::Wss => "wss://",
            ProtocolType::Dynamic => "dynamic://",
        };
        let path = self.path.as_deref().unwrap_or("");
        write!(f, "{}{}{}", prefix, self.address, path)
    }
}
pub fn parse_server(val: &str) -> Result<(ProtocolType, String), String> {
    let trimmed = val.trim();
    let lower = trimmed.to_lowercase();
    // 前缀匹配不区分大小写；返回的地址保留原始大小写，
    // 因为 dynamic://http(s):// 的 URL 路径/查询串是区分大小写的
    if lower.starts_with("quic://") {
        return Ok((ProtocolType::Quic, trimmed[7..].to_string()));
    }
    if lower.starts_with("tcp://") {
        return Ok((ProtocolType::TlsTcp, trimmed[6..].to_string()));
    }
    if lower.starts_with("wss://") {
        return Ok((ProtocolType::Wss, trimmed[6..].to_string()));
    }
    if lower.starts_with("dynamic://") {
        return Ok((ProtocolType::Dynamic, trimmed[10..].to_string()));
    }
    if trimmed.contains("://") {
        return Err(format!("Unknown protocol in server address: {}", val));
    }
    Ok((ProtocolType::TlsTcp, trimmed.to_string()))
}
impl ConnectRegConfig {
    pub fn reg_msg_request(
        &self,
        server_id: u32,
        registration_mode: RegistrationMode,
    ) -> RegRequestMsg {
        let identity = self.identity.get();
        RegRequestMsg {
            network_code: self.network_code.to_string(),
            device_id: self.device_id.to_string(),
            ip: self.ip.get().map(|network| network.ip),
            name: identity.name,
            version: env!("CARGO_PKG_VERSION").to_string(),
            key_sign: self.key_sign.clone(),
            ip_variable: self.ip_variable,
            server_id,
            registration_mode,
            advertised_subnets: identity.advertised_subnets,
            allow_ikev2: self.allow_ikev2,
            allow_wireguard: self.allow_wireguard,
            subscription: self
                .managed
                .as_ref()
                .map(|managed| managed.create_registration(&self.network_code, &self.device_id)),
            client_instance_id: self.client_instance_id.as_ref().clone(),
        }
    }
    /// 解析出全部候选服务器地址：动态地址（DNS TXT 记录或 http(s) 接口返回的
    /// 地址列表）与静态地址解析出的每个 IP 对应一个 ConnectConfig，
    /// 调用方逐个尝试连接直到成功。
    pub async fn to_connect_config(&self) -> anyhow::Result<Vec<ConnectConfig>> {
        let server_domains: Vec<(ProtocolType, String)> = match self.server_addr.protocol_type {
            ProtocolType::Dynamic => {
                // 动态发现：默认用 DNS TXT 记录；也支持填入 http(s) 接口，
                // 如 dynamic://https://example.com/servers，响应为换行分隔的
                // 服务器地址列表（每行同样支持 tcp:// quic:// wss:// 前缀）
                let address = &self.server_addr.address;
                let lower_address = address.to_lowercase();
                let entries: Vec<String> = if lower_address.starts_with("http://")
                    || lower_address.starts_with("https://")
                {
                    // HTTP(S) 接口：按返回顺序连接，服务端可自行控制优先级
                    let body = crate::utils::http_get::http_get_text(address).await?;
                    body.lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_string)
                        .collect()
                } else {
                    // DNS TXT 记录的顺序无意义，随机化以分散负载
                    let mut txt = crate::utils::dns_query::dns_query_txt(
                        address,
                        vec![],
                        &self.default_interface,
                    )
                    .await?;
                    txt.shuffle(&mut rand::rng());
                    txt
                };
                entries
                    .into_iter()
                    .map(|x| {
                        let x = x.to_lowercase();
                        let (protocol_type, domain) = parse_dynamic_txt(&x);
                        (protocol_type, domain.to_owned())
                    })
                    .collect()
            }
            v => vec![(v, self.server_addr.address.to_string())],
        };

        // 显式 wss_path 配置优先；否则用 wss 地址自带路径（parse 时保存，
        // dynamic 发现的地址不支持路径）
        let addr_wss_path = if self.server_addr.protocol_type == ProtocolType::Wss {
            self.server_addr.path.clone()
        } else {
            None
        };

        let mut connect_configs = Vec::new();
        for (protocol_type, server_domain) in server_domains {
            let effective_wss_path = if self.wss_path.is_empty() {
                addr_wss_path.clone().unwrap_or_default()
            } else {
                self.wss_path.clone()
            };
            // DNS 只负责解析 IP，端口由调用方从地址中解析后自行拼装
            let (host, port) = match crate::utils::addr::split_host_port(&server_domain) {
                Ok(v) => v,
                Err(error) => {
                    log::warn!("invalid server address {server_domain:?}: {error:#}");
                    continue;
                }
            };
            let addrs = match crate::utils::dns_query::dns_query_all(
                host,
                &vec![],
                &self.default_interface,
            )
            .await
            {
                Ok(addrs) => addrs,
                Err(error) => {
                    log::warn!("DNS query failed for {host:?}: {error:#}");
                    continue;
                }
            };
            connect_configs.extend(addrs.into_iter().map(|ip| ConnectConfig {
                protocol_type,
                server_addr: SocketAddr::new(ip, port),
                server_domain: host.to_owned(),
                cert_mode: self.cert_mode.clone(),
                default_interface: self.default_interface.clone(),
                wss_path: effective_wss_path.clone(),
                wss_headers: self.wss_headers.clone(),
            }));
        }
        if connect_configs.is_empty() {
            bail!(
                "no server address resolved for {}",
                self.server_addr.address
            );
        }
        Ok(connect_configs)
    }
}
/// 解析动态 DNS TXT 记录中的协议前缀。
/// 注意 wss:// 必须映射到 Wss（此前错映射为 TlsTcp，导致动态发现模式下
/// WSS 实际不可用）。
fn parse_dynamic_txt(txt: &str) -> (ProtocolType, &str) {
    if let Some(v) = txt.strip_prefix("udp://") {
        (ProtocolType::Quic, v)
    } else if let Some(v) = txt.strip_prefix("quic://") {
        (ProtocolType::Quic, v)
    } else if let Some(v) = txt.strip_prefix("tcp://") {
        (ProtocolType::TlsTcp, v)
    } else if let Some(v) = txt.strip_prefix("ws://") {
        (ProtocolType::TlsTcp, v)
    } else if let Some(v) = txt.strip_prefix("wss://") {
        (ProtocolType::Wss, v)
    } else {
        (ProtocolType::TlsTcp, txt)
    }
}
impl ConnectConfig {
    pub fn server_addr(&self) -> SocketAddr {
        self.server_addr
    }
    pub fn server_name(&self) -> &String {
        &self.server_domain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn registration_request_reads_latest_shared_ip_and_identity() {
        let initial_ip = Ipv4Addr::new(10, 26, 0, 2);
        let updated_ip = Ipv4Addr::new(10, 26, 0, 9);
        let shared_ip = SharedNetworkAddr::default();
        shared_ip.set(crate::context::NetworkAddr {
            ip: initial_ip,
            gateway: None,
            prefix_len: 24,
            broadcast: Ipv4Addr::new(10, 26, 0, 255),
        });
        let shared_identity =
            SharedNodeIdentity::new(crate::protocol::client_message::NodeIdentityTemplate {
                name: "device".to_string(),
                version: "test".to_string(),
                network_code: "test".to_string(),
                advertised_subnets: Vec::new(),
            });
        let config = ConnectRegConfig {
            server_addr: ProtocolAddress::default(),
            cert_mode: CertValidationMode::default(),
            network_code: "test".to_string(),
            device_id: "device".to_string(),
            identity: shared_identity.clone(),
            ip: shared_ip.clone(),
            key_sign: None,
            ip_variable: true,
            allow_ikev2: false,
            allow_wireguard: false,
            default_interface: None,
            managed: None,
            client_instance_id: Arc::new(vec![1; 32]),
            wss_path: String::new(),
            wss_headers: Vec::new(),
        };

        assert_eq!(
            config.reg_msg_request(0, RegistrationMode::Normal).ip,
            Some(initial_ip)
        );
        shared_ip.set(crate::context::NetworkAddr {
            ip: updated_ip,
            gateway: None,
            prefix_len: 24,
            broadcast: Ipv4Addr::new(10, 26, 0, 255),
        });
        shared_identity.set(crate::protocol::client_message::NodeIdentityTemplate {
            name: "renamed".to_string(),
            version: "test".to_string(),
            network_code: "test".to_string(),
            advertised_subnets: Vec::new(),
        });
        let request = config.reg_msg_request(0, RegistrationMode::Normal);
        assert_eq!(request.ip, Some(updated_ip));
        assert_eq!(request.name, "renamed");
    }

    /// 动态 DNS TXT 记录中的 wss:// 必须映射为 Wss，
    /// 其余前缀维持原有映射不变
    #[test]
    fn test_parse_dynamic_txt() {
        assert_eq!(
            parse_dynamic_txt("wss://example.com:443"),
            (ProtocolType::Wss, "example.com:443")
        );
        assert_eq!(
            parse_dynamic_txt("quic://example.com:29872"),
            (ProtocolType::Quic, "example.com:29872")
        );
        assert_eq!(
            parse_dynamic_txt("udp://example.com:29872"),
            (ProtocolType::Quic, "example.com:29872")
        );
        assert_eq!(
            parse_dynamic_txt("tcp://example.com:29872"),
            (ProtocolType::TlsTcp, "example.com:29872")
        );
        assert_eq!(
            parse_dynamic_txt("ws://example.com:80"),
            (ProtocolType::TlsTcp, "example.com:80")
        );
        assert_eq!(
            parse_dynamic_txt("example.com:29872"),
            (ProtocolType::TlsTcp, "example.com:29872")
        );
    }

    /// 协议前缀不区分大小写，但 dynamic://http(s):// 的 URL 地址必须保留
    /// 原始大小写（路径/查询串区分大小写）
    #[test]
    fn test_parse_server_preserves_url_case() {
        assert_eq!(
            parse_server("dynamic://HTTPS://Example.com/API/GetServers?token=AbC"),
            Ok((
                ProtocolType::Dynamic,
                "HTTPS://Example.com/API/GetServers?token=AbC".to_string()
            ))
        );
        assert_eq!(
            parse_server("DYNAMIC://http://127.0.0.1:8080/servers"),
            Ok((
                ProtocolType::Dynamic,
                "http://127.0.0.1:8080/servers".to_string()
            ))
        );
        assert_eq!(
            parse_server("TCP://Example.com:29872"),
            Ok((ProtocolType::TlsTcp, "Example.com:29872".to_string()))
        );
    }

    /// wss 地址支持 host:port/path 形式：路径剥离保存到 path 字段，
    /// 作为 wss 握手路径的默认值（穿越路径前缀反代）；其余协议不分割。
    /// Display 必须还原完整地址，保证配置序列化 roundtrip。
    #[test]
    fn parse_wss_address_extracts_path() {
        let addr: ProtocolAddress = "wss://Example.com:443/vnts/".parse().unwrap();
        assert_eq!(addr.address, "Example.com:443");
        assert_eq!(addr.path.as_deref(), Some("/vnts/"));
        assert_eq!(addr.to_string(), "wss://Example.com:443/vnts/");

        let no_path: ProtocolAddress = "wss://Example.com:443".parse().unwrap();
        assert_eq!(no_path.address, "Example.com:443");
        assert_eq!(no_path.path, None);
        assert_eq!(no_path.to_string(), "wss://Example.com:443");

        // 非协议默认端口的路径同样剥离；query 一并保留在 path 中
        let with_query: ProtocolAddress = "wss://Example.com:8443/gw?p=1".parse().unwrap();
        assert_eq!(with_query.address, "Example.com:8443");
        assert_eq!(with_query.path.as_deref(), Some("/gw?p=1"));
        assert_eq!(with_query.to_string(), "wss://Example.com:8443/gw?p=1");

        // 其余协议不分割路径（tcp/quic 地址无路径语义，host 部分原样保留）
        let tcp: ProtocolAddress = "tcp://Example.com:29872".parse().unwrap();
        assert_eq!(tcp.path, None);
        assert_eq!(tcp.to_string(), "tcp://Example.com:29872");
    }
}
