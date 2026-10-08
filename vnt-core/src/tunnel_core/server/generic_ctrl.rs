use crate::context::{AppState, PlatformNotification};
use crate::protocol::generic_ctrl::{
    GenericCtrlBody, GenericCtrlHello, GenericCtrlNotify, GenericCtrlRequest, GenericCtrlResponse,
    GenericCtrlResponseBody, GenericCtrlStatus, GenericCtrlUpgrade, GenericCtrlUpgradeAck,
    NOTIFY_BODY_MAX_BYTES, NOTIFY_TITLE_MAX_BYTES, SUPPORTED_METHODS,
};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

/// 通知展示出口：生产实现 = 日志 + app_state 环形缓冲；测试注入 double
pub(crate) trait NotifySink: Send + Sync {
    fn push(&self, notification: PlatformNotification);
}

/// 升级执行出口：返回 Ok 表示已接受（异步执行），Err(原因) 表示拒绝。
/// 生产实现在应答排干后才真正动手（见 SpawnUpgradeLauncher 的宽限期）
pub(crate) trait UpgradeLauncher: Send + Sync {
    fn launch(&self, upgrade: GenericCtrlUpgrade) -> Result<(), String>;
}

/// hello 应答所需的本端环境
pub(crate) struct HelloAgent {
    pub version: String,
    pub os: String,
    pub arch: String,
}

impl HelloAgent {
    pub(crate) fn from_env() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }
}

/// 请求分发：新增指令 = proto 加 oneof 变体 + 此处加一个 match 臂 + 指令模块
pub(crate) async fn dispatch(
    request: GenericCtrlRequest,
    sink: &dyn NotifySink,
    launcher: &dyn UpgradeLauncher,
    agent: &HelloAgent,
) -> GenericCtrlResponse {
    let body = match request.body {
        None => {
            // 对端（更新的服务器）认识信封但不认识变体是版本差异，回 UNSUPPORTED
            return unsupported(request.id);
        }
        Some(body) => body,
    };
    match body {
        GenericCtrlBody::Hello(_) => GenericCtrlResponse {
            id: request.id,
            status: GenericCtrlStatus::GenericCtrlOk,
            error: String::new(),
            body: Some(GenericCtrlResponseBody::Hello(GenericCtrlHello {
                methods: SUPPORTED_METHODS.iter().map(|s| s.to_string()).collect(),
                agent_version: agent.version.clone(),
                os: agent.os.clone(),
                arch: agent.arch.clone(),
            })),
        },
        GenericCtrlBody::Notify(notify) => handle_notify(request.id, notify, sink),
        GenericCtrlBody::Upgrade(upgrade) => handle_upgrade(request.id, upgrade, launcher),
        GenericCtrlBody::UpgradeResult(_) => GenericCtrlResponse {
            id: request.id,
            status: GenericCtrlStatus::GenericCtrlError,
            error: "upgrade_result 只应由节点发往服务器".to_string(),
            body: None,
        },
    }
}

/// notify：长度防线双侧强制（服务端已校验，此处防被绕过的直连会话）
fn handle_notify(id: u64, notify: GenericCtrlNotify, sink: &dyn NotifySink) -> GenericCtrlResponse {
    let level = match notify.level.as_str() {
        "warning" | "important" => notify.level,
        _ => "info".to_string(),
    };
    if notify.title.len() > NOTIFY_TITLE_MAX_BYTES || notify.body.len() > NOTIFY_BODY_MAX_BYTES {
        return error_response(
            id,
            format!(
                "通知内容超限（title≤{NOTIFY_TITLE_MAX_BYTES}B, body≤{NOTIFY_BODY_MAX_BYTES}B）"
            ),
        );
    }
    sink.push(PlatformNotification {
        level,
        title: notify.title,
        body: notify.body,
        received_at: crate::utils::time::now_ts_ms(),
    });
    GenericCtrlResponse {
        id,
        status: GenericCtrlStatus::GenericCtrlOk,
        error: String::new(),
        body: Some(GenericCtrlResponseBody::Notify),
    }
}

