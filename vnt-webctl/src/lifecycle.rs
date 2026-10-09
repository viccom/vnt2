//! 实例生命周期命令：start / stop / restart / status（计划 §4 Task 3）。
//!
//! start 的幂等与竞态按「复核钉死」处理：
//! - `begin_vnt_start` 在实例已 Running/Starting 时返回
//!   code:-1「配置 x 正在启动或已运行」（service_http.rs `starting`）——
//!   视为成功并查询当前状态，不误判为失败
//! - `Starting` 窗口内 /api/start/status 偶发 code:-1（启动竞态）——
//!   连续非终态错误至多容忍 [`MAX_CONSECUTIVE_ERRORS`] 次再报错

use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use crate::Cli;
use crate::config::normalize_config_name;
use crate::http::{WebClient, query_escape};

/// 连续非终态 code:-1 的容忍上限（Starting 竞态窗口）。
const MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// `begin_vnt_start` 幂等分支的错误文案特征（service_http.rs:143
/// 「配置 {} 正在启动或已运行」）。
const ALREADY_ACTIVE_HINT: &str = "正在启动或已运行";

/// stop 幂等分支：服务端对已停止实例返回 code:-1 "Vnt stopped"
/// （service_http.rs:1974），视为成功。
const STOPPED_HINT: &str = "Vnt stopped";

/// 启动轮询节奏。默认生产值；测试注入快速值避免拖慢套件。
#[derive(Clone, Copy, Debug)]
pub struct PollConfig {
    /// 每轮间隔
    pub interval: Duration,
    /// 总超时（达到即报错，见 poll_step 的超时优先语义）
    pub timeout: Duration,
}

impl Default for PollConfig {
    fn default() -> Self {
        // 测试经环境变量注入快速节奏（tests/lifecycle_e2e.rs）；
        // 生产环境不设这两个变量，落 1s/30s 默认值
        let interval_ms = std::env::var("VNT_WEBCTL_POLL_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        let timeout_ms = std::env::var("VNT_WEBCTL_POLL_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30_000);
        Self {
            interval: Duration::from_millis(interval_ms),
            timeout: Duration::from_millis(timeout_ms),
        }
    }
}

/// 轮询单次观测到的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollStatus {
    Stopped,
    Starting,
    Running,
    /// 非终态 code:-1（Starting 竞态窗口的状态查询失败）
    TransientError,
}

/// poll_step 的输入快照。
#[derive(Clone, Copy, Debug)]
pub struct PollStep {
    pub status: PollStatus,
    /// 本轮之前是否已观察到过 Starting
    pub saw_starting: bool,
    pub elapsed_ms: u64,
    /// 截至本轮的连续非终态错误次数（本轮为 TransientError 时已含本轮）
    pub consecutive_errors: u32,
}

/// poll_step 的决策。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollDecision {
    /// 继续等待下一轮
    Wait,
    /// 启动成功（Running）
    DoneRunning,
    /// 从未进入过 Starting 即 Stopped：没有可观察的启动，按成功退出
    DoneStopped,
    /// Starting 回落 Stopped：启动失败
    DoneFailed,
    /// 达到超时（优先级高于当轮状态）
    FailTimeout,
    /// 连续非终态错误达到上限
    FailTransient,
}

/// 轮询步进决策（纯函数，决策表见 tests/lifecycle_poll.rs）。
///
/// 超时判定优先于当轮状态与连续错误，保证循环必然终止；
/// 调用方保证 TransientError 轮的 consecutive_errors 已含本轮。
pub fn poll_step(step: PollStep) -> PollDecision {
    // poll_loop 以 elapsed >= timeout 为循环退出条件，到达此处即本轮已超时
    if step.elapsed_ms >= PollConfig::default().timeout.as_millis() as u64 {
        return PollDecision::FailTimeout;
    }
    match step.status {
        PollStatus::Running => PollDecision::DoneRunning,
        PollStatus::Stopped => {
            if step.saw_starting {
                PollDecision::DoneFailed
            } else {
                PollDecision::DoneStopped
            }
        }
        PollStatus::Starting => PollDecision::Wait,
        PollStatus::TransientError => {
            if step.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                PollDecision::FailTransient
            } else {
                PollDecision::Wait
            }
        }
    }
}

/// 从 /api/start/status 响应解析 VntStatus；服务端序列为小写
/// （VntStatus `rename_all = "lowercase"`，service_http.rs:49）。
/// 未知值按非终态错误处理（容忍窗口与 code:-1 一致，防字段漂移踢死轮询）。
fn parse_status(data: &serde_json::Value) -> PollStatus {
    match data.get("status").and_then(serde_json::Value::as_str) {
        Some("stopped") => PollStatus::Stopped,
        Some("starting") => PollStatus::Starting,
        Some("running") => PollStatus::Running,
        _ => PollStatus::TransientError,
    }
}

fn status_path(file_name: &str) -> String {
    format!("/api/start/status?file_name={}", query_escape(file_name))
}

fn fetch_status(client: &WebClient, file_name: &str) -> anyhow::Result<PollStatus> {
    let data = client.get(&status_path(file_name))?;
    Ok(parse_status(&data))
}

fn status_label(status: PollStatus) -> &'static str {
    match status {
        PollStatus::Stopped => "Stopped",
        PollStatus::Starting => "Starting",
        PollStatus::Running => "Running",
        PollStatus::TransientError => "查询失败",
    }
}

