//! HTTP 客户端与错误映射测试（计划 §4 Task 5；先红后绿）。
//!
//! 三层锁定：
//! - base 地址解析（复核钉死）：裸端口 / host:port 补 scheme / 完整 URL /
//!   尾斜杠去除 / 非法输入
//! - token 优先序：--token > VNT_WEB_TOKEN env > None（loopback 免 token）
//! - WebError 三分类：{code:-1} → Api、HTTP 非 2xx → Http、连接失败 → Connect
//!
//! 假服务模式与 tests/lifecycle_e2e.rs 同（标准库 TcpListener 线程）。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use vnt_webctl::http::{WebClient, WebError, resolve_token};

// ---- base 解析 ----

#[test]
fn base_bare_port_becomes_loopback() {
    let client = WebClient::new("19099", None).expect("裸端口必须可解析");
    assert_eq!(client.base(), "http://127.0.0.1:19099");
}

#[test]
fn base_host_port_gets_scheme() {
    let client = WebClient::new("127.0.0.1:19099", None).expect("host:port 必须可解析");
    assert_eq!(client.base(), "http://127.0.0.1:19099");
}

#[test]
fn base_full_url_passes_through() {
    let client = WebClient::new("http://10.0.0.2:8080", None).expect("完整 URL 必须原样接受");
    assert_eq!(client.base(), "http://10.0.0.2:8080");
}

#[test]
fn base_trailing_slash_is_trimmed() {
    let client = WebClient::new("http://127.0.0.1:19099/", None).expect("尾斜杠必须接受");
    assert_eq!(client.base(), "http://127.0.0.1:19099");
}

#[test]
fn base_invalid_input_is_connect_error_with_hint() {
    for input in ["", "http://", "not a url", "127.0.0.1:notaport"] {
        let err = WebClient::new(input, None).expect_err("非法 base 必须报错");
        assert!(
            matches!(err, WebError::Connect(_)),
            "非法 base 须归为 Connect: {input:?} -> {err:?}"
        );
        assert!(
            err.to_string().contains(input.trim()) || input.is_empty(),
            "错误应回显用户输入: {input:?} -> {err}"
        );
    }
}

// ---- resolve_token 优先序 ----

/// env 读取注入桩：用临时设置 + 恢复包住，避免测试间 env 串扰。
fn with_env_token<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
    let saved = std::env::var("VNT_WEB_TOKEN").ok();
    unsafe {
        match value {
            Some(v) => std::env::set_var("VNT_WEB_TOKEN", v),
            None => std::env::remove_var("VNT_WEB_TOKEN"),
        }
    }
    let result = f();
    unsafe {
        match saved {
            Some(v) => std::env::set_var("VNT_WEB_TOKEN", v),
            None => std::env::remove_var("VNT_WEB_TOKEN"),
        }
    }
    result
}

#[test]
fn cli_token_beats_env() {
    with_env_token(Some("env-token"), || {
        assert_eq!(
            resolve_token(Some("cli-token")),
            Some("cli-token".to_string())
        );
    });
}

#[test]
fn env_token_used_when_cli_absent() {
    with_env_token(Some("env-token"), || {
        assert_eq!(resolve_token(None), Some("env-token".to_string()));
    });
}

#[test]
fn no_token_falls_back_to_none() {
    with_env_token(None, || {
        assert_eq!(resolve_token(None), None);
    });
}

#[test]
fn empty_env_token_is_treated_as_absent() {
    with_env_token(Some(""), || {
        assert_eq!(resolve_token(None), None);
    });
}

// ---- WebError 三分类（假服务） ----

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
    let _ = reader.read_exact(&mut body);
    Some(request_line)
}

struct FakeServer {
    base: String,
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

/// 响应以 `(status_line, body)` 两参直接传入，避免字符串切分与 body 内空格冲突。
fn spawn_server(status: &'static str, body: String) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let wake_addr = listener.local_addr().unwrap();
    let base = format!("http://{wake_addr}");
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    let join = std::thread::spawn(move || {
        loop {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            if read_request(&stream).is_none() {
                continue;
            }
            let bytes = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            if stream.write_all(bytes.as_bytes()).is_err() {
                break;
            }
        }
    });
    FakeServer {
        base,
        shutdown: Some(shutdown_tx),
        wake_addr,
        join: Some(join),
    }
}

#[test]
fn api_error_maps_to_web_error_api() {
    let server = spawn_server(
        "200 OK",
        r#"{"code":-1,"msg":"Config file not found","data":null}"#.to_string(),
    );
    let client = WebClient::new(&server.base, None).unwrap();
    let err = client
        .get("/api/config?file_name=foo.toml")
        .expect_err("code:-1 必须报错");
    match err {
        WebError::Api(msg) => assert_eq!(msg, "Config file not found"),
        other => panic!("code:-1 须映射为 Api: {other:?}"),
    }
}

#[test]
fn http_500_maps_to_web_error_http() {
    let server = spawn_server("500 Internal Server Error", "{}".to_string());
    let client = WebClient::new(&server.base, None).unwrap();
    let err = client.get("/api/info").expect_err("HTTP 500 必须报错");
    match err {
        WebError::Http(status) => assert_eq!(status, 500),
        other => panic!("HTTP 500 须映射为 Http(500): {other:?}"),
    }
}

#[test]
fn connect_failure_maps_to_web_error_connect_with_hint() {
    // 无监听端口：连接即拒
    let client = WebClient::new("http://127.0.0.1:1", None).unwrap();
    let err = client.get("/api/info").expect_err("连接失败必须报错");
    match err {
        WebError::Connect(msg) => {
            assert!(
                msg.contains("服务未运行或无法连接"),
                "Connect 须带启动提示: {msg}"
            );
            assert!(msg.contains("--token"), "Connect 须提及 token 模式: {msg}");
        }
        other => panic!("连接失败须映射为 Connect: {other:?}"),
    }
}

#[test]
fn code_zero_returns_data() {
    let server = spawn_server(
        "200 OK",
        r#"{"code":0,"msg":"","data":{"x":1}}"#.to_string(),
    );
    let client = WebClient::new(&server.base, None).unwrap();
    let data = client.get("/api/info").expect("code:0 必须成功");
    assert_eq!(data.get("x").and_then(serde_json::Value::as_u64), Some(1));
}

// ---- run_cli 层：WebError → 退出码 1 ----

fn run_cli(args: &[&str], base: &str) -> anyhow::Result<i32> {
    vnt_webctl::run_cli(
        std::iter::once("vnt2_web".to_string()).chain(args.iter().map(|s| s.to_string())),
        base,
        None,
    )
}

/// 连接失败（服务未运行）：run_cli 映射为退出码 1 而非 Err 传播。
#[test]
fn run_cli_connect_failure_exits_one() {
    let code = run_cli(&["config", "list"], "http://127.0.0.1:1").expect("连接失败映射为退出码 1");
    assert_eq!(code, 1);
}

/// HTTP 非 2xx：run_cli 映射为退出码 1。
#[test]
fn run_cli_http_error_exits_one() {
    let server = spawn_server("500 Internal Server Error", "{}".to_string());
    let code = run_cli(&["config", "list"], &server.base).expect("HTTP 500 映射为退出码 1");
    assert_eq!(code, 1);
}
