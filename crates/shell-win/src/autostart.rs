//! 用户级开机启动（HKCU Run），不需要管理员权限，也不修改系统级启动项。
//!
//! 值名固定为 `DeskShell`，内容为带引号的当前可执行文件路径加 `app` 参数。
//! 安装器与托盘菜单共用这里的逻辑，避免两处写入不一致。

use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
            RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
        },
    },
    core::PCWSTR,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "DeskShell";

pub fn command_line(executable: &std::path::Path) -> String {
    format!("\"{}\" app", executable.display())
}

pub fn is_enabled() -> Result<bool, String> {
    let key = open_key(KEY_QUERY_VALUE)?;
    let name: Vec<u16> = VALUE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut buffer = [0u8; 2048];
    let mut length = buffer.len() as u32;
    // SAFETY: 名称与缓冲区在调用期间有效；只读取字符串值。
    let result = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buffer.as_mut_ptr()),
            Some(&mut length),
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if result != ERROR_SUCCESS {
        return Err(format!("读取开机启动项失败：{result:?}"));
    }
    Ok(length > 0)
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let key = open_key(KEY_SET_VALUE)?;
    let name: Vec<u16> = VALUE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    if !enabled {
        // SAFETY: 只删除本程序固定值名。
        let result = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
        if result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        return Err(format!("移除开机启动项失败：{result:?}"));
    }
    let executable =
        std::env::current_exe().map_err(|error| format!("获取当前程序路径失败：{error}"))?;
    let command = command_line(&executable);
    let bytes: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 名称与值缓冲区在调用期间有效；REG_SZ 以 UTF-16 字节长度传入。
    let result = unsafe {
        RegSetValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            REG_SZ,
            Some(std::slice::from_raw_parts(
                bytes.as_ptr() as *const u8,
                bytes.len() * std::mem::size_of::<u16>(),
            )),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(format!("写入开机启动项失败：{result:?}"));
    }
    Ok(())
}

fn open_key(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<KeyHandle, String> {
    let path: Vec<u16> = RUN_KEY.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = HKEY::default();
    // SAFETY: 路径以 NUL 结尾；成功时句柄由 KeyHandle 关闭。
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            Some(0),
            access,
            &mut key,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(format!("打开用户启动项注册表失败：{result:?}"));
    }
    Ok(KeyHandle(key))
}

struct KeyHandle(HKEY);

impl Drop for KeyHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄由 open_key 打开且只在此处关闭一次。
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_executable_and_adds_app_argument() {
        let command = command_line(std::path::Path::new(
            r"C:\Program Files\DeskShell\deskshell.exe",
        ));
        assert_eq!(command, r#""C:\Program Files\DeskShell\deskshell.exe" app"#);
    }
}
