//! 生命周期命令的假 HTTP 服务端到端测试（计划 §4 Task 3；先红后绿）。
//!
//! 假服务为标准库 TcpListener 线程：按路由表应答 `{code,msg,data}` 包裹，
//! 覆盖 run_cli -> config::http_post/http_get -> lifecycle 全链路。
//! 轮询节奏由测试用 fake 配置注入（sleep 1ms / 超时 500ms），不拖慢测试。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use vnt_webctl::lifecycle::{self, PollConfig};

/// 生产轮询节奏由环境变量覆盖（测试注入快速值，避免每个经 run_cli 的
/// 用例都按 1s/轮睡眠拖慢套件；缺省即生产语义）。
fn test_poll_env() {
    // 非幂等但测试进程内多次调用无害
    unsafe {
        std::env::set_var("VNT_WEBCTL_POLL_INTERVAL_MS", "1");
        std::env::set_var("VNT_WEBCTL_POLL_TIMEOUT_MS", "500");
    }
}

fn run_cli(args: &[&str], base: &str) -> anyhow::Result<i32> {
    test_poll_env();
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

fn http_response(status: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

struct FakeServer {
    base: String,
    hits: Arc<AtomicUsize>,
    stop_hits: Arc<AtomicUsize>,
    shutdown: Option<mpsc::Sender<()>>,
    wake_addr: std::net::SocketAddr,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        // 显式关停：accept 是阻塞调用，仅发信号线程感知不到——须再自建
        // 一条连接把它从 accept 唤醒（2026-10-09 两次挂死实测，卡 futex_wait）
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
            let _ = std::net::TcpStream::connect(self.wake_addr);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// 从已 accept 的连接读一个 HTTP 请求（请求行 + 头 + 按 content-length 丢弃 body）。
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

/// 假服务骨架：单线程 accept 循环，route(请求行, 序号) 给出响应体。
/// 关停靠 Drop 的信号 + 一条唤醒连接（accept 返回后 read_request 读不到
/// 请求行，continue 到循环顶检出信号退出）。
fn spawn_server(
    route: impl Fn(&str, usize) -> String + Send + 'static,
) -> (FakeServer, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let wake_addr = listener.local_addr().unwrap();
    let base = format!("http://{wake_addr}");
    let hits = Arc::new(AtomicUsize::new(0));
    let thread_hits = hits.clone();
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
            let index = thread_hits.fetch_add(1, Ordering::SeqCst);
            let body = route(&request_line, index);
            if stream.write_all(&http_response("200 OK", &body)).is_err() {
                break;
            }
        }
    });
    let server = FakeServer {
        base,
        hits: Arc::new(AtomicUsize::new(0)),
        stop_hits: Arc::new(AtomicUsize::new(0)),
        shutdown: Some(shutdown_tx),
        wake_addr,
        join: Some(join),
    };
    (server, hits)
}

/// 状态序列假服务：/api/start 应答 `start_reply`，之后每次 /api/start/status
/// 依次吐出 `statuses`（耗尽后重复最后一项）。序号只计状态查询。
fn spawn_sequence_server(start_reply: String, statuses: Vec<String>) -> FakeServer {
    let status_index = Arc::new(AtomicUsize::new(0));
    let (mut server, hits) = spawn_server(move |request_line, _| {
        if request_line.starts_with("POST /api/start ") {
            start_reply.clone()
        } else if request_line.starts_with("GET /api/start/status") {
            let index = status_index.fetch_add(1, Ordering::SeqCst);
            statuses
                .get(index)
                .or_else(|| statuses.last())
                .cloned()
                .unwrap_or_default()
        } else {
            err_envelope("unexpected route")
        }
    });
    server.hits = hits;
    server
}

/// stop 专用假服务：/api/stop 应答 `stop_reply`。
fn spawn_stop_server(stop_reply: String) -> FakeServer {
    let (mut server, hits) = spawn_server(move |request_line, _| {
        if request_line.starts_with("POST /api/stop ") {
            stop_reply.clone()
        } else {
            err_envelope("unexpected route")
        }
    });
    server.stop_hits = hits;
    server
}

const FAST: PollConfig = PollConfig {
    interval: std::time::Duration::from_millis(1),
    timeout: std::time::Duration::from_millis(500),
};

// ---- start ----

