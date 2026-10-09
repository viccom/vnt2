//! 遥测命令的假 HTTP 服务端到端测试（计划 §4 Task 4；先红后绿）。
//!
//! 假服务骨架与 tests/lifecycle_e2e.rs 同模式（标准库 TcpListener 线程）。
//! 核心锁定「复核钉死」：peers/routes 在实例未运行时 API 返回
//! code:-1 "VNT not running"，CLI 映射为「实例未运行」并退出码 0
//! （信息性，不视为命令失败）；其它 code:-1 仍为错误。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

fn run_cli(args: &[&str], base: &str) -> anyhow::Result<i32> {
    vnt_webctl::run_cli(
        std::iter::once("vnt2_web".to_string()).chain(args.iter().map(|s| s.to_string())),
        base,
        None,
    )
}

fn ok_envelope(data: &str) -> String {
    format!(r#"{{"code":0,"msg":"","data":{data}}}"#)
}

fn err_envelope(msg: &str) -> String {
    format!(r#"{{"code":-1,"msg":"{msg}","data":null}}"#)
}

fn http_response(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

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

/// 单线程 accept 假服务；Drop 时发信号 + 唤醒连接退出
/// （与 lifecycle_e2e.rs 相同的关停模式，防 accept 挂死）。
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

fn spawn_server(route: impl Fn(&str) -> String + Send + 'static) -> FakeServer {
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
            let Some(request_line) = read_request(&stream) else {
                continue;
            };
            // resolve_instance_name 会先查 /api/config/list：统一回空列表，
            // 既有用例以真实 file_name 直连（透传路径）；别名映射由 config_resolve.rs 锁定
            if request_line.starts_with("GET /api/config/list ") {
                let body = r#"{"code":0,"msg":"","data":[]}"#;
                if stream.write_all(&http_response(body)).is_err() {
                    break;
                }
                continue;
            }
            let body = route(&request_line);
            if stream.write_all(&http_response(&body)).is_err() {
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

/// info 走 /api/info：Stopped 态响应也能正常走完（退出码 0）。
#[test]
fn info_stopped_exits_zero() {
    let server = spawn_server(|request_line| {
        if request_line.starts_with("GET /api/info") {
            ok_envelope(
                r#"{"name":"u18","version":"1.2.13","ip":null,"prefix_len":null,
                   "gateway":null,"device_id":"dev-abc","status":"stopped",
                   "current_config_name":"foo","current_config_file":"foo.toml",
                   "online_client_num":0,"offline_client_num":0,"direct_client_num":0,
                   "server_info":[],"nat_type":null,"public_ipv6":null,"public_ipv4s":[],
                   "network_code":"dongnet","mtu":null,"fec":null,"compress":null,
                   "encrypt":null,"rtx":null,"allow_ikev2":true,"allow_wireguard":false,
                   "tunnel_listen_addrs":[],"input":[],"output":[],"automatic_input":[],
                   "config_changed":false}"#,
            )
        } else {
            err_envelope("unexpected route")
        }
    });
    let code = run_cli(&["info", "foo"], &server.base).expect("info Stopped 应成功");
    assert_eq!(code, 0);
}

/// peers 未运行：code:-1 "VNT not running" → 信息性成功（退出码 0）。
#[test]
fn peers_not_running_is_informational_success() {
    let server = spawn_server(|request_line| {
        if request_line.starts_with("GET /api/peers") {
            err_envelope("VNT not running")
        } else {
            err_envelope("unexpected route")
        }
    });
    let code = run_cli(&["peers", "foo"], &server.base).expect("未运行须映射为信息性成功");
    assert_eq!(code, 0);
}

/// routes 未运行：同 peers 的信息性语义。
#[test]
fn routes_not_running_is_informational_success() {
    let server = spawn_server(|request_line| {
        if request_line.starts_with("GET /api/routes") {
            err_envelope("VNT not running")
        } else {
            err_envelope("unexpected route")
        }
    });
    let code = run_cli(&["routes", "foo"], &server.base).expect("未运行须映射为信息性成功");
    assert_eq!(code, 0);
}

/// 其它 code:-1（如配置不存在）映射为退出码 1（Task 5 错误映射）。
#[test]
fn peers_other_api_error_exits_one() {
    let server = spawn_server(|_| err_envelope("Config file not found"));
    let code = run_cli(&["peers", "foo"], &server.base).expect("其它 -1 映射为退出码 1");
    assert_eq!(code, 1);
}

/// peers 正常数据：退出码 0。
#[test]
fn peers_running_exits_zero() {
    let server = spawn_server(|request_line| {
        if request_line.starts_with("GET /api/peers") {
            ok_envelope("[]")
        } else {
            err_envelope("unexpected route")
        }
    });
    let code = run_cli(&["peers", "foo"], &server.base).expect("peers 应成功");
    assert_eq!(code, 0);
}

/// routes 正常数据：退出码 0。
#[test]
fn routes_running_exits_zero() {
    let server = spawn_server(|request_line| {
        if request_line.starts_with("GET /api/routes") {
            ok_envelope("[]")
        } else {
            err_envelope("unexpected route")
        }
    });
    let code = run_cli(&["routes", "foo"], &server.base).expect("routes 应成功");
    assert_eq!(code, 0);
}
