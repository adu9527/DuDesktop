//! Shell 修改日志：在任何桌面状态修改之前落盘，记录原值、应用值、所有者会话与恢复进度。
//!
//! 设计约束：
//! - 先写日志再修改；写入采用同目录临时文件 + 原子替换，避免半写文件。
//! - 同一键重复记录时保留首次捕获的原值，防止把已修改后的值误当作原值。
//! - 损坏的日志不会静默丢弃：重命名为 `.corrupt-<时间>` 保留证据，然后从空日志继续。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RecoveryState {
    /// 日志已写入，修改尚未确认。
    Pending,
    /// 修改已应用，等待恢复。
    Applied,
    /// 已恢复到原值。
    Recovered,
    /// 恢复失败，保留原因供下次启动重试。
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellStateEntry {
    pub key: String,
    pub original: String,
    pub applied: String,
    pub owner_session: u32,
    pub recovery: RecoveryState,
    pub updated_unix_seconds: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalFile {
    schema_version: u32,
    entries: Vec<ShellStateEntry>,
}

impl Default for JournalFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

pub struct ShellStateJournal {
    path: PathBuf,
    entries: Vec<ShellStateEntry>,
}

impl ShellStateJournal {
    /// `%APPDATA%\DeskShell\shell-state.json`；Shell 状态属于用户状态而不是缓存。
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("DeskShell").join("shell-state.json"))
    }

    /// 打开或创建日志。不存在的文件视为空日志；损坏文件先改名保留再继续。
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let mut journal = Self {
            path,
            entries: Vec::new(),
        };
        if !journal.path.is_file() {
            return Ok(journal);
        }
        let bytes = fs::read(&journal.path).map_err(|error| {
            format!(
                "读取 Shell 状态日志 {} 失败：{error}",
                journal.path.display()
            )
        })?;
        match serde_json::from_slice::<JournalFile>(&bytes) {
            Ok(file) => {
                if file.schema_version > SCHEMA_VERSION {
                    return Err(format!(
                        "Shell 状态日志版本 {} 高于支持的 {SCHEMA_VERSION}，拒绝覆盖",
                        file.schema_version
                    ));
                }
                journal.entries = file.entries;
            }
            Err(error) => {
                let backup = journal.backup_corrupt()?;
                eprintln!(
                    "Shell 状态日志损坏（{error}），已保留为 {} 并从空日志继续",
                    backup.display()
                );
            }
        }
        Ok(journal)
    }

    fn backup_corrupt(&self) -> Result<PathBuf, String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let backup = self.path.with_extension(format!("corrupt-{nanos}"));
        fs::rename(&self.path, &backup)
            .map_err(|error| format!("重命名损坏日志 {} 失败：{error}", self.path.display()))?;
        Ok(backup)
    }

    /// 修改前调用：记录原值与应用值，并将恢复状态置为 `Pending`。
    /// 已存在同键记录时保留首次原值与所有者会话。
    pub fn begin(&mut self, key: &str, original: String, applied: String) -> Result<(), String> {
        let now = now_seconds();
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) {
            entry.applied = applied;
            entry.recovery = RecoveryState::Pending;
            entry.updated_unix_seconds = now;
        } else {
            self.entries.push(ShellStateEntry {
                key: key.to_owned(),
                original,
                applied,
                owner_session: shell_win::desktop::current_session_id().unwrap_or(0),
                recovery: RecoveryState::Pending,
                updated_unix_seconds: now,
            });
        }
        self.save()
    }

    pub fn mark_applied(&mut self, key: &str) -> Result<(), String> {
        self.update(key, |entry| entry.recovery = RecoveryState::Applied)
    }

    pub fn mark_recovered(&mut self, key: &str) -> Result<(), String> {
        self.update(key, |entry| entry.recovery = RecoveryState::Recovered)
    }

    pub fn mark_failed(&mut self, key: &str, reason: impl Into<String>) -> Result<(), String> {
        let reason = reason.into();
        self.update(key, |entry| {
            entry.recovery = RecoveryState::Failed {
                reason: reason.clone(),
            }
        })
    }

    fn update(
        &mut self,
        key: &str,
        apply: impl FnOnce(&mut ShellStateEntry),
    ) -> Result<(), String> {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.key == key) else {
            return Err(format!("Shell 状态日志中没有键 {key}"));
        };
        apply(entry);
        entry.updated_unix_seconds = now_seconds();
        self.save()
    }

    pub fn get(&self, key: &str) -> Option<&ShellStateEntry> {
        self.entries.iter().find(|entry| entry.key == key)
    }

    /// 需要恢复的条目：已应用但尚未恢复（含上次恢复失败）。
    pub fn pending_recovery(&self) -> impl Iterator<Item = &ShellStateEntry> {
        self.entries.iter().filter(|entry| {
            matches!(
                entry.recovery,
                RecoveryState::Applied | RecoveryState::Failed { .. } | RecoveryState::Pending
            )
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn save(&self) -> Result<(), String> {
        let file = JournalFile {
            schema_version: SCHEMA_VERSION,
            entries: self.entries.clone(),
        };
        let payload = serde_json::to_vec_pretty(&file)
            .map_err(|error| format!("序列化 Shell 状态日志失败：{error}"))?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("创建日志目录 {} 失败：{error}", parent.display()))?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, &payload)
            .map_err(|error| format!("写入临时日志 {} 失败：{error}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .map_err(|error| format!("原子替换日志 {} 失败：{error}", self.path.display()))
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "deskshell-journal-{tag}-{}-{}.json",
            std::process::id(),
            now_seconds()
        ))
    }

    #[test]
    fn journal_persists_before_modification_and_is_idempotent() {
        let path = temporary_path("basic");
        let _ = fs::remove_file(&path);
        {
            let mut journal = ShellStateJournal::open(&path).unwrap();
            journal
                .begin("desktop-icons", "true".into(), "false".into())
                .unwrap();
            // begin 必须先落盘：重新打开可见 Pending 记录。
            let reopened = ShellStateJournal::open(&path).unwrap();
            let entry = reopened.get("desktop-icons").unwrap();
            assert_eq!(entry.original, "true");
            assert_eq!(entry.applied, "false");
            assert_eq!(entry.recovery, RecoveryState::Pending);
            assert_eq!(reopened.pending_recovery().count(), 1);
        }
        {
            let mut journal = ShellStateJournal::open(&path).unwrap();
            journal.mark_applied("desktop-icons").unwrap();
            journal.mark_recovered("desktop-icons").unwrap();
            journal.mark_recovered("desktop-icons").unwrap();
            let reopened = ShellStateJournal::open(&path).unwrap();
            assert_eq!(
                reopened.get("desktop-icons").unwrap().recovery,
                RecoveryState::Recovered
            );
            assert_eq!(reopened.pending_recovery().count(), 0);
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn repeated_begin_keeps_original_value() {
        let path = temporary_path("repeat");
        let _ = fs::remove_file(&path);
        let mut journal = ShellStateJournal::open(&path).unwrap();
        journal
            .begin("wallpaper-mode", "system".into(), "live".into())
            .unwrap();
        journal
            .begin("wallpaper-mode", "live".into(), "paused".into())
            .unwrap();
        let entry = journal.get("wallpaper-mode").unwrap();
        assert_eq!(entry.original, "system");
        assert_eq!(entry.applied, "paused");
        assert_eq!(entry.recovery, RecoveryState::Pending);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn corrupt_journal_is_preserved_and_reset() {
        let path = temporary_path("corrupt");
        fs::write(&path, b"{not-json").unwrap();
        let journal = ShellStateJournal::open(&path).unwrap();
        assert_eq!(journal.pending_recovery().count(), 0);
        let prefix = format!("deskshell-journal-corrupt-{}-", std::process::id());
        let backup = fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix))
            });
        let backup = backup.expect("损坏日志应被重命名保留");
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("corrupt-")
        );
        let _ = fs::remove_file(&backup);
        let _ = fs::remove_file(&path);
    }
}
