use crate::protocol::ProtoToBytesMut;
use anyhow::bail;
use bytes::BytesMut;
use prost::Message;

mod proto {
    include!(concat!(env!("OUT_DIR"), "/protocol.generic_ctrl.rs"));
}

pub use proto::GenericCtrlStatus;

/// 信封载荷上限（双向强制）：低频管理控制通道无需更大载荷，
/// 超限按协议违约丢弃（对齐"客户端上报须有服务端上限"的既有纪律，反向补齐）
pub(crate) const GENERIC_CTRL_MAX_PAYLOAD: usize = 64 * 1024;
/// notify 标题/正文的展示层上限（字节）
pub(crate) const NOTIFY_TITLE_MAX_BYTES: usize = 128;
pub(crate) const NOTIFY_BODY_MAX_BYTES: usize = 4096;

/// 本端已实现的指令名（hello 上报；新增指令在此登记 + proto oneof + 分发臂）
pub(crate) const SUPPORTED_METHODS: &[&str] = &["hello", "notify", "upgrade", "upgrade_result"];

/// 能力握手载荷
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenericCtrlHello {
    pub methods: Vec<String>,
    pub agent_version: String,
    pub os: String,
    pub arch: String,
}

/// 平台通知载荷
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenericCtrlNotify {
    /// info | warning | important（未知值按 info 展示）
    pub level: String,
    pub title: String,
    pub body: String,
}

/// 远程升级指令载荷
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenericCtrlUpgrade {
    pub version: String,
    pub url: String,
    pub sha256: Vec<u8>,
    pub size: u64,
    pub filename: String,
}

/// 升级结果回执载荷
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenericCtrlUpgradeResult {
    pub success: bool,
    pub from_version: String,
    pub to_version: String,
    pub error: String,
}

/// 升级指令应答载荷
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GenericCtrlUpgradeAck {
    pub accepted: bool,
}

/// 通用控制请求：信封 + 指令体。body 为 None 表示对端不认识该变体
/// （解码成功但 oneof 未命中），应答 UNSUPPORTED 而非沉默
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GenericCtrlRequest {
    pub id: u64,
    pub body: Option<GenericCtrlBody>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GenericCtrlBody {
    Hello(GenericCtrlHello),
    Notify(GenericCtrlNotify),
    Upgrade(GenericCtrlUpgrade),
    UpgradeResult(GenericCtrlUpgradeResult),
}

impl GenericCtrlBody {
    /// 指令名（hello 上报的 methods 与之一致；测试锚点，勿删）
    #[allow(dead_code)]
    pub(crate) fn method(&self) -> &'static str {
        match self {
            GenericCtrlBody::Hello(_) => "hello",
            GenericCtrlBody::Notify(_) => "notify",
            GenericCtrlBody::Upgrade(_) => "upgrade",
            GenericCtrlBody::UpgradeResult(_) => "upgrade_result",
        }
    }
}

