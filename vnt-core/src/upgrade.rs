//! 远程升级执行器（GenericCtrl::upgrade 的落地实现）。
//!
//! 流程：下载到同目录暂存文件 → SHA-256/大小校验 → 写升级标记 →
//! 平台化替换自身 → exec 重启（unix 保 PID，与 nohup/systemd 兼容）。
//! 成功后进程被替换不返回；重启后由连接管理器读取标记上报 upgrade_result。

use anyhow::{Context, bail};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 远程升级计划（来自 GenericCtrlUpgrade 载荷）
pub struct UpgradePlan {
    pub version: String,
    pub url: String,
    pub sha256: Vec<u8>,
    pub size: u64,
    pub filename: String,
}

/// 升级标记（key=value 文本，避免引入额外依赖；版本号不含换行）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeMarker {
    pub from_version: String,
    pub to_version: String,
    pub started_at: i64,
    /// 就地失败结果（下载/校验/替换/exec 失败时写入，连接存活期间由
    /// 周期上报发出；None = 重启路径，按当前版本判定成败）
    pub failure: Option<String>,
}

/// 暂存文件路径：与可执行文件同目录（同文件系统保证 rename 原子性）
pub fn staging_path(exe: &Path) -> PathBuf {
    let mut name = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vnt".to_string());
    name.push_str(".new");
    exe.with_file_name(name)
}

/// 升级标记文件路径
pub fn marker_path(exe: &Path) -> PathBuf {
    let mut name = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "vnt".to_string());
    name.push_str(".upgrade-pending");
    exe.with_file_name(name)
}

pub fn write_marker(path: &Path, marker: &UpgradeMarker) -> anyhow::Result<()> {
    let mut content = format!(
        "from_version={}\nto_version={}\nstarted_at={}\n",
        marker.from_version, marker.to_version, marker.started_at
    );
    if let Some(failure) = &marker.failure {
        // 标记按行解析，失败原因压平为单行
        content.push_str(&format!("failure={}\n", failure.replace('\n', " ")));
    }
    fs::write(path, content).with_context(|| format!("写入升级标记失败: {}", path.display()))
}

pub fn read_marker(path: &Path) -> Option<UpgradeMarker> {
    let content = fs::read_to_string(path).ok()?;
    let mut from_version = String::new();
    let mut to_version = String::new();
    let mut started_at = 0;
    let mut failure = None;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("from_version=") {
            from_version = value.to_string();
        } else if let Some(value) = line.strip_prefix("to_version=") {
            to_version = value.to_string();
        } else if let Some(value) = line.strip_prefix("started_at=") {
            started_at = value.parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("failure=") {
            failure = Some(value.to_string());
        }
    }
    if from_version.is_empty() || to_version.is_empty() {
        return None;
    }
    Some(UpgradeMarker {
        from_version,
        to_version,
        started_at,
        failure,
    })
}

pub fn clear_marker(path: &Path) {
    let _ = fs::remove_file(path);
}

/// 重启后判定升级结果：标记带就地失败结果 = 失败；否则当前版本与标记
/// 目标一致 = 成功；不一致（替换后未生效/回滚）= 失败
pub fn evaluate_marker(marker: &UpgradeMarker, current_version: &str) -> (bool, String) {
    if let Some(failure) = marker.failure.as_ref().filter(|f| !f.is_empty()) {
        return (false, failure.clone());
    }
    if current_version == marker.to_version {
        (true, String::new())
    } else {
        (
            false,
            format!(
                "升级后版本不符：期望 {}，实际 {}",
                marker.to_version, current_version
            ),
        )
    }
}

