//! wss 握手配置的公共解析：路径归一与 "Name: Value" header 列表。
//!
//! 调用方有三处（CLI 参数、vnt-web 启动配置、订阅链接/信封），必须共用
//! 同一套规则，否则同一份配置在不同入口得到不同结果——审查中已发现
//! 客户端链接解析弱于握手约束、两处解析函数逐字节重复的问题。

/// 规范化 wss 握手路径：空串表示未配置（由内核 build_wss_request 归一为
/// "/"），非空必须以 '/' 开头且只含可见 ASCII。
///
/// 不以 '/' 开头时，客户端会拼出 `wss://host:443no-leading-slash`（主机名
/// 被污染）；空白/控制符/非 ASCII 会让 URI 解析失败或语义漂移。
pub fn normalize_wss_path(raw: Option<&str>) -> anyhow::Result<String> {
    let path = raw.unwrap_or("").trim();
    if path.is_empty() {
        return Ok(String::new());
    }
    if !path.starts_with('/') {
        anyhow::bail!("wss_path 必须以 / 开头，得到 {path:?}");
    }
    if !path.bytes().all(|byte| byte.is_ascii_graphic()) {
        anyhow::bail!("wss_path 只能包含可见 ASCII 字符，得到 {path:?}");
    }
    Ok(path.to_string())
}

/// 解析单条 "Name: Value" 形式的 wss header。
///
/// 名/值 trim 后名称非空，且必须能通过 http::HeaderName/HeaderValue 校验——
/// 与握手构建（`build_wss_request`）的约束逐字节等价，避免保存通过而
/// 设备握手时才失败。空值合法（HeaderValue 接受），仅空名拒绝。
pub fn parse_wss_header_entry(entry: &str) -> anyhow::Result<(String, String)> {
    let (name, value) = entry
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("wss_headers 条目 '{entry}' 无效：应为 \"Name: Value\""))?;
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() {
        anyhow::bail!("wss_headers 条目 '{entry}' 无效：header 名为空");
    }
    if let Err(error) = http::HeaderName::from_bytes(name.as_bytes()) {
        anyhow::bail!("wss_headers 条目 '{entry}' 无效：header 名不合法（{error}）");
    }
    if let Err(error) = http::HeaderValue::from_str(value) {
        anyhow::bail!("wss_headers 条目 '{entry}' 无效：header 值不合法（{error}）");
    }
    Ok((name.to_string(), value.to_string()))
}

/// 严格解析 header 列表（签发物/显式配置：任一非法即报错）。
pub fn parse_wss_headers(headers: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    headers
        .iter()
        .map(|entry| parse_wss_header_entry(entry))
        .collect()
}

/// 宽松解析 header 列表（订阅信封热更新：逐项跳过非法条目并告警）。
pub fn parse_wss_headers_lenient(headers: &[String]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|entry| match parse_wss_header_entry(entry) {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                log::warn!("忽略无效的 wss header 条目: {error}");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_normalization_accepts_empty_and_absolute_paths() {
        assert_eq!(normalize_wss_path(None).unwrap(), "");
        assert_eq!(normalize_wss_path(Some("")).unwrap(), "");
        assert_eq!(normalize_wss_path(Some("  ")).unwrap(), "");
        assert_eq!(normalize_wss_path(Some("/vnts/")).unwrap(), "/vnts/");
        assert_eq!(normalize_wss_path(Some(" /gw?p=1 ")).unwrap(), "/gw?p=1");
    }

    #[test]
    fn path_normalization_rejects_malformed_paths() {
        for bad in ["no-leading-slash", "/a b", "/a\nb", "/a\u{7f}", "/路径"] {
            assert!(normalize_wss_path(Some(bad)).is_err(), "应拒绝: {bad:?}");
        }
    }

    #[test]
    fn header_entry_requires_handshake_compatible_name_and_value() {
        // 合法：值内冒号/分号/tab/非 ASCII（obs-text）均允许；空值也合法
        for good in [
            "Cookie: a=1; b=2",
            "Authorization: Bearer x:y:z",
            "X-Ok: v\tv",
            "X-Ok: 中文",
            "X-Empty: ",
        ] {
            assert!(parse_wss_header_entry(good).is_ok(), "应放行: {good:?}");
        }
        // 非法：名非 token、值含控制字符、空名/无冒号
        for bad in [
            "no-colon",
            ": v",
            "Bad Name: v",
            "X@Bad: v",
            "X(Bad: v",
            "X-Ok: v\u{7f}",
            "X-Ok: v\nv",
            "X-Ok: v\u{0}",
        ] {
            assert!(parse_wss_header_entry(bad).is_err(), "应拒绝: {bad:?}");
        }
    }

    #[test]
    fn strict_and_lenient_lists_differ_only_in_error_handling() {
        let entries = vec![
            "Cookie: a=1".to_string(),
            "no-colon".to_string(),
            "X-B: c".to_string(),
        ];
        let strict = parse_wss_headers(&entries).unwrap_err();
        assert!(strict.to_string().contains("no-colon"));
        assert_eq!(
            parse_wss_headers_lenient(&entries),
            vec![
                ("Cookie".to_string(), "a=1".to_string()),
                ("X-B".to_string(), "c".to_string()),
            ]
        );
    }
}
