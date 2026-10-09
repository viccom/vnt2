//! file_name 规范化测试（计划 §4 Task 2 复核钉死：先红后绿）。
//!
//! CLI 侧预校验必须与服务端 `normalize_config_file_name`
//! （vnt-web/src/service_http.rs:1892）同逻辑同文案，否则
//! `config add dongnet` 与服务端补名后的 `dongnet.toml` 在后续
//! `start dongnet` 时对不上。

use vnt_webctl::config::normalize_config_name;

#[test]
fn bare_name_gets_toml_suffix() {
    assert_eq!(
        normalize_config_name("dongnet"),
        Ok("dongnet.toml".to_string())
    );
}

#[test]
fn toml_name_passes_through() {
    assert_eq!(
        normalize_config_name("dongnet.toml"),
        Ok("dongnet.toml".to_string())
    );
}

#[test]
fn path_traversal_is_rejected() {
    for name in ["../etc/passwd", "a/b", "a\\b", "..", ""] {
        assert!(
            normalize_config_name(name).is_err(),
            "路径穿越或空名必须被拒绝: {name:?}"
        );
    }
}

#[test]
fn non_toml_extension_is_rejected_with_server_message() {
    assert_eq!(
        normalize_config_name("foo.txt"),
        Err("Config file name must end with .toml")
    );
}
