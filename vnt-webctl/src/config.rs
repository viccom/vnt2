//! config 子命令实现（计划 §4 Task 2）。
//!
//! HTTP 调用统一走 [`crate::http::WebClient`]（Task 5 抽出）；
//! 本模块只留业务逻辑：file_name 规范化、订阅 TOML 模板、三个子命令。

use anyhow::{Context, bail};

use crate::http::{WebClient, query_escape};
use crate::{Cli, ConfigAction};

/// file_name 预校验与规范化，与服务端 `normalize_config_file_name`
/// （vnt-web/src/service_http.rs:1892）同逻辑同文案：
/// 无 `..`/`/`/`\`；无扩展名补 `.toml`；非 .toml 扩展名拒绝。
/// CLI 侧必须先做与服务端相同的规范化，否则 `config add dongnet` 落盘为
/// `dongnet.toml` 后，后续 `start dongnet` 与服务端补名对不上。
pub fn normalize_config_name(name: &str) -> Result<String, &'static str> {
    // 与 is_valid_file_name（service_http.rs:1883）同语义
    if name.is_empty() || name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err("Invalid file name");
    }
    match std::path::Path::new(name).extension() {
        None => Ok(format!("{name}.toml")),
        Some(ext) if ext == "toml" => Ok(name.to_string()),
        Some(_) => Err("Config file name must end with .toml"),
    }
}

/// 订阅配置 TOML 模板：config_name + subscription 两键（计划 §4 Task 2 模板）。
/// 值按 TOML 基本字符串规则转义（`"` 与 `\`），保证服务端 TOML 解析不误。
pub fn subscription_toml(config_name: &str, code: &str) -> String {
    fn escape(value: &str) -> String {
        value.replace('\\', "\\\\").replace('"', "\\\"")
    }
    format!(
        "config_name = \"{}\"\nsubscription = \"{}\"\n",
        escape(config_name),
        escape(code)
    )
}

pub fn dispatch(cli: &Cli, client: &WebClient) -> anyhow::Result<()> {
    let crate::Command::Config { action } = &cli.command else {
        bail!("非 config 子命令")
    };
    match action {
        ConfigAction::Add {
            name,
            from_file,
            subscription,
        } => add(client, name, from_file.as_deref(), subscription.as_deref()),
        ConfigAction::List => list(client),
        ConfigAction::Show { name } => show(client, name),
        ConfigAction::Delete { name, yes } => delete(client, name, *yes),
    }
}

fn add(
    client: &WebClient,
    name: &str,
    from_file: Option<&str>,
    subscription: Option<&str>,
) -> anyhow::Result<()> {
    let file_name = normalize_config_name(name).map_err(anyhow::Error::msg)?;
    let config = match (from_file, subscription) {
        (Some(path), None) => {
            std::fs::read_to_string(path).with_context(|| format!("读取配置文件失败: {path}"))?
        }
        (None, Some(code)) => {
            // 预览只读远端、不落盘；失败则不保存（计划 §4 Task 2）
            let preview = client.post(
                "/api/subscription/preview",
                serde_json::json!({ "subscription": code }),
            )?;
            print_preview(&preview);
            subscription_toml(name, code)
        }
        // clap 的 conflicts_with 已挡 (Some, Some)；两者皆缺无意义
        _ => bail!("须指定 --from-file <路径> 或 --subscription <码>"),
    };
    client.post(
        "/api/config",
        serde_json::json!({ "file_name": file_name, "config": config }),
    )?;
    println!("已保存配置 {file_name}");
    Ok(())
}

fn print_preview(preview: &serde_json::Value) {
    let servers = preview
        .get("server")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    println!("server: {servers}");
    for key in ["network_code", "device_id"] {
        println!(
            "{key}: {}",
            preview
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        );
    }
    println!(
        "revision: {}",
        preview
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    );
}

fn list(client: &WebClient) -> anyhow::Result<()> {
    let data = client.get("/api/config/list")?;
    let entries = data.as_array().context("配置列表响应不是数组")?;
    if entries.is_empty() {
        println!("（无已保存配置）");
        return Ok(());
    }
    let rows: Vec<(String, String)> = entries
        .iter()
        .map(|entry| {
            (
                entry
                    .get("file_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                entry
                    .get("config_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        })
        .collect();
    let name_width = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (file_name, config_name) in &rows {
        println!("{file_name:<name_width$}  {config_name}");
    }
    Ok(())
}

fn show(client: &WebClient, name: &str) -> anyhow::Result<()> {
    let file_name = normalize_config_name(name).map_err(anyhow::Error::msg)?;
    let data = client.get(&format!(
        "/api/config?file_name={}",
        query_escape(&file_name)
    ))?;
    let content = data.as_str().context("配置内容响应不是字符串")?;
    print!("{content}");
    if !content.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn delete(client: &WebClient, name: &str, yes: bool) -> anyhow::Result<()> {
    // --yes 门禁在任何网络/校验之前（无 TTY 交互场景，计划 §5）
    if !yes {
        bail!("删除配置须显式确认，请加 --yes");
    }
    let file_name = normalize_config_name(name).map_err(anyhow::Error::msg)?;
    client.delete(&format!(
        "/api/config?file_name={}",
        query_escape(&file_name)
    ))?;
    println!("已删除配置 {file_name}");
    Ok(())
}
