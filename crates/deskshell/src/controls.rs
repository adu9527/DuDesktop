//! 快捷控制数据：系统音量、WLAN 名称与蓝牙无线电状态。
//!
//! 与音乐小部件相同，所有 COM/WinRT 调用都在后台 MTA 线程执行；UI 线程只消费
//! `ControlsView` 快照。只有快捷控制小部件启用时才会创建线程。

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use render::ControlsView;
use windows::{
    Devices::Radios::{Radio, RadioKind, RadioState},
    Networking::Connectivity::NetworkInformation,
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{
            Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, eMultimedia,
            eRender,
        },
        System::{
            Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize, STGM_READ,
            },
        },
    },
};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
pub enum ControlsCommand {
    SetVolume(u8),
    ToggleMute,
    ToggleBluetooth,
    Refresh,
}

pub struct ControlsWorker {
    stop: Arc<AtomicBool>,
    commands: mpsc::Sender<ControlsCommand>,
    store: Arc<Mutex<Option<ControlsView>>>,
    thread: Option<JoinHandle<()>>,
}

impl ControlsWorker {
    pub fn spawn(notify: isize, message: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let store = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::channel();
        let thread_stop = Arc::clone(&stop);
        let thread_store = Arc::clone(&store);
        let thread = thread::Builder::new()
            .name("widget-controls".into())
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
                        if *slot != Some(state.clone()) {
                            *slot = Some(state);
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

    pub fn send(&self, command: ControlsCommand) {
        let _ = self.commands.send(command);
    }

    pub fn take(&self) -> Option<ControlsView> {
        self.store
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl Drop for ControlsWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.commands.send(ControlsCommand::Refresh);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Audio {
    endpoint: IAudioEndpointVolume,
    device: String,
}

fn open_audio() -> Result<Audio, String> {
    // SAFETY: COM 已在工作线程初始化；接口指针只在本次调用内使用。
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|error| format!("创建音频设备枚举器失败：{error}"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|error| format!("获取默认播放设备失败：{error}"))?;
        let endpoint: IAudioEndpointVolume = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|error| format!("激活音量接口失败：{error}"))?;
        let mut name = "系统默认输出".to_string();
        if let Ok(store) = device.OpenPropertyStore(STGM_READ)
            && let Ok(mut variant) = store.GetValue(&PKEY_Device_FriendlyName)
            && let Ok(value) = PropVariantToStringAlloc(&variant)
        {
            if let Ok(text) = value.to_string() {
                name = text;
            }
            CoTaskMemFree(Some(value.0 as *const core::ffi::c_void));
            let _ = PropVariantClear(&mut variant);
        }
        Ok(Audio {
            endpoint,
            device: name,
        })
    }
}

fn read_state() -> ControlsView {
    let mut state = ControlsView::default();
    if let Ok(audio) = open_audio() {
        // SAFETY: 音量接口由本函数持有，调用期间有效。
        unsafe {
            if let Ok(level) = audio.endpoint.GetMasterVolumeLevelScalar() {
                state.volume = (level * 100.0).round().clamp(0.0, 100.0) as u8;
            }
            if let Ok(muted) = audio.endpoint.GetMute() {
                state.muted = muted.as_bool();
            }
        }
        state.device = audio.device;
    }
    state.wifi = connection_ssid().unwrap_or_default();
    state.wifi_on = wifi_radio_on().unwrap_or(!state.wifi.is_empty());
    state.bluetooth = bluetooth_state();
    state
}

fn connection_ssid() -> Option<String> {
    let profile = NetworkInformation::GetInternetConnectionProfile().ok()?;
    let details = profile.WlanConnectionProfileDetails().ok()?;
    details.GetConnectedSsid().ok().map(|ssid| ssid.to_string())
}

fn wifi_radio_on() -> Option<bool> {
    let list = Radio::GetRadiosAsync().ok()?.get().ok()?;
    for index in 0..list.Size().ok()? {
        let radio = list.GetAt(index).ok()?;
        if radio.Kind().ok()? == RadioKind::WiFi {
            return radio.State().ok().map(|state| state == RadioState::On);
        }
    }
    None
}

fn bluetooth_state() -> Option<bool> {
    let list = Radio::GetRadiosAsync().ok()?.get().ok()?;
    for index in 0..list.Size().ok()? {
        let radio = list.GetAt(index).ok()?;
        if radio.Kind().ok()? == RadioKind::Bluetooth {
            return radio.State().ok().map(|state| state == RadioState::On);
        }
    }
    None
}

fn apply_command(command: ControlsCommand) {
    match command {
        ControlsCommand::SetVolume(volume) => {
            if let Ok(audio) = open_audio() {
                // SAFETY: 接口由本函数持有；事件上下文可为空。
                unsafe {
                    let _ = audio
                        .endpoint
                        .SetMasterVolumeLevelScalar(f32::from(volume) / 100.0, std::ptr::null());
                    let _ = audio.endpoint.SetMute(false, std::ptr::null());
                }
            }
        }
        ControlsCommand::ToggleMute => {
            if let Ok(audio) = open_audio() {
                // SAFETY: 接口由本函数持有；只读取并写回静音状态。
                unsafe {
                    if let Ok(muted) = audio.endpoint.GetMute() {
                        let _ = audio.endpoint.SetMute(!muted.as_bool(), std::ptr::null());
                    }
                }
            }
        }
        ControlsCommand::ToggleBluetooth => {
            let Some(list) = Radio::GetRadiosAsync().ok().and_then(|op| op.get().ok()) else {
                // 读不到无线电时退回系统设置，避免按钮无反馈。
                let _ =
                    shell_win::deskfiles::open_path(std::path::Path::new("ms-settings:bluetooth"));
                return;
            };
            let mut handled = false;
            if let Ok(size) = list.Size() {
                for index in 0..size {
                    let Ok(radio) = list.GetAt(index) else {
                        continue;
                    };
                    if radio.Kind().ok() != Some(RadioKind::Bluetooth) {
                        continue;
                    }
                    handled = true;
                    let next = if radio.State().ok() == Some(RadioState::On) {
                        RadioState::Off
                    } else {
                        RadioState::On
                    };
                    let ok = radio
                        .SetStateAsync(next)
                        .ok()
                        .and_then(|operation| operation.get().ok())
                        .is_some();
                    if !ok {
                        let _ = shell_win::deskfiles::open_path(std::path::Path::new(
                            "ms-settings:bluetooth",
                        ));
                    }
                    break;
                }
            }
            if !handled {
                let _ =
                    shell_win::deskfiles::open_path(std::path::Path::new("ms-settings:bluetooth"));
            }
        }
        ControlsCommand::Refresh => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_view_is_safe() {
        let view = ControlsView::default();
        assert!(view.volume <= 100);
        assert!(!view.device.is_empty());
    }
}