/// upgrade：先同步判定接受与否（平台防线 + 并发防重入），执行在应答
/// 成功写出后由 kick_reserved_upgrade 启动（见 inbound.rs 接线）
fn handle_upgrade(
    id: u64,
    upgrade: GenericCtrlUpgrade,
    launcher: &dyn UpgradeLauncher,
) -> GenericCtrlResponse {
    // 平台防线：制品目标平台与本节点不符直接拒绝（服务端 hello 筛选之外的
    // 双保险；os/arch 为空 = 旧服务器指令未携带，跳过校验）
    let platform_mismatch = (!upgrade.os.is_empty() && upgrade.os != std::env::consts::OS)
        || (!upgrade.arch.is_empty() && upgrade.arch != std::env::consts::ARCH);
    let outcome = if platform_mismatch {
        Err(format!(
            "制品平台 {}/{} 与本节点 {}/{} 不符",
            upgrade.os,
            upgrade.arch,
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    } else {
        // 拒绝（并发防重入等）不是信封级错误：accepted=false + error 带原因
        launcher.launch(upgrade)
    };
    let accepted = outcome.is_ok();
    let error = match outcome {
        Ok(()) => String::new(),
        Err(reason) => reason,
    };
    GenericCtrlResponse {
        id,
        status: GenericCtrlStatus::GenericCtrlOk,
        error,
        body: Some(GenericCtrlResponseBody::Upgrade(GenericCtrlUpgradeAck {
            accepted,
        })),
    }
}

fn unsupported(id: u64) -> GenericCtrlResponse {
    GenericCtrlResponse {
        id,
        status: GenericCtrlStatus::GenericCtrlUnsupported,
        error: "unknown generic ctrl method".to_string(),
        body: None,
    }
}

fn error_response(id: u64, error: String) -> GenericCtrlResponse {
    GenericCtrlResponse {
        id,
        status: GenericCtrlStatus::GenericCtrlError,
        error,
        body: None,
    }
}

/// 客户端发起的通用请求（升级回执）的待决表，模式复刻 rpc.rs RpcNotifier
#[derive(Clone, Default)]
pub(crate) struct GenericCtrlNotifier {
    inner: Arc<Mutex<GenericCtrlNotifierInner>>,
}

#[derive(Default)]
struct GenericCtrlNotifierInner {
    next_id: u64,
    pending: HashMap<u64, oneshot::Sender<GenericCtrlResponse>>,
}

impl GenericCtrlNotifier {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 进程级全局通知器：客户端单实例运行，所有服务端连接共用一张
    /// 待决表（id 全局唯一即无冲突）
    pub(crate) fn global() -> &'static Self {
        static NOTIFIER: std::sync::OnceLock<GenericCtrlNotifier> = std::sync::OnceLock::new();
        NOTIFIER.get_or_init(Self::new)
    }

    /// 只取下一个请求 id（不登记等待；用于发后即弃的上报）
    pub(crate) fn allocate_id(&self) -> u64 {
        let mut inner = self.inner.lock();
        inner.next_id += 1;
        inner.next_id
    }

    /// 登记待决请求并返回 (id, 等待句柄)。当前生产路径只上报不等待
    /// （发后即弃），保留等待能力供后续需要应答的管理指令使用
    #[allow(dead_code)]
    pub(crate) fn create_waiter(&self) -> (u64, GenericCtrlWaiter) {
        let mut inner = self.inner.lock();
        inner.next_id += 1;
        let id = inner.next_id;
        let (tx, rx) = oneshot::channel();
        inner.pending.insert(id, tx);
        (
            id,
            GenericCtrlWaiter {
                id,
                inner: Arc::clone(&self.inner),
                rx,
            },
        )
    }

    /// 收到应答时按 id 唤醒等待方（无等待方则忽略——迟到应答）
    pub(crate) fn notify_response(&self, response: GenericCtrlResponse) {
        let mut inner = self.inner.lock();
        if let Some(tx) = inner.pending.remove(&response.id) {
            let _ = tx.send(response);
        }
    }
}

#[allow(dead_code)]
pub(crate) struct GenericCtrlWaiter {
    id: u64,
    inner: Arc<Mutex<GenericCtrlNotifierInner>>,
    rx: oneshot::Receiver<GenericCtrlResponse>,
}

impl GenericCtrlWaiter {
    /// 等待应答（当前生产路径发后即弃，等待能力留给后续需要应答的管理指令）
    #[allow(dead_code)]
    pub(crate) async fn wait(mut self, timeout: Duration) -> anyhow::Result<GenericCtrlResponse> {
        match tokio::time::timeout(timeout, &mut self.rx).await {
            Err(_) => anyhow::bail!("等待通用控制应答超时"),
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => anyhow::bail!("通用控制应答通道关闭"),
        }
    }
}

impl Drop for GenericCtrlWaiter {
    fn drop(&mut self) {
        let mut inner = self.inner.lock();
        let _ = inner.pending.remove(&self.id);
    }
}

/// 生产通知出口：日志（vnt2_web 的 web 日志页可见）+ app_state 缓冲
struct AppStateNotifySink {
    app_state: AppState,
}

impl NotifySink for AppStateNotifySink {
    fn push(&self, notification: PlatformNotification) {
        log::info!(
            "[平台通知][{}] {} —— {}",
            notification.level,
            notification.title,
            notification.body
        );
        self.app_state.push_platform_notification(notification);
    }
}

/// 生产升级出口：只做"预订"——占进程级防重入闸并暂存计划；真正执行由
/// kick_reserved_upgrade 在 accepted 应答成功写出后触发（inbound.rs），
/// 应答发送失败走 abandon_reserved_upgrade 释放闸位。
/// exec 成功不回落，闸位随进程消亡复位。
struct SpawnUpgradeLauncher;

static UPGRADE_IN_PROGRESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static RESERVED_UPGRADE: Mutex<Option<GenericCtrlUpgrade>> = Mutex::new(None);

impl UpgradeLauncher for SpawnUpgradeLauncher {
    fn launch(&self, upgrade: GenericCtrlUpgrade) -> Result<(), String> {
        if UPGRADE_IN_PROGRESS.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Err("升级已在进行中，拒绝重复指令".to_string());
        }
        *RESERVED_UPGRADE.lock() = Some(upgrade);
        Ok(())
    }
}

