//! CLI 参数解析的失败路径测试（计划 §4 Task 1：先红后绿）。
//!
//! 覆盖四类解析错误：非法子命令、缺少必需名、缺少必需参数值、未知 flag。
//! 断言 clap 报错非空，且语义指向用户输错的那个词。

use vnt_webctl::run_cli;

fn parse_err(args: &[&str]) -> String {
    let args: Vec<String> = std::iter::once("vnt2_web".to_string())
        .chain(args.iter().map(|s| s.to_string()))
        .collect();
    run_cli(args.into_iter())
        .expect_err("非法参数必须返回解析错误")
        .to_string()
}

#[test]
fn unknown_subcommand_is_rejected() {
    let err = parse_err(&["bogus"]);
    assert!(!err.is_empty(), "clap 必须产出非空错误信息");
    assert!(err.contains("bogus"), "错误应指向未知子命令词: {err}");
}

#[test]
fn missing_instance_name_is_rejected() {
    // start 缺 <name>
    let err = parse_err(&["start"]);
    assert!(!err.is_empty());
    assert!(
        err.contains("name") || err.contains("NAME"),
        "错误应指出缺少 name 参数: {err}"
    );
}

#[test]
fn missing_flag_value_is_rejected() {
    // config add 的 --from-file 缺路径值
    let err = parse_err(&["config", "add", "foo", "--from-file"]);
    assert!(!err.is_empty());
    assert!(err.contains("--from-file"), "错误应指向缺值的 flag: {err}");
}

#[test]
fn unknown_flag_is_rejected() {
    let err = parse_err(&["status", "foo", "--nope"]);
    assert!(!err.is_empty());
    assert!(err.contains("--nope"), "错误应指向未知 flag: {err}");
}
