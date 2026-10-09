//! vnt2_web 的管理子命令 CLI（busybox 派发入口）。
//!
//! 库自身不 `exit()`：`run_cli` 解析参数并派发子命令，
//! 由 main_web 把返回码映射为进程退出码（计划 §4 Task 1 / Task 6）。

use clap::{Parser, Subcommand};

pub mod config;
pub mod lifecycle;
pub mod render;
pub mod telemetry;

#[derive(Parser, Debug)]
#[command(name = "vnt2_web", about = "vnt2_web 管理子命令", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 配置管理
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// 启动实例
    Start { name: String },
    /// 停止实例
    Stop { name: String },
    /// 重启实例
    Restart { name: String },
    /// 实例运行状态
    Status { name: String },
    /// 实例信息
    Info { name: String },
    /// 对等节点列表
    Peers { name: String },
    /// 路由表
    Routes { name: String },
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// 添加配置（文件导入或订阅码）
    Add {
        name: String,
        /// 从 TOML 配置文件导入
        #[arg(long, conflicts_with = "subscription")]
        from_file: Option<String>,
        /// 通过订阅码生成配置
        #[arg(long)]
        subscription: Option<String>,
    },
    /// 列出全部配置
    List,
    /// 查看配置内容
    Show { name: String },
    /// 删除配置
    Delete {
        name: String,
        /// 跳过确认（脚本场景）
        #[arg(long)]
        yes: bool,
    },
}

/// 解析参数并派发子命令；成功返回退出码 0。
/// `base` 为服务地址（如 `http://127.0.0.1:19099`），`token` 为可选 Bearer 令牌
/// （Task 6 由 main_web 接线传入）。
pub fn run_cli(
    args: impl Iterator<Item = String>,
    base: &str,
    token: Option<&str>,
) -> anyhow::Result<i32> {
    let cli = Cli::try_parse_from(args)?;
    match &cli.command {
        Command::Config { .. } => config::dispatch(&cli, base, token)?,
        Command::Start { .. }
        | Command::Stop { .. }
        | Command::Restart { .. }
        | Command::Status { .. } => lifecycle::dispatch(&cli, base, token)?,
        Command::Info { .. } | Command::Peers { .. } | Command::Routes { .. } => {
            telemetry::dispatch(&cli, base, token)?
        }
    }
    Ok(0)
}
