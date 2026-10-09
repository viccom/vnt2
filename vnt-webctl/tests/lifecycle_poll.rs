//! 启动轮询步进决策表测试（计划 §4 Task 3；先红后绿）。
//!
//! 决策表锁定轮询循环的退出条件：终态返回、超时、连续非终态错误的容忍上限。

use vnt_webctl::lifecycle::{PollDecision, PollStatus, PollStep, poll_step};

/// 从未进入过 Starting：首轮即 Stopped，视为「没有可观察的启动」（成功退出）。
#[test]
fn stopped_without_starting_is_done() {
    let decision = poll_step(PollStep {
        status: PollStatus::Stopped,
        saw_starting: false,
        elapsed_ms: 0,
        consecutive_errors: 0,
    });
    assert_eq!(decision, PollDecision::DoneStopped);
}

/// Starting 且未超时：继续等待。
#[test]
fn starting_before_timeout_waits() {
    let decision = poll_step(PollStep {
        status: PollStatus::Starting,
        saw_starting: true,
        elapsed_ms: 29_000,
        consecutive_errors: 0,
    });
    assert_eq!(decision, PollDecision::Wait);
}

/// Starting -> Running：成功。
#[test]
fn starting_to_running_is_success() {
    let decision = poll_step(PollStep {
        status: PollStatus::Running,
        saw_starting: true,
        elapsed_ms: 5_000,
        consecutive_errors: 0,
    });
    assert_eq!(decision, PollDecision::DoneRunning);
}

/// Starting -> Stopped：启动失败回落，报错。
#[test]
fn starting_to_stopped_is_failed() {
    let decision = poll_step(PollStep {
        status: PollStatus::Stopped,
        saw_starting: true,
        elapsed_ms: 5_000,
        consecutive_errors: 0,
    });
    assert_eq!(decision, PollDecision::DoneFailed);
}

/// 超时优先于当轮状态判定：即使当轮读到 Running，超时也报错。
#[test]
fn timeout_takes_precedence() {
    for status in [
        PollStatus::Stopped,
        PollStatus::Starting,
        PollStatus::Running,
    ] {
        let decision = poll_step(PollStep {
            status,
            saw_starting: true,
            elapsed_ms: 30_000,
            consecutive_errors: 0,
        });
        assert_eq!(decision, PollDecision::FailTimeout, "status={status:?}");
    }
}

/// 非终态错误（竞态窗口的 code:-1）连续 1-2 次：容忍，继续等待。
#[test]
fn transient_errors_within_tolerance_wait() {
    for consecutive_errors in [0, 1, 2] {
        let decision = poll_step(PollStep {
            status: PollStatus::TransientError,
            saw_starting: true,
            elapsed_ms: 5_000,
            consecutive_errors,
        });
        assert_eq!(
            decision,
            PollDecision::Wait,
            "consecutive_errors={consecutive_errors}"
        );
    }
}

/// 连续第 3 次非终态错误：不再容忍，报错。
#[test]
fn third_consecutive_transient_error_fails() {
    let decision = poll_step(PollStep {
        status: PollStatus::TransientError,
        saw_starting: true,
        elapsed_ms: 5_000,
        consecutive_errors: 3,
    });
    assert_eq!(decision, PollDecision::FailTransient);
}

/// 非终态错误同样受超时约束。
#[test]
fn transient_error_after_timeout_fails() {
    let decision = poll_step(PollStep {
        status: PollStatus::TransientError,
        saw_starting: false,
        elapsed_ms: 31_000,
        consecutive_errors: 0,
    });
    assert_eq!(decision, PollDecision::FailTimeout);
}
