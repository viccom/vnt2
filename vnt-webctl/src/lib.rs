//! vnt2_web 的管理子命令 CLI（busybox 派发入口）。
//!
//! 库自身不 `exit()`：`run_cli` 只解析参数并返回退出码占位，
//! 由 main_web 映射为进程退出码（计划 §4 Task 1 / Task 6）。

use clap::{Parser, Subcommand};

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

/// 解析参数并返回退出码占位（本任务仅骨架，解析成功即返回 0）。
pub fn run_cli(args: impl Iterator<Item = String>) -> anyhow::Result<i32> {
    let _cli = Cli::try_parse_from(args)?;
    Ok(0)
}
