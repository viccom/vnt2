//! vnt2_web 生命周期子命令的冒烟驱动（Task 3 联调用，非产品代码）。
//!
//! Task 6 尚未把子命令派发接进 main_web，此处直接调 vnt_webctl::run_cli
//! 驱动同一代码路径；进程退出码即 run_cli 返回码（Err → 1）。

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: webctl_smoke <base> <subcommand> [args...]（token 取 VNT_WEB_TOKEN 环境变量）"
        );
        std::process::exit(2);
    }
    let base = args[1].clone();
    let token = std::env::var("VNT_WEB_TOKEN").ok();
    let cli_args = std::iter::once("vnt2_web".to_string()).chain(args[2..].iter().cloned());
    match vnt_webctl::run_cli(cli_args, &base, token.as_deref()) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }
}
