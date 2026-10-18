use anyhow::{Context, bail};
use clap::Parser;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
mod extract_wintun_dll;
#[cfg(windows)]
mod web_service;

/// VNT Web 服务。
#[derive(Parser, Clone, Debug)]
#[command(author, version, about, long_about = None)]
pub(crate) struct Args {
    /// 本地 HTTP 服务监听地址。
    #[arg(long)]
    addr: Option<SocketAddr>,
    /// 加载 VNT 配置路径，配置内容参考 Web 端的配置格式。
    #[arg(long)]
    conf: Option<PathBuf>,
    /// Web API 访问令牌；未指定时会生成随机令牌并输出到日志。
    #[arg(long, env = "VNT_WEB_TOKEN")]
    token: Option<String>,
    /// 强制所有来源（含 loopback）必须携带 Bearer 令牌；
    /// 默认关闭，即本机 loopback 来源免 token。
    #[arg(long, env = "VNT_WEB_AUTH_STRICT")]
    auth_strict: bool,
    /// 读取桌面端保存的内核设置。
    #[arg(long, hide = true)]
    desktop_config: Option<PathBuf>,
    /// 由 Windows 服务控制管理器启动。
    #[cfg(windows)]
    #[arg(long, hide = true)]
    service: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub(crate) struct DesktopKernelConfig {
    listen_address: String,
    port: u16,
    token: String,
}

impl Default for DesktopKernelConfig {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1".to_string(),
            port: 19099,
            token: String::new(),
        }
    }
}

impl DesktopKernelConfig {
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(
            &std::fs::read_to_string(path)
                .with_context(|| format!("读取桌面端内核设置失败：{}", path.display()))?,
        )
        .context("解析桌面端内核设置失败")?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        let _: std::net::IpAddr = self
            .listen_address
            .parse()
            .context("桌面端内核监听地址无效")?;
        if self.port == 0 {
            bail!("桌面端内核监听端口不能为 0");
        }
        if self.token.len() < 16 {
            bail!("桌面端内核访问令牌至少需要 16 个字符");
        }
        Ok(())
    }

    fn socket_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(SocketAddr::new(self.listen_address.parse()?, self.port))
    }
}

/// CLI 子命令白名单（裸词精确匹配；`--xxx` flag 永不误入，
/// 计划 §4 Task 6）。`serve` 故意不注册为子命令、但列入白名单——让它被
/// 误派发到 CLI 侧报「未知子命令」而非静默走服务模式，防歧义。
const CLI_SUBCOMMANDS: &[&str] = &[
    "config", "start", "stop", "restart", "status", "info", "peers", "routes", "help", "serve",
];

fn main() {
    // 派发必须在 Args::parse() 之前：clap 遇未知裸词（如 `config`）会直接
    // 报错退出（计划 §4 Task 6 复核钉死）。服务模式派生入口共 4 处、均以
    // `--flag` 打头（计划 §1 论断 2），故首个裸词即 CLI 子命令候选；
    // 带值 flag（--addr/--token/--conf/--desktop_config）的值须跳过，
    // 防 `--conf config` 之类值与白名单词相撞。
    if let Some(subcommand) = first_bare_word()
        && CLI_SUBCOMMANDS.contains(&subcommand.as_str())
    {
        std::process::exit(run_cli_mode());
    }
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

/// 服务模式带值 flag（其后的 argv 项是值、不是子命令候选）。
/// `--service`（windows）是 bool flag 不带值，不在此列。
const VALUE_FLAGS: &[&str] = &["--addr", "--token", "--conf", "--desktop_config"];

/// 首个裸词（非 `--flag`、非带值 flag 的值）；无则 None（纯 flag 的服务模式）。
fn first_bare_word() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if VALUE_FLAGS.contains(&arg.as_str()) {
            args.next(); // 跳过 flag 值
        } else if !arg.starts_with('-') {
            return Some(arg);
        }
        // `--flag=value` 与 bool flag：无后续值可跳
    }
    None
}

/// CLI 模式：从原始 argv 解析 `--addr`/`--token`（与服务模式同 flag 名），
/// 其余参数原样交 vnt-webctl。`--addr` 默认与服务模式默认绑定一致。
fn run_cli_mode() -> i32 {
    let mut base = "127.0.0.1:19099".to_string();
    let mut token: Option<String> = None;
    let mut cli_args: Vec<String> = vec!["vnt2_web".to_string()];
    let mut rest = std::env::args().skip(1).peekable();
    while let Some(arg) = rest.next() {
        if arg == "--addr" {
            match rest.next() {
                Some(value) => base = value,
                None => {
                    eprintln!("--addr 缺少参数值");
                    return 1;
                }
            }
        } else if let Some(value) = arg.strip_prefix("--addr=") {
            base = value.to_string();
        } else if arg == "--token" {
            match rest.next() {
                Some(value) => token = Some(value),
                None => {
                    eprintln!("--token 缺少参数值");
                    return 1;
                }
            }
        } else if let Some(value) = arg.strip_prefix("--token=") {
            token = Some(value.to_string());
        } else {
            cli_args.push(arg);
        }
    }
    match vnt_webctl::run_cli(cli_args.into_iter(), &base, token.as_deref()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}

fn run() -> anyhow::Result<()> {
    let args = Args::parse();

    #[cfg(windows)]
    {
        if args.service {
            return web_service::dispatch(args);
        }
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建异步运行时失败")?
        .block_on(run_server(args, None))
}

pub(crate) async fn run_server(
    mut args: Args,
    service_cancellation: Option<CancellationToken>,
) -> anyhow::Result<()> {
    if let Some(config_path) = args.desktop_config.as_deref() {
        let config = DesktopKernelConfig::load(config_path)?;
        args.addr = Some(config.socket_addr()?);
        args.token = Some(config.token);
        let data_dir = config_path
            .parent()
            .context("桌面端内核设置路径缺少父目录")?;
        std::fs::create_dir_all(data_dir)?;
        std::env::set_current_dir(data_dir)
            .with_context(|| format!("切换数据目录失败：{}", data_dir.display()))?;
    }

    vnt2::log::log_init("vnt2-web");
    log::info!("version: {:?}", env!("CARGO_PKG_VERSION"));
    #[cfg(windows)]
    extract_wintun_dll::extract_wintun();

    let addr = args.addr.unwrap_or("127.0.0.1:19099".parse()?);
    let token = args.token.unwrap_or_else(vnt_web::generate_access_token);
    let browser_host = if addr.ip().is_unspecified() {
        "127.0.0.1".to_string()
    } else {
        addr.ip().to_string()
    };
    log::info!("Web access token: {}", token);
    log::info!(
        "Web access URL: http://{}:{}/?token={}",
        browser_host,
        addr.port(),
        token
    );

    let service = vnt_web::VntService::new(args.conf).await?;
    let cancellation = CancellationToken::new();
    let auth = vnt_web::AuthConfig::new(token, args.auth_strict);
    let handle = service.start_http(addr, auth, cancellation.clone()).await?;

    if let Some(service_cancellation) = service_cancellation {
        service_cancellation.cancelled().await;
    } else {
        shutdown_signal().await?;
    }
    cancellation.cancel();
    handle.await??;
    Ok(())
}

async fn shutdown_signal() -> anyhow::Result<()> {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .context("failed to install Ctrl+C handler")
    };

    #[cfg(unix)]
    let terminate = async {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context("failed to install terminate signal handler")?;
        signal.recv().await;
        Ok::<(), anyhow::Error>(())
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<anyhow::Result<()>>();

    tokio::select! {
        result = ctrl_c => result?,
        result = terminate => result?,
    }
    Ok(())
}
