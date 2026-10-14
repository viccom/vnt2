use crate::protocol::transmission::TransmissionBytes;
use crate::tunnel_core::server::transport::config::ConnectConfig;
use anyhow::{Context, bail};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};
use tokio_tungstenite::{WebSocketStream, client_async, tungstenite::Message};

type WssStream = WebSocketStream<TlsStream<TcpStream>>;

#[derive(Default)]
pub struct WssTransport {
    stream: Option<WssStream>,
}

impl WssTransport {
    pub fn new() -> Self {
        Default::default()
    }
    pub fn disconnect(&mut self) {
        self.stream = None;
    }
    pub async fn graceful_disconnect(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            // Close the WebSocket protocol first, then explicitly shut down
            // rustls so the peer receives TLS close_notify as well.
            let _ = stream.close(None).await;
            let _ = stream.get_mut().shutdown().await;
        }
    }
    pub async fn connect(&mut self, config: &ConnectConfig) -> anyhow::Result<()> {
        if self.stream.is_some() {
            bail!("Already connected");
        }
        let stream = connect_wss(config).await?;
        self.stream = Some(stream);
        Ok(())
    }
    pub async fn send(&mut self, buf: Bytes) -> anyhow::Result<()> {
        let Some(framed) = self.stream.as_mut() else {
            bail!("Not connected");
        };
        framed
            .send(Message::Binary(buf))
            .await
            .context("send to server failed")
    }
    pub async fn next(&mut self) -> anyhow::Result<TransmissionBytes> {
        let Some(framed) = self.stream.as_mut() else {
            bail!("Not connected");
        };
        loop {
            let message = framed
                .next()
                .await
                .context("EOF")?
                .context("receive from server failed")?;
            match message {
                Message::Binary(buf) => {
                    return Ok(TransmissionBytes::from(buf));
                }
                Message::Close(_) => {
                    bail!("Disconnected");
                }
                _ => {
                    continue;
                }
            }
        }
    }
}

pub async fn connect_wss(config: &ConnectConfig) -> anyhow::Result<WssStream> {
    let server_addr = config.server_addr();
    let server_name = config.server_name().clone();

    let rustls_config = config.cert_mode.create_tls_client_config()?;
    let connector = TlsConnector::from(Arc::new(rustls_config));

    let tcp_stream =
        crate::utils::socket::connect_tcp(server_addr, config.default_interface.as_ref())
            .await
            .context("Failed to establish underlying TCP connection")?;
    if let Err(e) = tcp_stream.set_nodelay(true) {
        log::error!("Failed to set TCP_NODELAY: {}", e);
    }
    let request = build_wss_request(
        &server_name,
        server_addr.port(),
        &config.wss_path,
        &config.wss_headers,
    )?;

    let dns_name = server_name
        .try_into()
        .context("Invalid server name for TLS")?;

    let tls_stream = connector
        .connect(dns_name, tcp_stream)
        .await
        .context("Failed to perform TLS handshake")?;

    let (ws_stream, _response) = client_async(request, tls_stream)
        .await
        .context("Failed to perform WebSocket handshake")?;

    Ok(ws_stream)
}