/// 校验暂存文件（大小 + SHA-256）；不符时删除暂存并报错
pub fn verify_download(path: &Path, expect_sha256: &[u8], expect_size: u64) -> anyhow::Result<()> {
    let cleanup = |path: &Path| {
        let _ = fs::remove_file(path);
    };
    let meta =
        fs::metadata(path).with_context(|| format!("读取暂存文件失败: {}", path.display()))?;
    if meta.len() != expect_size {
        cleanup(path);
        bail!("制品大小不符：期望 {expect_size}，实际 {}", meta.len());
    }
    let data = fs::read(path).with_context(|| format!("读取暂存文件失败: {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let actual = hasher.finalize();
    if actual.as_slice() != expect_sha256 {
        cleanup(path);
        bail!(
            "制品 SHA-256 不符：期望 {}，实际 {}",
            hex(expect_sha256),
            hex(actual.as_slice())
        );
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 替换前自检：暂存二进制必须能启动并报告目标版本。
/// 拦截"哈希自洽但内容损坏"的制品（如上传截断后按截断内容算出的哈希）——
/// 这类制品一旦替换成功会把节点变成死进程，且 exec 后无法回滚。
pub fn verify_binary_runs(staged: &Path, expect_version: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 暂存文件需要执行位才能自检（正式替换时会再设一次）
        let mut permissions = fs::metadata(staged)
            .with_context(|| format!("读取暂存文件属性失败: {}", staged.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(staged, permissions)
            .with_context(|| format!("设置暂存文件执行权限失败: {}", staged.display()))?;
    }
    let output = match std::process::Command::new(staged).arg("--version").output() {
        Ok(output) => output,
        Err(error) => {
            let _ = fs::remove_file(staged);
            bail!(
                "启动暂存二进制自检失败（{}），已丢弃该制品: {error}",
                staged.display()
            );
        }
    };
    if !output.status.success() {
        let _ = fs::remove_file(staged);
        bail!(
            "暂存二进制自检失败（退出码 {:?}），已丢弃该制品",
            output.status.code()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !expect_version.is_empty() && !stdout.contains(expect_version) {
        let _ = fs::remove_file(staged);
        bail!(
            "暂存二进制版本自检不符：期望包含 {expect_version}，实际输出 {}",
            stdout.trim()
        );
    }
    Ok(())
}

/// 替换自身二进制。unix：chmod + rename（运行中的旧 inode 不受影响）；
/// windows：运行中 exe 不能覆盖但可以改名——旧文件改 .old 后新文件就位
pub fn apply_binary(exe: &Path, staged: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staged, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("设置执行权限失败: {}", staged.display()))?;
        fs::rename(staged, exe).with_context(|| {
            format!("替换二进制失败: {} -> {}", staged.display(), exe.display())
        })?;
    }
    #[cfg(windows)]
    {
        let old = exe.with_extension("old");
        let _ = fs::remove_file(&old);
        fs::rename(exe, &old)
            .with_context(|| format!("旧二进制改名失败: {} -> {}", exe.display(), old.display()))?;
        if let Err(error) = fs::rename(staged, exe) {
            // 回滚旧二进制：否则下次启动时 exe 路径不存在，节点无法拉起
            if let Err(rollback) = fs::rename(&old, exe) {
                return Err(anyhow::anyhow!(
                    "替换二进制失败: {error}；回滚旧二进制也失败（{rollback}），{} 需人工恢复",
                    exe.display()
                ));
            }
            return Err(anyhow::anyhow!("替换二进制失败（已回滚旧二进制）: {error}"));
        }
    }
    Ok(())
}

/// 流式下载并就地校验；失败清理暂存文件
pub async fn download_verified(
    url: &str,
    dest: &Path,
    expect_sha256: &[u8],
    expect_size: u64,
) -> anyhow::Result<()> {
    // 与 utils::http_get 相同的 provider 安装（rustls-no-provider 集成要求）
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .context("构建下载客户端失败")?;
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("下载请求失败: {url}"))?
        .error_for_status()
        .with_context(|| format!("下载返回错误状态: {url}"))?;

    let mut file =
        fs::File::create(dest).with_context(|| format!("创建暂存文件失败: {}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("下载数据读取失败: {url}"))?;
        downloaded += chunk.len() as u64;
        // 超过声明大小立即中止：既防错误制品也防恶意流式放大
        if downloaded > expect_size {
            let _ = fs::remove_file(dest);
            bail!("下载内容超过声明大小 {expect_size}");
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .with_context(|| format!("写入暂存文件失败: {}", dest.display()))?;
    }
    drop(file);
    if downloaded != expect_size {
        let _ = fs::remove_file(dest);
        bail!("下载不完整：期望 {expect_size}，实际 {downloaded}");
    }
    let actual = hasher.finalize();
    if actual.as_slice() != expect_sha256 {
        let _ = fs::remove_file(dest);
        bail!(
            "制品 SHA-256 不符：期望 {}，实际 {}",
            hex(expect_sha256),
            hex(actual.as_slice())
        );
    }
    Ok(())
}

/// 重启自身。unix 用 execve 保 PID（nohup/systemd 均兼容）；windows 旧进程
/// 退出由新进程接管（.old 残留待下次启动清理）
pub fn exec_restart(exe: &Path) -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(exe).args(&args).exec();
        bail!("exec 新二进制失败: {error:?}");
    }
    #[cfg(windows)]
    {
        std::process::Command::new(exe)
            .args(&args)
            .spawn()
            .with_context(|| format!("启动新二进制失败: {}", exe.display()))?;
        // 给子进程一点启动时间再退出，避免竞态下双双退出
        std::thread::sleep(Duration::from_millis(500));
        std::process::exit(0);
    }
}

/// 完整升级流程（在独立任务中运行）。成功路径以 exec 结束不返回。
pub async fn run_upgrade(plan: UpgradePlan) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("获取自身路径失败")?;
    let staging = staging_path(&exe);
    download_verified(&plan.url, &staging, &plan.sha256, plan.size).await?;
    // download_verified 已做流式校验，此处幂等复核（防暂存被篡改的窗口）
    verify_download(&staging, &plan.sha256, plan.size)?;
    // 自检可执行且版本正确：拦截哈希自洽但内容损坏的制品（替换即无回滚）
    verify_binary_runs(&staging, &plan.version)?;
    write_marker(
        &marker_path(&exe),
        &UpgradeMarker {
            from_version: env!("CARGO_PKG_VERSION").to_string(),
            to_version: plan.version.clone(),
            started_at: crate::utils::time::now_ts_ms(),
            failure: None,
        },
    )?;
    apply_binary(&exe, &staging)?;
    log::info!(
        "远程升级就绪（{} → {}），重启进程",
        env!("CARGO_PKG_VERSION"),
        plan.version
    );
    exec_restart(&exe)
}

/// 启动期清理上次升级残留（windows .old、崩溃遗留的 .new）
pub fn cleanup_stale_files(exe: &Path) {
    let _ = fs::remove_file(staging_path(exe));
    #[cfg(windows)]
    {
        let _ = fs::remove_file(exe.with_extension("old"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, content: &[u8]) {
        fs::write(path, content).unwrap();
    }

    fn sha256_of(content: &[u8]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hasher.finalize().to_vec()
    }

    // T6: 校验通过路径
    #[test]
    fn verify_download_accepts_matching_file() {
        let dir = std::env::temp_dir().join(format!("vnt-upg-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("staged");
        let content = b"hello vnt upgrade";
        write_file(&file, content);
        verify_download(&file, &sha256_of(content), content.len() as u64).unwrap();
        assert!(file.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    // T6: sha 不符 → 暂存被删除
    #[test]
    fn verify_download_mismatch_deletes_staging() {
        let dir = std::env::temp_dir().join(format!("vnt-upg-mm-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("staged");
        write_file(&file, b"corrupted");
        let err = verify_download(&file, &sha256_of(b"expected"), 9).unwrap_err();
        assert!(err.to_string().contains("SHA-256"));
        assert!(!file.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    // T6: 大小不符 → 同样清理
    #[test]
    fn verify_download_size_mismatch_deletes_staging() {
        let dir = std::env::temp_dir().join(format!("vnt-upg-sz-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("staged");
        write_file(&file, b"12345678");
        assert!(verify_download(&file, &sha256_of(b"12345678"), 7).is_err());
        assert!(!file.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    // T6: unix 替换路径（rename 原子替换 + 执行位）
    #[cfg(unix)]
    #[test]
    fn apply_binary_replaces_and_sets_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vnt-upg-ap-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("vnt_fake");
        let staged = dir.join("vnt_fake.new");
        write_file(&exe, b"old");
        write_file(&staged, b"new-binary");
        apply_binary(&exe, &staged).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new-binary");
        assert!(!staged.exists());
        let mode = fs::metadata(&exe).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "执行位未设置: {mode:o}");
        fs::remove_dir_all(&dir).unwrap();
    }

    // T6b: 自检拒绝损坏二进制（不可执行/版本不符），并清理暂存
    #[cfg(unix)]
    #[test]
    fn verify_binary_runs_rejects_corrupted_staging() {
        let dir = std::env::temp_dir().join(format!("vnt-upg-br-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let staged = dir.join("staged");
        // 截断的 ELF（模拟上传被截断的制品）
        write_file(&staged, b"\x7fELF truncated garbage not executable");
        let err = verify_binary_runs(&staged, "2.0.99").unwrap_err();
        assert!(!err.to_string().is_empty());
        assert!(!staged.exists(), "自检失败的暂存应被清理");
        fs::remove_dir_all(&dir).unwrap();
    }

    // T6b: 自检通过真实脚本（版本匹配）
    #[cfg(unix)]
    #[test]
    fn verify_binary_runs_accepts_matching_script() {
        let dir = std::env::temp_dir().join(format!("vnt-upg-bs-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let staged = dir.join("staged");
        write_file(&staged, b"#!/bin/sh\necho 'vnt2 2.0.99'\n");
        verify_binary_runs(&staged, "2.0.99").unwrap();
        assert!(staged.exists());
        // 版本不符 → 拒绝并清理
        let staged2 = dir.join("staged2");
        write_file(&staged2, b"#!/bin/sh\necho 'vnt2 2.0.10'\n");
        assert!(verify_binary_runs(&staged2, "2.0.99").is_err());
        assert!(!staged2.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    // T8: 标记文件 roundtrip
    #[test]
    fn marker_round_trip() {
        let dir = std::env::temp_dir().join(format!("vnt-upg-mk-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pending");
        let marker = UpgradeMarker {
            from_version: "2.0.11".into(),
            to_version: "2.0.12".into(),
            started_at: 1699000000,
            failure: None,
        };
        write_marker(&path, &marker).unwrap();
        assert_eq!(read_marker(&path), Some(marker));
        clear_marker(&path);
        assert_eq!(read_marker(&path), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    // F3: 就地失败结果随标记持久化（含换行压平），evaluate 直接采用
    #[test]
    fn marker_failure_outcome_round_trip() {
        let dir = std::env::temp_dir().join(format!("vnt-upg-mf-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pending");
        let marker = UpgradeMarker {
            from_version: "2.0.11".into(),
            to_version: "2.0.12".into(),
            started_at: 1699000000,
            failure: Some("下载失败: 连接超时\n第二行".into()),
        };
        write_marker(&path, &marker).unwrap();
        let read = read_marker(&path).unwrap();
        // 换行被压平为单行，其余字段保留
        assert_eq!(read.failure.as_deref(), Some("下载失败: 连接超时 第二行"));
        let (ok, reason) = evaluate_marker(&read, "2.0.11");
        assert!(!ok);
        assert!(reason.contains("连接超时"));
        fs::remove_dir_all(&dir).unwrap();
    }

    // T8: 残缺标记（无目标版本）判无效
    #[test]
    fn malformed_marker_is_none() {
        let dir = std::env::temp_dir().join(format!("vnt-upg-bm-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pending");
        write_file(&path, b"from_version=1.0\nstarted_at=5\n");
        assert_eq!(read_marker(&path), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    // T8: 重启后判定——版本匹配成功 / 不匹配失败带原因
    #[test]
    fn evaluate_marker_compares_versions() {
        let marker = UpgradeMarker {
            from_version: "2.0.11".into(),
            to_version: "2.0.12".into(),
            started_at: 0,
            failure: None,
        };
        let (ok, _) = evaluate_marker(&marker, "2.0.12");
        assert!(ok);
        let (ok, reason) = evaluate_marker(&marker, "2.0.11");
        assert!(!ok);
        assert!(reason.contains("2.0.12"));
    }

    // 路径派生：暂存与标记与可执行文件同目录
    #[test]
    fn derived_paths_stay_beside_executable() {
        let exe = Path::new("/opt/vnt/bin/vnt2_web");
        assert_eq!(staging_path(exe), Path::new("/opt/vnt/bin/vnt2_web.new"));
        assert_eq!(
            marker_path(exe),
            Path::new("/opt/vnt/bin/vnt2_web.upgrade-pending")
        );
    }
}