/// 启动已预订的升级执行。只在 accepted 应答成功写出后调用——顺序保证
/// 是结构性的（inbound.rs 先 send_turn 后 kick），不再依赖延时猜测。
pub(crate) fn kick_reserved_upgrade() {
    if let Some(upgrade) = RESERVED_UPGRADE.lock().take() {
        let target_version = upgrade.version.clone();
        tokio::spawn(async move {
            let plan = crate::upgrade::UpgradePlan {
                version: upgrade.version,
                url: upgrade.url,
                sha256: upgrade.sha256,
                size: upgrade.size,
                filename: upgrade.filename,
            };
            if let Err(error) = crate::upgrade::run_upgrade(plan).await {
                log::error!("远程升级执行失败: {error:#}");
                // 失败回执：写入标记（持久化），连接管理器周期上报发出
                record_upgrade_failure(&target_version, &format!("{error:#}"));
                UPGRADE_IN_PROGRESS.store(false, std::sync::atomic::Ordering::Release);
            }
            // 成功路径 run_upgrade 以 exec/exit 结束，不会返回
        });
    }
}

/// 放弃已预订的升级（accepted 应答发送失败时调用，释放防重入闸）
pub(crate) fn abandon_reserved_upgrade() {
    if RESERVED_UPGRADE.lock().take().is_some() {
        UPGRADE_IN_PROGRESS.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// 就地失败结果落盘：连接存活时由周期上报发出，进程崩溃则重启后补报
fn record_upgrade_failure(to_version: &str, error: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let marker = crate::upgrade::UpgradeMarker {
        from_version: env!("CARGO_PKG_VERSION").to_string(),
        to_version: to_version.to_string(),
        started_at: crate::utils::time::now_ts_ms(),
        failure: Some(error.to_string()),
    };
    if let Err(write_error) =
        crate::upgrade::write_marker(&crate::upgrade::marker_path(&exe), &marker)
    {
        log::warn!("升级失败标记写入失败（失败原因将不上报）: {write_error:#}");
    }
}

/// 入站请求的生产入口（inbound.rs 调用）
pub(crate) async fn dispatch_request(
    request: GenericCtrlRequest,
    app_state: &AppState,
) -> GenericCtrlResponse {
    let sink = AppStateNotifySink {
        app_state: app_state.clone(),
    };
    dispatch(
        request,
        &sink,
        &SpawnUpgradeLauncher,
        &HelloAgent::from_env(),
    )
    .await
}

/// 升级标记的互斥 claim 文件路径（多服务器连接并发出报时只留一个发送者）
fn marker_claim_path(marker: &std::path::Path) -> std::path::PathBuf {
    let mut name = marker.as_os_str().to_os_string();
    name.push(".claim");
    std::path::PathBuf::from(name)
}

/// 尝试独占 claim；已存在且超过 60s 视为崩溃残留，可窃取
fn try_claim(claim: &std::path::Path) -> bool {
    let create = |path: &std::path::Path| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .is_ok()
    };
    if create(claim) {
        return true;
    }
    let stealable = std::fs::metadata(claim)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > Duration::from_secs(60));
    stealable && std::fs::remove_file(claim).is_ok() && create(claim)
}

