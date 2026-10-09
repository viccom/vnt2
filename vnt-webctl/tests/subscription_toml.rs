//! 订阅配置 TOML 模板生成测试（计划 §4 Task 2：先红后绿）。
//!
//! 模板为纯函数，产出两键 TOML；值含 `"` 或 `\` 时必须按 TOML
//! 基本字符串规则转义，否则落盘的配置无法被服务端 TOML 解析。

use vnt_webctl::config::subscription_toml;

#[test]
fn generates_basic_template() {
    let toml = subscription_toml("dongnet", "vnt2://join/abc123");
    assert_eq!(
        toml,
        "config_name = \"dongnet\"\nsubscription = \"vnt2://join/abc123\"\n"
    );
}

#[test]
fn escapes_quotes_and_backslashes() {
    let toml = subscription_toml("my\"net", "vnt2://join/a\\b\"c");
    assert_eq!(
        toml,
        "config_name = \"my\\\"net\"\nsubscription = \"vnt2://join/a\\\\b\\\"c\"\n"
    );
}
