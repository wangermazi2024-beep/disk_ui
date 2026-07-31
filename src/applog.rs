//! 双写日志：GUI 主程序双击运行时没有控制台，`eprintln!` 直接就看不到了。
//! `dlog!` 宏在保留 `eprintln!` 行为的同时，把同样的内容追加写进一个日志文件，
//! 这样不方便编译 `verify_mft.exe` 单独调试的时候，也能从这个文件里拿到诊断信息。
//!
//! 日志文件固定放在 exe 所在目录下的 `disklens_log.txt`，每次启动追加（不清空），
//! 方便跨多次运行做前后对比。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// 日志文件的完整路径，供 UI 层展示给用户（比如导出成功后提示"日志见 xxx"）。
///
/// 优先顺序：
/// 1. exe 所在目录下的 `disklens_log.txt`（方便跟 exe 一起贴走调试）
/// 2. %TEMP%\disklens_log.txt（双击运行时 cwd 可能不可写，退化到临时目录）
/// 以前只退化到 `PathBuf::from("disklens_log.txt")`——cwd 不一致导致找不到日志。
pub fn log_path() -> PathBuf {
    // 1. exe 所在目录
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("disklens_log.txt")))
    {
        return p;
    }
    // 2. %TEMP% （Windows） / /tmp（Unix）
    #[cfg(windows)]
    {
        if let Some(tmp) = std::env::var_os("TEMP").or_else(|| std::env::var_os("TMP")) {
            return PathBuf::from(tmp).join("disklens_log.txt");
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            return PathBuf::from(tmp).join("disklens_log.txt");
        }
    }
    // 3. 最后兑底：cwd（至少不会崩）
    PathBuf::from("disklens_log.txt")
}

/// 程序启动时调用一次。打开日志文件失败也不影响程序正常运行，只是退化成只有
/// `eprintln!`（比如日志文件所在目录只读的极端情况）。
pub fn init() {
    let path = log_path();
    let file = OpenOptions::new().create(true).append(true).open(&path).ok();
    match &file {
        Some(_) => eprintln!("[applog] 诊断日志会写到: {}", path.display()),
        None => eprintln!("[applog] 打开日志文件失败（仅控制台可见诊断信息）: {}", path.display()),
    }
    let _ = LOG_FILE.set(Mutex::new(file));
    log(&format!(
        "==== DiskLens 启动 (unix_ts={}) ====",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    ));
}

/// 写一行日志：`eprintln!` 到控制台（有控制台时可见）+ 追加到日志文件。
/// 一般不直接调用，用下面的 `dlog!` 宏。
pub fn log(msg: &str) {
    eprintln!("{msg}");
    if let Some(lock) = LOG_FILE.get() {
        if let Ok(mut guard) = lock.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{msg}");
                let _ = f.flush(); // 每行都 flush，方便扫描中途查看，不等程序退出。
            }
        }
    }
}

/// 用法和 `eprintln!` 完全一样：`dlog!("...{}...", x)`。
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        $crate::applog::log(&format!($($arg)*))
    };
}