/// 轮询 /api/start/status 到终态（计划 §4 Task 3）。
///
/// 每轮打印状态行；Running 成功、Starting 回落 Stopped 报错、超时报错、
/// 连续非终态 code:-1 达上限报错。返回终态供「已在运行」分支复用输出。
fn poll_until_terminal(
    client: &WebClient,
    name: &str,
    file_name: &str,
    config: &PollConfig,
) -> anyhow::Result<PollStatus> {
    let begin = Instant::now();
    let mut saw_starting = false;
    let mut consecutive_errors = 0u32;
    loop {
        let observed = match fetch_status(client, file_name) {
            Ok(status) => {
                consecutive_errors = 0;
                status
            }
            Err(error) => {
                consecutive_errors += 1;
                println!("状态查询失败（连续第 {consecutive_errors} 次）: {error:#}");
                PollStatus::TransientError
            }
        };
        if observed == PollStatus::Starting {
            saw_starting = true;
        }
        let decision = poll_step(PollStep {
            status: observed,
            saw_starting,
            elapsed_ms: begin.elapsed().as_millis() as u64,
            consecutive_errors,
        });
        match decision {
            PollDecision::Wait => {
                if observed != PollStatus::TransientError {
                    println!("实例 {name} 状态: {}", status_label(observed));
                }
                std::thread::sleep(config.interval);
            }
            PollDecision::DoneRunning => {
                println!("实例 {name} 状态: Running");
                return Ok(PollStatus::Running);
            }
            PollDecision::DoneStopped => {
                println!("实例 {name} 状态: Stopped（未观察到启动过程）");
                return Ok(PollStatus::Stopped);
            }
            PollDecision::DoneFailed => {
                bail!(
                    "实例 {name} 启动失败（状态已回落 Stopped，\
                     详见 web 界面实例日志）"
                );
            }
            PollDecision::FailTimeout => bail!(
                "等待实例 {name} 启动超时（{}s），当前状态: {}",
                config.timeout.as_secs(),
                status_label(observed)
            ),
            PollDecision::FailTransient => {
                bail!("实例 {name} 状态查询连续失败 {consecutive_errors} 次，放弃等待")
            }
        }
    }
}

/// `start <名>`：POST /api/start 后轮询到终态。
///
/// 已在运行/正在启动（begin_vnt_start 幂等分支）视为成功，打印当前状态。
pub fn start(client: &WebClient, name: &str, config: &PollConfig) -> anyhow::Result<()> {
    let file_name = normalize_config_name(name).map_err(anyhow::Error::msg)?;
    let body = serde_json::json!({ "file_name": file_name });
    match client.post("/api/start", body.clone()) {
        Ok(_) => {
            poll_until_terminal(client, name, &file_name, config)?;
            Ok(())
        }
        Err(error) if error.to_string().contains(ALREADY_ACTIVE_HINT) => {
            // begin_vnt_start 幂等分支（复核钉死）：已在运行/启动中不算失败
            println!("实例 {name} 已在运行或正在启动");
            let status = fetch_status(client, &file_name).context("查询当前状态失败")?;
            println!("实例 {name} 状态: {}", status_label(status));
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// `stop <名>`：POST /api/stop。已停止实例的 code:-1 "Vnt stopped"
/// 视为幂等成功；其它 code:-1 原样报错。
pub fn stop(client: &WebClient, name: &str) -> anyhow::Result<()> {
    let file_name = normalize_config_name(name).map_err(anyhow::Error::msg)?;
    let body = serde_json::json!({ "file_name": file_name });
    match client.post("/api/stop", body) {
        Ok(_) => println!("已停止实例 {name}"),
        Err(error) if error.to_string().contains(STOPPED_HINT) => {
            println!("实例 {name} 已处于停止状态");
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// `restart <名>`：POST /api/restart（服务端异步启动，不回传终态，
/// 与 web 界面行为一致；观察启动结果用 `status`）。
pub fn restart(client: &WebClient, name: &str) -> anyhow::Result<()> {
    let file_name = normalize_config_name(name).map_err(anyhow::Error::msg)?;
    let body = serde_json::json!({ "file_name": file_name });
    client.post("/api/restart", body)?;
    println!("已重启实例 {name}");
    Ok(())
}

/// `status <名>`：打印状态；Running 时附加 /api/info 关键摘要。
pub fn status(client: &WebClient, name: &str) -> anyhow::Result<()> {
    let file_name = normalize_config_name(name).map_err(anyhow::Error::msg)?;
    let current = fetch_status(client, &file_name)?;
    println!("实例 {name} 状态: {}", status_label(current));
    if current != PollStatus::Running {
        return Ok(());
    }
    let info = client.get(&format!("/api/info?file_name={}", query_escape(&file_name)))?;
    let get_str = |key: &str| {
        info.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let get_num = |key: &str| {
        info.get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    println!("虚拟 IP: {}", get_str("ip"));
    println!("组网编号: {}", get_str("network_code"));
    println!(
        "在线对端: {}（直连 {}）",
        get_num("online_client_num"),
        get_num("direct_client_num")
    );
    Ok(())
}

pub fn dispatch(cli: &Cli, client: &WebClient) -> anyhow::Result<()> {
    match &cli.command {
        crate::Command::Start { name } => start(client, name, &PollConfig::default()),
        crate::Command::Stop { name } => stop(client, name),
        crate::Command::Restart { name } => restart(client, name),
        crate::Command::Status { name } => status(client, name),
        _ => bail!("非生命周期子命令"),
    }
}
