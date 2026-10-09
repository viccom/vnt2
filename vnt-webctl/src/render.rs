//! 输出层纯函数：单位换算、连接模型映射、Option 展示、列宽对齐表格
//! （计划 §4 Task 4）。
//!
//! 不做 unicode-width 依赖：显示宽度按「ASCII 计 1、其余计 2」的字符级
//! 近似——覆盖本 CLI 的中英混排场景，不为 exotic 脚本引入依赖。

use std::fmt::Display;

/// 1024 进制字节换算，1 位小数：1023→"1023 B"、1024→"1.0 KB"、
/// 1536→"1.5 KB"、0→"0 B"。整数倍单位仍打 `.0`（对齐列宽稳定）。
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// 连接模型 → 中文标签（口径见 vnt-core LinkModel::as_str 与上报 model 字段）。
/// 未知值原样透传（防协议演进踢死展示）。
pub fn format_mode(mode: &str) -> &str {
    match mode {
        "lan" => "局域网直连",
        "udp-v4" | "udp-v6" | "tcp-v4" | "tcp-v6" => "打洞直连",
        "direct" => "直连",
        "node-relay" => "节点中继",
        "relay" => "服务器中继",
        other => other,
    }
}

/// Option 展示：Some→值、None→"-"（Stopped 实例全 null 字段的展示基础）。
pub fn opt<T: Display>(value: Option<T>) -> String {
    value.map_or_else(|| "-".to_string(), |v| v.to_string())
}

/// 字符级显示宽度近似：ASCII 计 1，其余计 2。
fn display_width(text: &str) -> usize {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// 列宽对齐表格：表头一行 + 每行各列按该列最大显示宽度左对齐，
/// 列间两空格分隔。末列不做尾部填充。
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| display_width(h)).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(display_width(cell));
            }
        }
    }
    let mut out = String::new();
    let render_row = |cells: &[String], out: &mut String| {
        let last = cells.len().saturating_sub(1);
        for (index, cell) in cells.iter().enumerate() {
            out.push_str(cell);
            if index < last {
                let padding = widths[index].saturating_sub(display_width(cell)) + 2;
                out.push_str(&" ".repeat(padding));
            }
        }
        out.push('\n');
    };
    let header_cells: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    render_row(&header_cells, &mut out);
    for row in rows {
        render_row(row, &mut out);
    }
    out
}
