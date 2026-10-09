//! HTTP 客户端与错误映射（计划 §4 Task 5）。
//!
//! WebClient 统一收口 base 解析、token 注入、`{code,msg,data}` 包裹展开
//! 与 WebError 三分类；config/lifecycle/telemetry 三个模块只拿 `&WebClient`，
//! 不再各自拼装 URL 与 Bearer 头。

use serde_json::Value;

/// Web API 调用的三类失败（run_cli 统一映射为退出码 1，文案各异）。
#[derive(Debug)]
pub enum WebError {
    /// `{code:-1,msg}`：服务端业务错误（透传 msg）
    Api(String),
    /// HTTP 非 2xx（状态码）
    Http(u16),
    /// 连接失败/非法 base（带启动提示）
    Connect(String),
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebError::Api(msg) => write!(f, "{msg}"),
            WebError::Http(status) => write!(f, "HTTP 错误: {status}"),
            WebError::Connect(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for WebError {}

/// 连接失败的统一提示（复核钉死：须提及两种合法启动形态）。
const CONNECT_HINT: &str =
    "服务未运行或无法连接（须以 --token 固定令牌启动，或本机 loopback 免 token 模式）";

/// token 优先序：--token > VNT_WEB_TOKEN env > None（loopback 免 token）。
/// env 为空串视为未设置（防 `VNT_WEB_TOKEN=` 空导出误注入空 Bearer）。
pub fn resolve_token(cli_token: Option<&str>) -> Option<String> {
    cli_token.map(str::to_string).or_else(|| {
        std::env::var("VNT_WEB_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
    })
}

/// vnt2_web 管理面 HTTP 客户端（阻塞，CLI 短命进程）。
#[derive(Debug)]
pub struct WebClient {
    base: String,
    token: Option<String>,
    client: reqwest::blocking::Client,
}

impl WebClient {
    /// base 解析（复核钉死）：
    /// - `19099`（裸端口）→ `http://127.0.0.1:19099`
    /// - `127.0.0.1:19099`（host:port）→ 补 `http://` scheme
    /// - `http://...` / `https://...` → 原样
    /// - 统一 trim_end_matches('/') 防与 path 拼接出双斜杠
    /// - 非法输入报 WebError::Connect 并回显用户输入
    pub fn new(base: &str, token: Option<String>) -> Result<Self, WebError> {
        let invalid = || WebError::Connect(format!("非法服务地址: {base:?}"));
        let trimmed = base.trim();
        let with_scheme = if trimmed.is_empty() {
            return Err(invalid());
        } else if trimmed.chars().all(|c| c.is_ascii_digit()) {
            format!("http://127.0.0.1:{trimmed}")
        } else if let Some(rest) = trimmed
            .strip_prefix("http://")
            .or_else(|| trimmed.strip_prefix("https://"))
        {
            // 剥 scheme 后须剩 host（防 "http://" 经 trim 坍缩成裸 "http:"）
            if rest.trim_end_matches('/').is_empty() {
                return Err(invalid());
            }
            trimmed.to_string()
        } else if trimmed.contains(':')
            && trimmed
                .rsplit(':')
                .next()
                .is_some_and(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
        {
            format!("http://{trimmed}")
        } else {
            return Err(invalid());
        };
        let base = with_scheme.trim_end_matches('/').to_string();
        Ok(Self {
            base,
            token,
            client: reqwest::blocking::Client::new(),
        })
    }

    /// 规范化后的 base（测试锁定解析结果用）。
    pub fn base(&self) -> &str {
        &self.base
    }

    fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, WebError> {
        let url = format!("{}{}", self.base, path);
        let mut request = self.client.request(method, url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .map_err(|_| WebError::Connect(CONNECT_HINT.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(WebError::Http(status.as_u16()));
        }
        let body = response
            .text()
            .map_err(|_| WebError::Connect(CONNECT_HINT.to_string()))?;
        let trimmed = body.trim();
        // 空 body / 非 JSON 的 2xx 响应：按服务端恒有包裹的约定视为协议错误，
        // 归为 Connect（连接层拿到了 2xx 但内容不可解析）
        let value = serde_json::from_str::<Value>(trimmed)
            .map_err(|_| WebError::Connect(CONNECT_HINT.to_string()))?;
        unwrap_envelope(value)
    }

    pub fn get(&self, path_and_query: &str) -> Result<Value, WebError> {
        self.call(reqwest::Method::GET, path_and_query, None)
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value, WebError> {
        self.call(reqwest::Method::POST, path, Some(body))
    }

    pub fn delete(&self, path_and_query: &str) -> Result<Value, WebError> {
        self.call(reqwest::Method::DELETE, path_and_query, None)
    }
}

/// 解析 `{code,msg,data}` 包裹：code:0 → data，code:-1 → WebError::Api(msg)。
fn unwrap_envelope(value: Value) -> Result<Value, WebError> {
    let code = value
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| WebError::Api("响应缺少 code 字段".to_string()))?;
    if code == 0 {
        Ok(value.get("data").cloned().unwrap_or(Value::Null))
    } else {
        let msg = value
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        Err(WebError::Api(msg.to_string()))
    }
}

/// URL query 值转义（`%`/`?`/`&`/`=`/`#`）；URL 构造辅助，与 WebClient 同属
/// HTTP 层（从 config.rs 迁入，config.rs 只留业务逻辑）。
pub fn query_escape(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('?', "%3F")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('#', "%23")
}
