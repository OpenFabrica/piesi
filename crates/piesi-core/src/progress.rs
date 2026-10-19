//! 进度回调与取消令牌。驱动层按字节/条目上报，这里负责节流，避免跨 FFI 回调过密。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 一次进度快照。`current` 是当前正在处理的条目名（已解码）。
#[derive(Clone, Debug, Default)]
pub struct ProgressEvent {
    pub done_bytes: u64,
    pub total_bytes: Option<u64>,
    pub done_entries: u64,
    pub total_entries: Option<u64>,
    pub current: Option<String>,
}

/// 进度接收方。UI 层经 UniFFI 实现它；CLI 直接打印。
pub trait Progress: Sync + Send {
    /// 收到一次进度快照。
    fn report(&self, event: &ProgressEvent);
}

/// 不上报任何进度的空实现。
pub struct NoProgress;

impl Progress for NoProgress {
    fn report(&self, _event: &ProgressEvent) {}
}

/// 取消令牌：跨线程共享的布尔标志，驱动层每处理一小块数据就检查一次。
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// 新建一个未取消的令牌。
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求取消；所有持有该令牌的操作会在下一次检查点返回 `Error::Cancelled`。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// 是否已请求取消。
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// 已取消则返回错误，用在循环里当检查点。
    pub fn check(&self) -> crate::Result<()> {
        if self.is_cancelled() {
            Err(crate::Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// 一次操作的上下文：进度接收方 + 取消令牌。所有驱动 API 都带它。
pub struct OpContext<'a> {
    pub progress: &'a dyn Progress,
    pub cancel: &'a CancelToken,
}

impl<'a> OpContext<'a> {
    /// 组装上下文。
    pub fn new(progress: &'a dyn Progress, cancel: &'a CancelToken) -> Self {
        Self { progress, cancel }
    }

    /// 不报进度、不可取消的上下文，测试和简单调用用。
    pub fn silent() -> OpContext<'static> {
        static NONE: NoProgress = NoProgress;
        static TOKEN: std::sync::OnceLock<CancelToken> = std::sync::OnceLock::new();
        OpContext { progress: &NONE, cancel: TOKEN.get_or_init(CancelToken::new) }
    }
}

/// 节流器：只有距上次上报 ≥50 ms 或新增 ≥4 MB 时才真正回调，
/// 因为每次回调都要跨 FFI 进主线程，太密会拖慢解压本身。
pub struct Throttle<'a> {
    ctx: &'a OpContext<'a>,
    last_report: Instant,
    last_bytes: u64,
    pub event: ProgressEvent,
}

impl<'a> Throttle<'a> {
    const MIN_INTERVAL: Duration = Duration::from_millis(50);
    const MIN_BYTES: u64 = 4 * 1024 * 1024;

    /// 新建节流器，`total_bytes`/`total_entries` 已知时填上，UI 才能显示百分比。
    pub fn new(ctx: &'a OpContext<'a>, total_bytes: Option<u64>, total_entries: Option<u64>) -> Self {
        Self {
            ctx,
            last_report: Instant::now() - Self::MIN_INTERVAL,
            last_bytes: 0,
            event: ProgressEvent { total_bytes, total_entries, ..Default::default() },
        }
    }

    /// 累加已处理字节并视情况上报；同时充当取消检查点。
    pub fn add_bytes(&mut self, n: u64) -> crate::Result<()> {
        self.event.done_bytes += n;
        self.maybe_report(false)
    }

    /// 开始处理新条目：更新计数与当前名字，强制上报一次。
    pub fn begin_entry(&mut self, name: &str) -> crate::Result<()> {
        self.event.done_entries += 1;
        self.event.current = Some(name.to_owned());
        self.maybe_report(true)
    }

    /// 操作结束时强制上报最终状态。
    pub fn finish(&mut self) {
        self.ctx.progress.report(&self.event);
    }

    /// 真正决定要不要回调的地方。
    fn maybe_report(&mut self, force: bool) -> crate::Result<()> {
        self.ctx.cancel.check()?;
        let now = Instant::now();
        if force
            || now.duration_since(self.last_report) >= Self::MIN_INTERVAL
            || self.event.done_bytes - self.last_bytes >= Self::MIN_BYTES
        {
            self.ctx.progress.report(&self.event);
            self.last_report = now;
            self.last_bytes = self.event.done_bytes;
        }
        Ok(())
    }
}
