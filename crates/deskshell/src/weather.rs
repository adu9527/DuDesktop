//! 天气小部件数据：Open-Meteo 公开接口，无需 API Key。
//!
//! 网络请求只在后台线程执行；UI 线程只消费结果。失败时保留上一份数据，
//! 由界面显示错误提示，不伪造温度。

use std::ffi::c_void;

use serde_json::Value;
use windows::{
    Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER,
        WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
        WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
        WinHttpSendRequest, WinHttpSetTimeouts,
    },
    core::PCWSTR,
};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const HTTPS_PORT: u16 = 443;

#[derive(Debug, Clone, PartialEq)]
pub struct WeatherData {
    pub city: String,
    pub temperature: f32,
    pub text: String,
    pub code: u8,
}

/// 查询城市当前天气。会阻塞当前线程，调用方必须放在后台线程。
pub fn fetch(city: &str) -> Result<WeatherData, String> {
    let query = percent_encode(city.trim());
    let search = get_json(
        "geocoding-api.open-meteo.com",
        &format!("/v1/search?name={query}&count=1&language=zh&format=json"),
    )?;
    let result = search
        .get("results")
        .and_then(Value::as_array)
        .and_then(|results| results.first())
        .ok_or_else(|| format!("找不到城市：{city}"))?;
    let latitude = result
        .get("latitude")
        .and_then(Value::as_f64)
        .ok_or("天气接口缺少纬度")?;
    let longitude = result
        .get("longitude")
        .and_then(Value::as_f64)
        .ok_or("天气接口缺少经度")?;
    let name = result
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(city)
        .to_string();
    let forecast = get_json(
        "api.open-meteo.com",
        &format!(
            "/v1/forecast?latitude={latitude:.4}&longitude={longitude:.4}\
             &current=temperature_2m,weather_code&timezone=auto"
        ),
    )?;
    let current = forecast.get("current").ok_or("天气接口缺少 current")?;
    let temperature = current
        .get("temperature_2m")
        .and_then(Value::as_f64)
        .ok_or("天气接口缺少温度")? as f32;
    let code = current
        .get("weather_code")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u8;
    Ok(WeatherData {
        city: name,
        temperature: temperature.round(),
        text: weather_text(code).into(),
        code,
    })
}

fn weather_text(code: u8) -> &'static str {
    match code {
        0 => "晴",
        1 => "大部晴朗",
        2 => "多云",
        3 => "阴",
        45 | 48 => "雾",
        51 | 53 | 55 => "毛毛雨",
        56 | 57 => "冻雨",
        61 | 63 | 65 => "雨",
        66 | 67 => "冻雨",
        71 | 73 | 75 | 77 => "雪",
        80..=82 => "阵雨",
        85 | 86 => "阵雪",
        95 => "雷雨",
        96 | 99 => "雷雨伴冰雹",
        _ => "未知",
    }
}

fn get_json(host: &str, path: &str) -> Result<Value, String> {
    let bytes = http_get(host, path)?;
    serde_json::from_slice(&bytes).map_err(|error| format!("天气响应 JSON 无效：{error}"))
}

fn http_get(host: &str, path: &str) -> Result<Vec<u8>, String> {
    let agent = wide("DeskShell/0.1");
    // SAFETY: 字符串在调用期间有效；句柄由 Internet 释放。
    let session = Internet(unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    });
    if session.0.is_null() {
        return Err(format!(
            "初始化 WinHTTP 失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: 会话句柄有效；超时只影响本次请求。
    unsafe {
        WinHttpSetTimeouts(session.0, 3_000, 3_000, 3_000, 5_000)
            .map_err(|error| format!("设置网络超时失败：{error}"))?;
    }
    let host_wide = wide(host);
    // SAFETY: 服务器名以 NUL 结尾。
    let connect =
        Internet(unsafe { WinHttpConnect(session.0, PCWSTR(host_wide.as_ptr()), HTTPS_PORT, 0) });
    if connect.0.is_null() {
        return Err(format!(
            "连接天气服务失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    let path_wide = wide(path);
    let verb = wide("GET");
    // SAFETY: 路径与动词在调用期间有效；使用 HTTPS。
    let request = Internet(unsafe {
        WinHttpOpenRequest(
            connect.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        )
    });
    if request.0.is_null() {
        return Err(format!(
            "创建天气请求失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: 请求句柄有效；无请求体。
    unsafe {
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)
            .map_err(|error| format!("发送天气请求失败：{error}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|error| format!("读取天气响应失败：{error}"))?;
    }
    let mut status = 0u32;
    let mut length = std::mem::size_of::<u32>() as u32;
    // SAFETY: 输出缓冲区与长度匹配。
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut length,
            std::ptr::null_mut(),
        )
    }
    .map_err(|error| format!("读取天气状态码失败：{error}"))?;
    if status != 200 {
        return Err(format!("天气服务返回 HTTP {status}"));
    }
    let mut body = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let mut read = 0u32;
        // SAFETY: 缓冲区有效；读取长度由系统写入。
        unsafe {
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len() as u32,
                &mut read,
            )
        }
        .map_err(|error| format!("读取天气数据失败：{error}"))?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read as usize]);
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(format!("天气响应超过 {} 字节", MAX_RESPONSE_BYTES));
        }
    }
    Ok(body)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 网络句柄所有权；析构时关闭。
struct Internet(*mut c_void);

impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 句柄由 WinHTTP 创建且只在此处关闭一次。
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_codes_map_to_chinese() {
        assert_eq!(weather_text(0), "晴");
        assert_eq!(weather_text(61), "雨");
        assert_eq!(weather_text(95), "雷雨");
    }

    #[test]
    fn percent_encoding_handles_chinese() {
        assert_eq!(percent_encode("上海"), "%E4%B8%8A%E6%B5%B7");
        assert_eq!(percent_encode("a b"), "a%20b");
    }
}