async fn send_upgrade_result(
    transport: &mut crate::tunnel_core::server::transport::TransportClient,
    result: crate::protocol::generic_ctrl::GenericCtrlUpgradeResult,
) -> anyhow::Result<()> {
    let request = GenericCtrlRequest {
        id: GenericCtrlNotifier::global().allocate_id(),
        body: Some(GenericCtrlBody::UpgradeResult(result)),
    };
    let payload = request.encode();
    let mut packet = match crate::protocol::ip_packet_protocol::NetPacket::new(
        crate::protocol::transmission::TransmissionBytes::zeroed(
            crate::protocol::ip_packet_protocol::HEAD_LENGTH + payload.len(),
        ),
    ) {
        Ok(packet) => packet,
        Err(error) => anyhow::bail!("构造升级结果上报包失败: {error}"),
    };
    // 帧风格对齐 rpc.rs 的 RpcReq：gateway 包由会话绑定，无需显式 src/dest
    packet.set_msg_type(crate::protocol::ip_packet_protocol::MsgType::GenericCtrlReq);
    packet.set_gateway_flag(true);
    packet.set_ttl(1);
    packet.set_payload(payload.as_ref())?;
    transport.send_turn(packet).await
}

/// 升级结果上报：读到标记即发送、发送成功即清标记。
/// 在连接建立后与周期 tick 调用（就地失败也写标记 → 存活连接 ≤5s 内上报）。
/// 应答为尽力而为（不等待——data_handle_loop 内等待会与读循环死锁），
/// 平台另有版本观测兜底；claim 文件防多服务器连接重复上报。
pub(crate) async fn report_pending_upgrade_result(
    transport: &mut crate::tunnel_core::server::transport::TransportClient,
) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let marker_path = crate::upgrade::marker_path(&exe);
    if !marker_path.exists() {
        return; // 周期调用的快速路径：绝大多数 tick 无标记
    }
    let claim_path = marker_claim_path(&marker_path);
    if !try_claim(&claim_path) {
        return; // 另一条服务器连接正在上报
    }
    let release_claim = || {
        let _ = std::fs::remove_file(&claim_path);
    };
    let Some(marker) = crate::upgrade::read_marker(&marker_path) else {
        release_claim();
        return;
    };
    let (success, error) = crate::upgrade::evaluate_marker(&marker, env!("CARGO_PKG_VERSION"));
    let result = crate::protocol::generic_ctrl::GenericCtrlUpgradeResult {
        success,
        from_version: marker.from_version.clone(),
        to_version: marker.to_version.clone(),
        error,
    };
    match send_upgrade_result(transport, result).await {
        Ok(()) => {
            crate::upgrade::clear_marker(&marker_path);
            release_claim();
            log::info!(
                "升级结果已上报（{} → {}，success={success}）",
                marker.from_version,
                marker.to_version
            );
        }
        Err(error) => {
            // 保留标记待重试；释放 claim 让本连接下个 tick 或其他连接接手
            release_claim();
            log::warn!("升级结果上报失败（保留标记待下次上报）: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct SinkDouble {
        received: Mutex<Vec<PlatformNotification>>,
    }

    impl NotifySink for SinkDouble {
        fn push(&self, notification: PlatformNotification) {
            self.received.lock().push(notification);
        }
    }

    #[derive(Default)]
    struct LauncherDouble {
        launches: AtomicUsize,
        fail_with: Option<String>,
    }

    impl UpgradeLauncher for LauncherDouble {
        fn launch(&self, _upgrade: GenericCtrlUpgrade) -> Result<(), String> {
            self.launches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match &self.fail_with {
                Some(reason) => Err(reason.clone()),
                None => Ok(()),
            }
        }
    }

    fn agent() -> HelloAgent {
        HelloAgent {
            version: "9.9.9".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    fn sample_notify() -> GenericCtrlNotify {
        GenericCtrlNotify {
            level: "important".into(),
            title: "维护通知".into(),
            body: "今晚 22:00 重启".into(),
        }
    }

    // T4: notify 正常路径——写入 sink 并回 Notify ack
    #[tokio::test]
    async fn notify_pushes_to_sink_and_acks() {
        let sink = SinkDouble::default();
        let launcher = LauncherDouble::default();
        let response = dispatch(
            GenericCtrlRequest {
                id: 3,
                body: Some(GenericCtrlBody::Notify(sample_notify())),
            },
            &sink,
            &launcher,
            &agent(),
        )
        .await;
        assert_eq!(response.id, 3);
        assert_eq!(response.status, GenericCtrlStatus::GenericCtrlOk);
        assert!(matches!(
            response.body,
            Some(GenericCtrlResponseBody::Notify)
        ));
        let received = sink.received.lock();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].level, "important");
        assert_eq!(received[0].title, "维护通知");
    }

    // T4: 未知 level 归一为 info（前向兼容）
    #[tokio::test]
    async fn notify_unknown_level_normalized() {
        let sink = SinkDouble::default();
        let mut notify = sample_notify();
        notify.level = "future-level".into();
        dispatch(
            GenericCtrlRequest {
                id: 1,
                body: Some(GenericCtrlBody::Notify(notify)),
            },
            &sink,
            &LauncherDouble::default(),
            &agent(),
        )
        .await;
        assert_eq!(sink.received.lock()[0].level, "info");
    }

    // T4: 超长内容拒绝
    #[tokio::test]
    async fn notify_oversized_rejected() {
        let sink = SinkDouble::default();
        let mut notify = sample_notify();
        notify.title = "t".repeat(NOTIFY_TITLE_MAX_BYTES + 1);
        let response = dispatch(
            GenericCtrlRequest {
                id: 2,
                body: Some(GenericCtrlBody::Notify(notify)),
            },
            &sink,
            &LauncherDouble::default(),
            &agent(),
        )
        .await;
        assert_eq!(response.status, GenericCtrlStatus::GenericCtrlError);
        assert!(sink.received.lock().is_empty());
    }

    // T3: 未知变体（body=None）→ UNSUPPORTED 同 id
    #[tokio::test]
    async fn unknown_variant_answers_unsupported() {
        let response = dispatch(
            GenericCtrlRequest { id: 11, body: None },
            &SinkDouble::default(),
            &LauncherDouble::default(),
            &agent(),
        )
        .await;
        assert_eq!(response.id, 11);
        assert_eq!(response.status, GenericCtrlStatus::GenericCtrlUnsupported);
    }

    // hello 应答携带环境与方法清单
    #[tokio::test]
    async fn hello_answers_methods_and_env() {
        let response = dispatch(
            GenericCtrlRequest {
                id: 5,
                body: Some(GenericCtrlBody::Hello(GenericCtrlHello {
                    methods: Vec::new(),
                    agent_version: String::new(),
                    os: String::new(),
                    arch: String::new(),
                })),
            },
            &SinkDouble::default(),
            &LauncherDouble::default(),
            &agent(),
        )
        .await;
        let Some(GenericCtrlResponseBody::Hello(hello)) = response.body else {
            panic!("expected hello body");
        };
        assert_eq!(hello.agent_version, "9.9.9");
        assert_eq!(hello.os, "linux");
        assert!(hello.methods.contains(&"notify".to_string()));
        assert!(hello.methods.contains(&"upgrade".to_string()));
    }

    // T5: 升级接受路径——launcher 被调用且应答 accepted=true
    #[tokio::test]
    async fn upgrade_launches_and_acks_accepted() {
        let launcher = LauncherDouble::default();
        let response = dispatch(
            GenericCtrlRequest {
                id: 6,
                body: Some(GenericCtrlBody::Upgrade(GenericCtrlUpgrade {
                    version: "2.0.12".into(),
                    url: "http://127.0.0.1:1/x".into(),
                    sha256: vec![1; 32],
                    size: 1,
                    filename: "vnt2_web".into(),
                    os: String::new(),
                    arch: String::new(),
                })),
            },
            &SinkDouble::default(),
            &launcher,
            &agent(),
        )
        .await;
        assert_eq!(
            response.body,
            Some(GenericCtrlResponseBody::Upgrade(GenericCtrlUpgradeAck {
                accepted: true
            }))
        );
        assert_eq!(
            launcher.launches.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    // T5: launcher 拒绝（并发防重入）→ accepted=false + error 说明
    #[tokio::test]
    async fn upgrade_rejection_reported_not_accepted() {
        let launcher = LauncherDouble {
            fail_with: Some("busy".into()),
            ..Default::default()
        };
        let response = dispatch(
            GenericCtrlRequest {
                id: 7,
                body: Some(GenericCtrlBody::Upgrade(GenericCtrlUpgrade {
                    version: "2.0.12".into(),
                    url: String::new(),
                    sha256: Vec::new(),
                    size: 0,
                    filename: String::new(),
                    os: String::new(),
                    arch: String::new(),
                })),
            },
            &SinkDouble::default(),
            &launcher,
            &agent(),
        )
        .await;
        let Some(GenericCtrlResponseBody::Upgrade(ack)) = response.body else {
            panic!("expected upgrade ack");
        };
        assert!(!ack.accepted);
        assert!(!response.error.is_empty());
    }

    // F1: 平台不符 → 不调用执行器，accepted=false 带原因（本机 os 必不匹配）
    #[tokio::test]
    async fn upgrade_platform_mismatch_rejected_without_launch() {
        let launcher = LauncherDouble::default();
        let response = dispatch(
            GenericCtrlRequest {
                id: 8,
                body: Some(GenericCtrlBody::Upgrade(GenericCtrlUpgrade {
                    version: "2.0.12".into(),
                    url: String::new(),
                    sha256: Vec::new(),
                    size: 0,
                    filename: String::new(),
                    os: format!("{}-mismatch", std::env::consts::OS),
                    arch: std::env::consts::ARCH.to_string(),
                })),
            },
            &SinkDouble::default(),
            &launcher,
            &agent(),
        )
        .await;
        let Some(GenericCtrlResponseBody::Upgrade(ack)) = response.body else {
            panic!("expected upgrade ack");
        };
        assert!(!ack.accepted);
        assert!(response.error.contains("平台"));
        assert_eq!(
            launcher.launches.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    // F4: 预订生命周期——空槽 kick 无副作用、重入拒绝、abandon 释放
    #[test]
    fn reservation_lifecycle_rejects_reentry_and_release() {
        kick_reserved_upgrade(); // 空槽位 no-op
        let upgrade = GenericCtrlUpgrade {
            version: "9.9.9".into(),
            url: String::new(),
            sha256: Vec::new(),
            size: 0,
            filename: String::new(),
            os: String::new(),
            arch: String::new(),
        };
        assert!(SpawnUpgradeLauncher.launch(upgrade.clone()).is_ok());
        // 防重入：闸位已占
        assert!(SpawnUpgradeLauncher.launch(upgrade.clone()).is_err());
        abandon_reserved_upgrade();
        assert!(SpawnUpgradeLauncher.launch(upgrade).is_ok());
        abandon_reserved_upgrade();
    }

    // M6: claim 独占——占用后第二个 claim 失败，释放后可再占
    #[test]
    fn marker_claim_is_exclusive() {
        let dir = std::env::temp_dir().join(format!("vnt-upg-cl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let claim = dir.join("m.claim");
        let _ = std::fs::remove_file(&claim);
        assert!(try_claim(&claim));
        assert!(!try_claim(&claim));
        std::fs::remove_file(&claim).unwrap();
        assert!(try_claim(&claim));
        std::fs::remove_file(&claim).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // T7: 待决表 id 关联唤醒
    #[tokio::test]
    async fn notifier_resolves_by_id() {
        let notifier = GenericCtrlNotifier::new();
        let (id, waiter) = notifier.create_waiter();
        let (id2, waiter2) = notifier.create_waiter();
        assert_ne!(id, id2);
        notifier.notify_response(GenericCtrlResponse {
            id,
            status: GenericCtrlStatus::GenericCtrlOk,
            error: String::new(),
            body: None,
        });
        let resolved = waiter.wait(Duration::from_millis(100)).await.unwrap();
        assert_eq!(resolved.id, id);
        // 迟到/未知 id 不影响其他等待方
        notifier.notify_response(GenericCtrlResponse {
            id: 999,
            status: GenericCtrlStatus::GenericCtrlOk,
            error: String::new(),
            body: None,
        });
        assert!(waiter2.wait(Duration::from_millis(50)).await.is_err());
    }
}
