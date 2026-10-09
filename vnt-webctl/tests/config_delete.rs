//! config delete 的 --yes 门禁测试（计划 §5：破坏性命令强制 --yes；先红后绿）。
//!
//! 无 --yes 时必须在发起任何 HTTP 请求之前就报错退出（纯参数校验），
//! 因此 base 指向无监听端口（连接即拒），若实现误发请求测试会挂或报连接错。

use vnt_webctl::run_cli;

#[test]
fn delete_without_yes_fails_before_http() {
    let args: Vec<String> = ["vnt2_web", "config", "delete", "foo"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let err = run_cli(args.into_iter(), "http://127.0.0.1:1", None).expect_err("无 --yes 必须报错");
    assert!(
        err.to_string().contains("--yes"),
        "错误应提示加 --yes: {err}"
    );
}
