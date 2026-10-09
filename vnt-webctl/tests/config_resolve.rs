//! 实例名解析（config_name → file_name 别名）的端到端测试。
//!
//! 背景：API 实例查找主键是 file_name（网页保存的无语义时间戳名，如
//! `1791428521875.toml`），而用户心智模型是 config_name（如 `metme`）。
//! CLI 必须先经 /api/config/list 把 config_name 解析成 file_name，
//! 否则 `status metme` 会对着不存在的 `metme.toml` 报 Stopped/实例不存在。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use vnt_webctl::config::resolve_instance_name;
use vnt_webctl::http::WebClient;

fn ok_envelope(data: &str) -> String {
    format!(r#"{{"code":0,"msg":"","data":{data}}}"#)
}

fn http_response(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// 单路由假服务：记录收到的请求行，按 route 应答。
struct FakeServer {
    base: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    shutdown: Option<mpsc::Sender<()>>,
    wake_addr: std::net::SocketAddr,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
            let _ = std::net::TcpStream::connect(self.wake_addr);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// 从已 accept 的连接读一个 HTTP 请求行（丢弃头与 body）。
fn read_request(stream: &std::net::TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 || line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).ok()?;
    Some(request_line)
}

fn spawn_server(route: impl Fn(&str) -> String + Send + 'static) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let wake_addr = listener.local_addr().unwrap();
    let base = format!("http://{wake_addr}");
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let thread_requests = requests.clone();
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    let join = std::thread::spawn(move || {
        loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let Some(request_line) = read_request(&stream) else {
                continue;
            };
            thread_requests.lock().unwrap().push(request_line.clone());
            let response = route(&request_line);
            if stream.write_all(&http_response(&response)).is_err() {
                break;
            }
        }
    });
    FakeServer {
        base,
        requests,
        shutdown: Some(shutdown_tx),
        wake_addr,
        join: Some(join),
    }
}

const CONFIG_LIST: &str = r#"[{"file_name":"1791428521875.toml","config_name":"metme"},{"file_name":"office.toml","config_name":"office"}]"#;

fn config_list_route(request_line: &str) -> String {
    if request_line.starts_with("GET /api/config/list ") {
        ok_envelope(CONFIG_LIST)
    } else {
        ok_envelope("null")
    }
}

#[test]
fn config_name_resolves_to_file_name() {
    let server = spawn_server(config_list_route);
    let client = WebClient::new(&server.base, None).unwrap();
    let resolved = resolve_instance_name(&client, "metme").unwrap();
    assert_eq!(resolved, "1791428521875.toml");
}

#[test]
fn exact_file_name_passes_through() {
    let server = spawn_server(config_list_route);
    let client = WebClient::new(&server.base, None).unwrap();
    let resolved = resolve_instance_name(&client, "1791428521875.toml").unwrap();
    assert_eq!(resolved, "1791428521875.toml");
    // 无扩展名但 file_name 匹配（规范化补 .toml 后命中）
    let resolved = resolve_instance_name(&client, "1791428521875").unwrap();
    assert_eq!(resolved, "1791428521875.toml");
}

#[test]
fn unknown_name_falls_back_to_normalized_for_server_error() {
    let server = spawn_server(config_list_route);
    let client = WebClient::new(&server.base, None).unwrap();
    // 无匹配：透传规范化名，让服务端报它的原错误（保持既有语义）
    let resolved = resolve_instance_name(&client, "nope").unwrap();
    assert_eq!(resolved, "nope.toml");
}

#[test]
fn ambiguous_config_name_is_rejected() {
    let server = spawn_server(|request_line| {
        if request_line.starts_with("GET /api/config/list ") {
            ok_envelope(
                r#"[{"file_name":"a.toml","config_name":"dup"},{"file_name":"b.toml","config_name":"dup"}]"#,
            )
        } else {
            ok_envelope("null")
        }
    });
    let client = WebClient::new(&server.base, None).unwrap();
    let error = resolve_instance_name(&client, "dup").unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("a.toml") && message.contains("b.toml"),
        "{message}"
    );
}

/// 全链路：`info metme` 须以解析后的 file_name 查 /api/info。
#[test]
fn info_command_uses_resolved_file_name() {
    let server = spawn_server(|request_line| {
        if request_line.starts_with("GET /api/config/list ") {
            ok_envelope(CONFIG_LIST)
        } else if request_line.starts_with("GET /api/info?file_name=1791428521875.toml") {
            ok_envelope(r#"{"version":"2.0.10","status":"running"}"#)
        } else {
            ok_envelope("null")
        }
    });
    let code = vnt_webctl::run_cli(
        std::iter::once("vnt2_web".to_string())
            .chain(["info", "metme"].iter().map(|s| s.to_string())),
        &server.base,
        None,
    )
    .unwrap();
    assert_eq!(code, 0);
    let requests = server.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|r| r.starts_with("GET /api/info?file_name=1791428521875.toml")),
        "须以解析后的 file_name 查询，实际请求: {requests:?}"
    );
}

/// 交叉匹配：A 的 file_name 恰为 B 的 config_name 时，file_name 精确命中
/// 必须胜出，不得误报歧义（8009401 审查发现的语义缺陷）
#[test]
fn exact_file_name_beats_cross_config_name_match() {
    let server = spawn_server(|request_line| {
        if request_line.starts_with("GET /api/config/list ") {
            ok_envelope(
                r#"[{"file_name":"metme.toml","config_name":"1791428521875"},{"file_name":"1791428521875.toml","config_name":"metme"}]"#,
            )
        } else {
            ok_envelope("null")
        }
    });
    let client = WebClient::new(&server.base, None).unwrap();
    let resolved = resolve_instance_name(&client, "metme").unwrap();
    assert_eq!(
        resolved, "metme.toml",
        "file_name 精确命中须无条件胜出，不受他配置 config_name 同名影响"
    );
}