/// Build the WebSocket handshake request from the configured server name,
/// path and extra headers.
///
/// `ConnectConfig::server_name` intentionally stores IPv6 literals without
/// brackets so it can be used as a TLS `ServerName`. URLs, however, require
/// brackets around an IPv6 host and must include the configured port.
///
/// Extra headers let the client traverse HTTP reverse proxies that require
/// cookie-based addressing (e.g. sandbox webview gateways): an empty path
/// normalizes to `/`, and malformed header names/values are rejected up
/// front instead of failing mid-handshake.
fn build_wss_request(
    server_name: &str,
    port: u16,
    path: &str,
    headers: &[(String, String)],
) -> anyhow::Result<http::Request<()>> {
    let path = if path.is_empty() { "/" } else { path };
    let host = if server_name.contains(':') {
        format!("[{server_name}]")
    } else {
        server_name.to_owned()
    };
    let uri: http::Uri = format!("wss://{host}:{port}{path}")
        .parse()
        .context("Invalid WebSocket URL")?;
    let authority = uri
        .authority()
        .map(|a| a.as_str().to_string())
        .unwrap_or_default();
    let mut request = http::Request::builder()
        .uri(uri)
        .body(())
        .context("Failed to build WebSocket request")?;
    {
        let request_headers = request.headers_mut();
        // 用户自定义 header 先插入（可覆盖 Host 等）
        for (name, value) in headers {
            let header_name = http::HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("Invalid header name: {name}"))?;
            let header_value = http::HeaderValue::from_str(value)
                .with_context(|| format!("Invalid header value for header {name}"))?;
            request_headers.insert(header_name, header_value);
        }
        // tungstenite generate_request 要求请求自带全部 WS 握手头
        // （IntoClientRequest for http::Request<()> 原样返回，不自动生成）；
        // Host 用户可自定义（反代后端伪装场景），缺省取 URI authority，
        // 其余四个强制写入保证握手有效
        let defaults = [
            (http::header::CONNECTION, "Upgrade".to_string()),
            (http::header::UPGRADE, "websocket".to_string()),
            (http::header::SEC_WEBSOCKET_VERSION, "13".to_string()),
            (
                http::header::SEC_WEBSOCKET_KEY,
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            ),
        ];
        for (name, value) in defaults {
            let header_value = http::HeaderValue::from_str(&value).expect("static header value");
            request_headers.insert(name, header_value);
        }
        if !request_headers.contains_key(http::header::HOST) && !authority.is_empty() {
            let header_value =
                http::HeaderValue::from_str(&authority).expect("authority header value");
            request_headers.insert(http::header::HOST, header_value);
        }
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::build_wss_request;

    #[test]
    fn request_preserves_port_and_brackets_ipv6_literal() {
        let request = build_wss_request("example.com", 8443, "/", &[]).unwrap();
        assert_eq!(request.uri(), "wss://example.com:8443/");
        let request = build_wss_request("192.0.2.1", 8443, "/", &[]).unwrap();
        assert_eq!(request.uri(), "wss://192.0.2.1:8443/");
        let request = build_wss_request("2001:db8::1", 8443, "/", &[]).unwrap();
        assert_eq!(request.uri(), "wss://[2001:db8::1]:8443/");
    }

    #[test]
    fn request_appends_path_and_headers() {
        let headers = vec![(
            "Cookie".to_string(),
            "x-cs-sandbox-id=abc; x-cs-sandbox-port=9000".to_string(),
        )];
        let request = build_wss_request("webview.example.com", 443, "/vnts/", &headers).unwrap();
        assert_eq!(request.uri(), "wss://webview.example.com:443/vnts/");
        assert_eq!(
            request
                .headers()
                .get("cookie")
                .and_then(|value| value.to_str().ok()),
            Some("x-cs-sandbox-id=abc; x-cs-sandbox-port=9000")
        );
    }

    #[test]
    fn request_normalizes_empty_path_to_root() {
        let request = build_wss_request("example.com", 8443, "", &[]).unwrap();
        assert_eq!(request.uri(), "wss://example.com:8443/");
    }

    /// tungstenite 的 generate_request 要求请求自带全部 WS 握手头
    /// （Host/Connection/Upgrade/Sec-WebSocket-Version/Sec-WebSocket-Key），
    /// IntoClientRequest for http::Request<()> 原样返回不补头——必须在此补全，
    /// 否则握手被服务器以缺 sec-websocket-key 拒绝
    #[test]
    fn request_includes_websocket_handshake_headers() {
        let request = build_wss_request("example.com", 8443, "/", &[]).unwrap();
        let headers = request.headers();
        assert_eq!(headers.get("host").unwrap(), "example.com:8443");
        assert_eq!(headers.get("connection").unwrap(), "Upgrade");
        assert_eq!(headers.get("upgrade").unwrap(), "websocket");
        assert_eq!(headers.get("sec-websocket-version").unwrap(), "13");
        let key = headers.get("sec-websocket-key").unwrap();
        assert!(!key.is_empty(), "Sec-WebSocket-Key must be generated");

        // 用户自定义 Host 优先（反代后端伪装场景），缺省取 URI authority
        let custom_host = vec![("Host".to_string(), "gateway.internal".to_string())];
        let request = build_wss_request("example.com", 8443, "/", &custom_host).unwrap();
        assert_eq!(request.headers().get("host").unwrap(), "gateway.internal");
    }

    #[test]
    fn request_rejects_invalid_header_name() {
        let headers = vec![("Bad Header".to_string(), "value".to_string())];
        assert!(build_wss_request("example.com", 8443, "/", &headers).is_err());
    }

    #[test]
    fn request_rejects_invalid_header_value() {
        let headers = vec![("Cookie".to_string(), "bad\nvalue".to_string())];
        assert!(build_wss_request("example.com", 8443, "/", &headers).is_err());
    }
}