/// 通用控制应答
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GenericCtrlResponse {
    pub id: u64,
    pub status: GenericCtrlStatus,
    pub error: String,
    pub body: Option<GenericCtrlResponseBody>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GenericCtrlResponseBody {
    Hello(GenericCtrlHello),
    Notify,
    Upgrade(GenericCtrlUpgradeAck),
}

impl GenericCtrlRequest {
    pub(crate) fn encode(&self) -> BytesMut {
        proto::GenericCtrlRequest {
            id: self.id,
            body: self.body.as_ref().map(request_body_to_proto),
        }
        .encode_bytes_mut()
    }

    pub(crate) fn from_slice(buf: &[u8]) -> anyhow::Result<Self> {
        if buf.len() > GENERIC_CTRL_MAX_PAYLOAD {
            bail!(
                "generic ctrl request exceeds {} bytes",
                GENERIC_CTRL_MAX_PAYLOAD
            );
        }
        let msg = proto::GenericCtrlRequest::decode(buf)?;
        Ok(Self {
            id: msg.id,
            body: msg.body.map(request_body_from_proto),
        })
    }
}

impl GenericCtrlResponse {
    pub(crate) fn encode(&self) -> BytesMut {
        proto::GenericCtrlResponse {
            id: self.id,
            status: self.status as i32,
            error: self.error.clone(),
            body: self.body.as_ref().map(response_body_to_proto),
        }
        .encode_bytes_mut()
    }

    pub(crate) fn from_slice(buf: &[u8]) -> anyhow::Result<Self> {
        if buf.len() > GENERIC_CTRL_MAX_PAYLOAD {
            bail!(
                "generic ctrl response exceeds {} bytes",
                GENERIC_CTRL_MAX_PAYLOAD
            );
        }
        let msg = proto::GenericCtrlResponse::decode(buf)?;
        Ok(Self {
            id: msg.id,
            status: msg.status(),
            error: msg.error,
            body: msg.body.map(response_body_from_proto),
        })
    }
}

fn hello_to_proto(hello: &GenericCtrlHello) -> proto::GenericCtrlHello {
    proto::GenericCtrlHello {
        methods: hello.methods.clone(),
        agent_version: hello.agent_version.clone(),
        os: hello.os.clone(),
        arch: hello.arch.clone(),
    }
}

fn hello_from_proto(hello: proto::GenericCtrlHello) -> GenericCtrlHello {
    GenericCtrlHello {
        methods: hello.methods,
        agent_version: hello.agent_version,
        os: hello.os,
        arch: hello.arch,
    }
}

fn request_body_to_proto(body: &GenericCtrlBody) -> proto::generic_ctrl_request::Body {
    match body {
        GenericCtrlBody::Hello(hello) => {
            proto::generic_ctrl_request::Body::Hello(hello_to_proto(hello))
        }
        GenericCtrlBody::Notify(notify) => {
            proto::generic_ctrl_request::Body::Notify(proto::GenericCtrlNotify {
                level: notify.level.clone(),
                title: notify.title.clone(),
                body: notify.body.clone(),
            })
        }
        GenericCtrlBody::Upgrade(upgrade) => {
            proto::generic_ctrl_request::Body::Upgrade(proto::GenericCtrlUpgrade {
                version: upgrade.version.clone(),
                url: upgrade.url.clone(),
                sha256: upgrade.sha256.clone(),
                size: upgrade.size,
                filename: upgrade.filename.clone(),
            })
        }
        GenericCtrlBody::UpgradeResult(result) => {
            proto::generic_ctrl_request::Body::UpgradeResult(proto::GenericCtrlUpgradeResult {
                success: result.success,
                from_version: result.from_version.clone(),
                to_version: result.to_version.clone(),
                error: result.error.clone(),
            })
        }
    }
}

fn request_body_from_proto(body: proto::generic_ctrl_request::Body) -> GenericCtrlBody {
    match body {
        proto::generic_ctrl_request::Body::Hello(hello) => {
            GenericCtrlBody::Hello(hello_from_proto(hello))
        }
        proto::generic_ctrl_request::Body::Notify(notify) => {
            GenericCtrlBody::Notify(GenericCtrlNotify {
                level: notify.level,
                title: notify.title,
                body: notify.body,
            })
        }
        proto::generic_ctrl_request::Body::Upgrade(upgrade) => {
            GenericCtrlBody::Upgrade(GenericCtrlUpgrade {
                version: upgrade.version,
                url: upgrade.url,
                sha256: upgrade.sha256,
                size: upgrade.size,
                filename: upgrade.filename,
            })
        }
        proto::generic_ctrl_request::Body::UpgradeResult(result) => {
            GenericCtrlBody::UpgradeResult(GenericCtrlUpgradeResult {
                success: result.success,
                from_version: result.from_version,
                to_version: result.to_version,
                error: result.error,
            })
        }
    }
}

fn response_body_to_proto(body: &GenericCtrlResponseBody) -> proto::generic_ctrl_response::Body {
    match body {
        GenericCtrlResponseBody::Hello(hello) => {
            proto::generic_ctrl_response::Body::Hello(hello_to_proto(hello))
        }
        GenericCtrlResponseBody::Notify => {
            proto::generic_ctrl_response::Body::Notify(proto::GenericCtrlNotifyAck {})
        }
        GenericCtrlResponseBody::Upgrade(ack) => {
            proto::generic_ctrl_response::Body::Upgrade(proto::GenericCtrlUpgradeAck {
                accepted: ack.accepted,
            })
        }
    }
}

fn response_body_from_proto(body: proto::generic_ctrl_response::Body) -> GenericCtrlResponseBody {
    match body {
        proto::generic_ctrl_response::Body::Hello(hello) => {
            GenericCtrlResponseBody::Hello(hello_from_proto(hello))
        }
        proto::generic_ctrl_response::Body::Notify(_) => GenericCtrlResponseBody::Notify,
        proto::generic_ctrl_response::Body::Upgrade(ack) => {
            GenericCtrlResponseBody::Upgrade(GenericCtrlUpgradeAck {
                accepted: ack.accepted,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hello() -> GenericCtrlHello {
        GenericCtrlHello {
            methods: vec!["hello".into(), "notify".into()],
            agent_version: "2.0.11".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        }
    }

    // T2: 四种请求体 roundtrip
    #[test]
    fn request_bodies_round_trip() {
        let bodies = [
            GenericCtrlBody::Hello(sample_hello()),
            GenericCtrlBody::Notify(GenericCtrlNotify {
                level: "important".into(),
                title: "维护通知".into(),
                body: "今晚 22:00 重启".into(),
            }),
            GenericCtrlBody::Upgrade(GenericCtrlUpgrade {
                version: "2.0.12".into(),
                url: "https://vnts2.example.com/upgrades/abc/vnt2_web".into(),
                sha256: vec![7; 32],
                size: 10_000_000,
                filename: "vnt2_web-linux-x86_64".into(),
            }),
            GenericCtrlBody::UpgradeResult(GenericCtrlUpgradeResult {
                success: true,
                from_version: "2.0.11".into(),
                to_version: "2.0.12".into(),
                error: String::new(),
            }),
        ];
        for body in bodies {
            let request = GenericCtrlRequest {
                id: 42,
                body: Some(body),
            };
            let decoded = GenericCtrlRequest::from_slice(request.encode().as_ref()).unwrap();
            assert_eq!(decoded, request);
        }
    }

    // T2: 应答信封（含状态与三种应答体）roundtrip
    #[test]
    fn response_envelope_round_trip() {
        let ok = GenericCtrlResponse {
            id: 7,
            status: GenericCtrlStatus::GenericCtrlOk,
            error: String::new(),
            body: Some(GenericCtrlResponseBody::Upgrade(GenericCtrlUpgradeAck {
                accepted: true,
            })),
        };
        assert_eq!(
            GenericCtrlResponse::from_slice(ok.encode().as_ref()).unwrap(),
            ok
        );

        let err = GenericCtrlResponse {
            id: 8,
            status: GenericCtrlStatus::GenericCtrlUnsupported,
            error: "unknown variant".into(),
            body: None,
        };
        assert_eq!(
            GenericCtrlResponse::from_slice(err.encode().as_ref()).unwrap(),
            err
        );

        let hello = GenericCtrlResponse {
            id: 9,
            status: GenericCtrlStatus::GenericCtrlOk,
            error: String::new(),
            body: Some(GenericCtrlResponseBody::Hello(sample_hello())),
        };
        assert_eq!(
            GenericCtrlResponse::from_slice(hello.encode().as_ref()).unwrap(),
            hello
        );
    }

    // T3 的前置：仅 id 的裸信封（模拟对端不认识的变体被 prost 丢弃）解码 body=None
    #[test]
    fn id_only_request_decodes_body_none() {
        let raw = proto::GenericCtrlRequest { id: 5, body: None }.encode_bytes_mut();
        let decoded = GenericCtrlRequest::from_slice(raw.as_ref()).unwrap();
        assert_eq!(decoded.id, 5);
        assert!(decoded.body.is_none());
    }

    // 信封上限：超限拒绝解码
    #[test]
    fn oversized_envelope_rejected() {
        let big = vec![0u8; GENERIC_CTRL_MAX_PAYLOAD + 1];
        assert!(GenericCtrlRequest::from_slice(&big).is_err());
        assert!(GenericCtrlResponse::from_slice(&big).is_err());
    }

    // T1: MsgType 33/34 注册且 roundtrip；35 仍为非法值
    #[test]
    fn msg_type_33_34_registered() {
        use crate::protocol::ip_packet_protocol::MsgType;
        assert_eq!(u8::from(MsgType::GenericCtrlReq), 33);
        assert_eq!(u8::from(MsgType::GenericCtrlRes), 34);
        assert_eq!(MsgType::try_from(33u8).unwrap(), MsgType::GenericCtrlReq);
        assert_eq!(MsgType::try_from(34u8).unwrap(), MsgType::GenericCtrlRes);
        assert!(MsgType::try_from(35u8).is_err());
    }

    // 指令名与 hello 清单一致（新增指令时两侧同步的锚点）
    #[test]
    fn body_method_names_match_supported_list() {
        let all = [
            GenericCtrlBody::Hello(sample_hello()),
            GenericCtrlBody::Notify(GenericCtrlNotify {
                level: "info".into(),
                title: String::new(),
                body: String::new(),
            }),
            GenericCtrlBody::Upgrade(GenericCtrlUpgrade {
                version: String::new(),
                url: String::new(),
                sha256: Vec::new(),
                size: 0,
                filename: String::new(),
            }),
            GenericCtrlBody::UpgradeResult(GenericCtrlUpgradeResult {
                success: false,
                from_version: String::new(),
                to_version: String::new(),
                error: String::new(),
            }),
        ];
        for body in &all {
            assert!(SUPPORTED_METHODS.contains(&body.method()));
        }
    }
}
