//! 输出层纯函数测试（计划 §4 Task 4；先红后绿）。

use vnt_webctl::render;

// ---- format_bytes ----

#[test]
fn bytes_boundary_values() {
    let cases: &[(u64, &str)] = &[
        (0, "0 B"),
        (1, "1 B"),
        (1023, "1023 B"),
        (1024, "1.0 KB"),
        (1536, "1.5 KB"),
        (1024 * 1024, "1.0 MB"),
        (1536 * 1024 * 1024, "1.5 GB"),
        (1024 * 1024 * 1024, "1.0 GB"),
        (5 * 1024u64.pow(4), "5.0 TB"),
    ];
    for (input, expected) in cases {
        assert_eq!(render::format_bytes(*input), *expected, "input={input}");
    }
}

// ---- format_mode ----

#[test]
fn mode_seven_mappings() {
    // 七种映射全覆盖：lan、四种打洞、节点中继、服务器中继
    let cases: &[(&str, &str)] = &[
        ("lan", "局域网直连"),
        ("udp-v4", "打洞直连"),
        ("udp-v6", "打洞直连"),
        ("tcp-v4", "打洞直连"),
        ("tcp-v6", "打洞直连"),
        ("direct", "直连"),
        ("node-relay", "节点中继"),
        ("relay", "服务器中继"),
    ];
    for (input, expected) in cases {
        assert_eq!(render::format_mode(input), *expected, "input={input}");
    }
}

#[test]
fn mode_unknown_passthrough() {
    assert_eq!(render::format_mode("wireguard"), "wireguard");
    assert_eq!(render::format_mode(""), "");
}

// ---- opt ----

#[test]
fn opt_some_and_none() {
    assert_eq!(render::opt(Some("10.26.0.1")), "10.26.0.1");
    assert_eq!(render::opt::<&str>(None), "-");
    assert_eq!(render::opt(Some(1400u16)), "1400");
    assert_eq!(render::opt::<u16>(None), "-");
}

// ---- table ----

#[test]
fn table_aligns_columns() {
    let out = render::table(
        &["IP", "名称"],
        &[
            vec!["10.26.0.1".to_string(), "甲".to_string()],
            vec!["10.26.0.22".to_string(), "乙丙丁".to_string()],
        ],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "表头 + 两行: {out:?}");
    // 第一列宽由最长行（10.26.0.22 = 11 字符）决定，行间两空格分隔
    assert_eq!(lines[0], "IP          名称");
    assert_eq!(lines[1], "10.26.0.1   甲");
    assert_eq!(lines[2], "10.26.0.22  乙丙丁");
}

#[test]
fn table_cjk_width_counts_two() {
    // 中文字符宽度按 2 计：「乙」（1 字 = 2 列宽）比 "x"（1 列宽）长，
    // 第一列须按「乙」的 2 列宽对齐后续列
    let out = render::table(
        &["名", "值"],
        &[
            vec!["x".to_string(), "1".to_string()],
            vec!["乙".to_string(), "2".to_string()],
        ],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "名  值"); // 「名」宽 2，与「乙」同宽，补 2 空格
    assert_eq!(lines[1], "x   1"); // "x" 宽 1，补 1+2=3 空格
    assert_eq!(lines[2], "乙  2");
}
