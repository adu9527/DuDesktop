//! 音乐小部件数据：Windows 系统媒体会话（SMTC）。
//!
//! QQ 音乐、Edge 等注册了 SMTC 的播放器都可读取曲目并控制播放；不注入进程、
//! 不抓窗口。查询与控制在后台线程执行，UI 线程只消费快照。

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use windows::{
    Media::Control::{
        GlobalSystemMediaTransportControlsSession,
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    },
    Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MusicState {
    pub title: String,
    pub artist: String,
    /// 播放器名称，例如 `QQMusic.exe`。
    pub source: String,
    pub playing: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum MusicCommand {
    Toggle,
    Next,
    Previous,
}

pub struct MusicWorker {
    stop: Arc<AtomicBool>,
    commands: mpsc::Sender<MusicCommand>,
    store: Arc<Mutex<Option<MusicState>>>,
    thread: Option<JoinHandle<()>>,
}

impl MusicWorker {
    pub fn spawn(notify: isize, message: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let store = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::channel();
        let thread_stop = Arc::clone(&stop);
        let thread_store = Arc::clone(&store);
        let thread = thread::Builder::new()
            .name("widget-music".into())
            .spawn(move || {
                // SAFETY: 后台线程自行初始化 MTA；结束时配对反初始化。
                let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
                loop {
                    if thread_stop.load(Ordering::Acquire) {
                        break;
                    }
                    match receiver.recv_timeout(POLL_INTERVAL) {
                        Ok(command) => apply_command(command),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    let state = read_state();
                    let changed = {
                        let mut slot = thread_store
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        if *slot != state {
                            *slot = state;
                            true
                        } else {
                            false
                        }
                    };
                    if changed {
                        shell_win::shellui::post_message_raw(notify, message);
                    }
                }
                if initialized {
                    // SAFETY: 与本线程的 CoInitializeEx 配对。
                    unsafe { CoUninitialize() };
                }
            })
            .ok();
        Self {
            stop,
            commands: sender,
            store,
            thread,
        }
    }

    pub fn send(&self, command: MusicCommand) {
        let _ = self.commands.send(command);
    }

    pub fn take(&self) -> Option<MusicState> {
        self.store
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl Drop for MusicWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.commands.send(MusicCommand::Toggle);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn manager() -> Result<GlobalSystemMediaTransportControlsSessionManager, String> {
    GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
        .map_err(|error| format!("获取媒体会话管理器失败：{error}"))?
        .get()
        .map_err(|error| format!("等待媒体会话管理器失败：{error}"))
}

fn current_session(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
) -> Result<Option<GlobalSystemMediaTransportControlsSession>, String> {
    let sessions = manager
        .GetSessions()
        .map_err(|error| format!("枚举媒体会话失败：{error}"))?;
    let count = sessions
        .Size()
        .map_err(|error| format!("读取媒体会话数量失败：{error}"))?;
    let mut first = None;
    for index in 0..count {
        let session = sessions
            .GetAt(index)
            .map_err(|error| format!("读取媒体会话失败：{error}"))?;
        let playing = session
            .GetPlaybackInfo()
            .ok()
            .and_then(|info| info.PlaybackStatus().ok())
            == Some(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
        if playing {
            return Ok(Some(session));
        }
        if first.is_none() {
            first = Some(session);
        }
    }
    Ok(first)
}

fn read_state() -> Option<MusicState> {
    let manager = manager().ok()?;
    let session = current_session(&manager).ok()??;
    let source = session
        .SourceAppUserModelId()
        .map(|value| value.to_string())
        .unwrap_or_default();
    let properties = session.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
    let title = properties
        .Title()
        .map(|value| value.to_string())
        .unwrap_or_default();
    let artist = properties
        .Artist()
        .map(|value| value.to_string())
        .unwrap_or_default();
    if title.is_empty() {
        return None;
    }
    let playing = session
        .GetPlaybackInfo()
        .ok()
        .and_then(|info| info.PlaybackStatus().ok())
        == Some(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
    Some(MusicState {
        title,
        artist,
        source,
        playing,
    })
}

fn apply_command(command: MusicCommand) {
    let Ok(manager) = manager() else {
        return;
    };
    let Ok(Some(session)) = current_session(&manager) else {
        return;
    };
    let operation = match command {
        // QQ 音乐等播放器对 Toggle 与显式暂停/播放的支持不一致，依次尝试。
        MusicCommand::Toggle => {
            let playing = session
                .GetPlaybackInfo()
                .ok()
                .and_then(|info| info.PlaybackStatus().ok())
                == Some(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
            let explicit = if playing {
                session.TryPauseAsync()
            } else {
                session.TryPlayAsync()
            };
            match explicit {
                Ok(operation) if operation.get().unwrap_or(false) => return,
                _ => session.TryTogglePlayPauseAsync(),
            }
        }
        MusicCommand::Next => session.TrySkipNextAsync(),
        MusicCommand::Previous => session.TrySkipPreviousAsync(),
    };
    if let Ok(operation) = operation {
        let _ = operation.get();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_equality_ignores_nothing() {
        let left = MusicState {
            title: "A".into(),
            artist: "B".into(),
            source: "QQMusic.exe".into(),
            playing: true,
        };
        let mut right = left.clone();
        assert_eq!(left, right);
        right.playing = false;
        assert_ne!(left, right);
    }
}