#[test]
fn start_starting_then_running_succeeds() {
    let server = spawn_sequence_server(
        ok_envelope("null"),
        vec![
            ok_envelope(r#"{"status":"starting"}"#),
            ok_envelope(r#"{"status":"running"}"#),
        ],
    );
    let code = run_cli(&["start", "foo"], &server.base).expect("start 应成功");
    assert_eq!(code, 0);
    assert!(server.hits.load(Ordering::SeqCst) >= 2);
}

/// begin_vnt_start 幂等语义：已 Running/Starting 时 /api/start 返回
/// code:-1「配置 x 正在启动或已运行」——视为成功并查询当前状态。
#[test]
fn start_already_running_is_success() {
    let server = spawn_sequence_server(
        err_envelope("配置 foo.toml 正在启动或已运行"),
        vec![ok_envelope(r#"{"status":"running"}"#)],
    );
    let code = run_cli(&["start", "foo"], &server.base).expect("已在运行须视为成功");
    assert_eq!(code, 0);
}

/// 直接成功路径不受轮询节奏影响：默认（生产）配置下 Running 立即返回。
#[test]
fn start_already_running_uses_default_poll_config() {
    let server = spawn_sequence_server(
        err_envelope("配置 foo.toml 正在启动或已运行"),
        vec![ok_envelope(r#"{"status":"running"}"#)],
    );
    let client = reqwest::blocking::Client::new();
    lifecycle::start(
        &client,
        &server.base,
        None,
        "foo",
        &lifecycle::PollConfig::default(),
    )
    .expect("直接成功路径不得被 30s 轮询阻塞");
}

#[test]
fn start_failure_falls_back_to_stopped_is_error() {
    let server = spawn_sequence_server(
        ok_envelope("null"),
        vec![
            ok_envelope(r#"{"status":"starting"}"#),
            ok_envelope(r#"{"status":"stopped"}"#),
        ],
    );
    let err = run_cli(&["start", "foo"], &server.base).expect_err("启动失败回落须报错");
    assert!(
        err.to_string().contains("启动失败"),
        "错误应说明启动失败: {err}"
    );
}

/// 竞态窗口：Starting 期间状态查询偶发 code:-1，连续 2 次内容忍并最终成功。
#[test]
fn start_tolerates_transient_status_errors() {
    let server = spawn_sequence_server(
        ok_envelope("null"),
        vec![
            ok_envelope(r#"{"status":"starting"}"#),
            err_envelope("实例不存在"),
            err_envelope("实例不存在"),
            ok_envelope(r#"{"status":"running"}"#),
        ],
    );
    let code = run_cli(&["start", "foo"], &server.base).expect("偶发 -1 须被容忍");
    assert_eq!(code, 0);
}

/// 连续 3 次非终态 code:-1：不再容忍。
#[test]
fn start_fails_after_three_consecutive_status_errors() {
    let server = spawn_sequence_server(
        ok_envelope("null"),
        vec![
            ok_envelope(r#"{"status":"starting"}"#),
            err_envelope("x"),
            err_envelope("x"),
            err_envelope("x"),
            ok_envelope(r#"{"status":"running"}"#),
        ],
    );
    let err = run_cli(&["start", "foo"], &server.base).expect_err("连续 3 次 -1 须报错");
    assert!(
        err.to_string().contains("连续"),
        "错误应提及连续失败: {err}"
    );
}

/// 状态始终 Starting：超时（fake 配置 500ms）报错。
#[test]
fn start_timeout_is_error() {
    let server = spawn_sequence_server(
        ok_envelope("null"),
        vec![ok_envelope(r#"{"status":"starting"}"#)],
    );
    let client = reqwest::blocking::Client::new();
    let err = lifecycle::start(&client, &server.base, None, "foo", &FAST)
        .expect_err("始终 Starting 须超时");
    assert!(err.to_string().contains("超时"), "错误应提及超时: {err}");
}

/// 配置文件不存在：/api/start 的 code:-1 原样透出。
#[test]
fn start_missing_config_surfaces_api_error() {
    let server = spawn_sequence_server(err_envelope("Config file not found"), vec![]);
    let err = run_cli(&["start", "foo"], &server.base).expect_err("缺配置须报错");
    assert!(
        err.to_string().contains("Config file not found"),
        "错误应透出 API 文案: {err}"
    );
}

// ---- stop ----

#[test]
fn stop_running_succeeds() {
    let server = spawn_stop_server(ok_envelope("null"));
    let code = run_cli(&["stop", "foo"], &server.base).expect("stop 应成功");
    assert_eq!(code, 0);
    assert_eq!(server.stop_hits.load(Ordering::SeqCst), 1);
}

/// stop 幂等：服务端对已停止实例返回 code:-1 "Vnt stopped"，视为成功。
#[test]
fn stop_already_stopped_is_idempotent_success() {
    let server = spawn_stop_server(err_envelope("Vnt stopped"));
    let code = run_cli(&["stop", "foo"], &server.base).expect("已停止须视为幂等成功");
    assert_eq!(code, 0);
}

/// 其它 code:-1（如「停止网络实例超时」）仍为错误。
#[test]
fn stop_other_error_is_failure() {
    let server = spawn_stop_server(err_envelope("停止网络实例超时"));
    let err = run_cli(&["stop", "foo"], &server.base).expect_err("其它 -1 须报错");
    assert!(
        err.to_string().contains("停止网络实例超时"),
        "错误应透出 API 文案: {err}"
    );
}
