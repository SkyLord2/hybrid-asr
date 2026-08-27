use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::SystemTime;

use crate::types::AsrState;

/// ASR 模型资源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelResourceKind {
    /// ASR 主模型。
    Asr,
    /// VAD 模型。
    Vad,
}

impl ModelResourceKind {
    /// 返回稳定的资源类型字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asr => "asr",
            Self::Vad => "vad",
        }
    }
}

/// 模型生命周期阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelLifecyclePhase {
    /// 检测到模型缺失。
    Missing,
    /// 模型正在下载或安装。
    Downloading,
    /// 模型已经准备完成。
    Downloaded,
    /// 模型下载或安装失败。
    Failed,
}

impl ModelLifecyclePhase {
    /// 返回稳定的阶段字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Downloading => "downloading",
            Self::Downloaded => "downloaded",
            Self::Failed => "failed",
        }
    }
}

/// ASR 状态变更原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrStateChangeReason {
    /// 会话已创建。
    SessionCreated,
    /// 开始初始化。
    StartRequested,
    /// 初始化成功。
    StartSucceeded,
    /// 开始识别。
    RecognitionRequested,
    /// 已暂停。
    PauseRequested,
    /// 已恢复。
    ResumeRequested,
    /// 会话完成。
    FinishRequested,
    /// 准备下一轮完成。
    PrepareNextTurnRequested,
    /// 已重置。
    ResetRequested,
    /// 出现错误。
    ErrorOccurred,
}

/// 状态迁移事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsrStateTransitionEvent {
    /// 变更前状态。
    pub from: AsrState,
    /// 变更后状态。
    pub to: AsrState,
    /// 状态变更原因。
    pub reason: AsrStateChangeReason,
    /// 事件时间戳。
    pub timestamp: SystemTime,
}

/// 模型生命周期事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelLifecycleEvent {
    /// 资源类型。
    pub resource_kind: ModelResourceKind,
    /// 生命周期阶段。
    pub phase: ModelLifecyclePhase,
    /// 已下载字节数。
    pub downloaded_bytes: Option<u64>,
    /// 总字节数。
    pub total_bytes: Option<u64>,
    /// 目标目录。
    pub target_dir: PathBuf,
    /// 事件说明。
    pub message: String,
    /// 事件时间戳。
    pub timestamp: SystemTime,
}

/// 会话事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvent {
    /// 会话事件说明。
    pub message: String,
    /// 事件时间戳。
    pub timestamp: SystemTime,
}

/// ASR 对外统一事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrEvent {
    /// 状态变更事件。
    StateChanged(AsrStateTransitionEvent),
    /// 会话事件。
    Session(SessionEvent),
    /// 模型生命周期事件。
    Model(ModelLifecycleEvent),
    /// 错误事件。
    Error(String),
}

/// 统一 ASR 事件总线。
#[derive(Debug)]
pub struct EventHub {
    subscribers: Mutex<Vec<Sender<AsrEvent>>>,
    closed: AtomicBool,
}

/// `hybrid-asr` 对内对外统一使用的事件总线类型别名。
pub type AsrEventHub = EventHub;

impl EventHub {
    fn disconnected_receiver() -> Receiver<AsrEvent> {
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        receiver
    }

    /// 注册一个订阅者。
    pub fn subscribe(&self) -> Receiver<AsrEvent> {
        if self.closed.load(Ordering::SeqCst) {
            return Self::disconnected_receiver();
        }

        let mut subscribers = self.subscribers.lock().expect("asr event hub poisoned");
        if self.closed.load(Ordering::SeqCst) {
            return Self::disconnected_receiver();
        }

        let (sender, receiver) = mpsc::channel();
        subscribers.push(sender);
        receiver
    }

    /// 显式关闭事件总线。
    ///
    /// 关闭后会立即释放所有 sender，让已有 receiver 自然收到 disconnected。
    /// 这是一个幂等操作，可安全重复调用。
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }

        self.subscribers
            .lock()
            .expect("asr event hub poisoned")
            .clear();
    }

    /// 广播事件，并清理已经断开的订阅者。
    ///
    /// 关闭后的事件总线不再投递任何事件。
    pub fn emit(&self, event: AsrEvent) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }

        let mut subscribers = self.subscribers.lock().expect("asr event hub poisoned");
        if self.closed.load(Ordering::SeqCst) {
            subscribers.clear();
            return;
        }

        subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }
}

impl Default for EventHub {
    fn default() -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    use super::{AsrEvent, EventHub};

    fn assert_receiver_disconnected<T>(result: Result<T, RecvTimeoutError>) {
        assert!(matches!(result, Err(RecvTimeoutError::Disconnected)));
    }

    #[test]
    fn close_disconnects_existing_receivers() {
        let hub = EventHub::default();
        let receiver = hub.subscribe();

        hub.close();

        assert_receiver_disconnected(receiver.recv_timeout(Duration::from_millis(50)));
    }

    #[test]
    fn close_is_idempotent() {
        let hub = EventHub::default();

        hub.close();
        hub.close();
    }

    #[test]
    fn emit_becomes_noop_after_close() {
        let hub = EventHub::default();
        let receiver = hub.subscribe();
        hub.close();

        hub.emit(AsrEvent::Error("should not be delivered".to_string()));

        assert_receiver_disconnected(receiver.recv_timeout(Duration::from_millis(50)));
    }

    #[test]
    fn subscribe_after_close_returns_disconnected_receiver() {
        let hub = EventHub::default();
        hub.close();

        let receiver = hub.subscribe();

        assert_receiver_disconnected(receiver.recv_timeout(Duration::from_millis(50)));
    }
}
