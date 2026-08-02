//! 双写日志：GUI 主程序双击运行时没有控制台，`eprintln!` 直接就看不到了。
//! `dlog!` 宏在保留 `eprintln!` 行为的同时，把同样的内容追加写进一个日志文件，
//! 这样不方便编译 `verify_mft.exe` 单独调试的时候，也能从这个文件里拿到诊断信息。
//!
//! 日志文件固定放在 exe 所在目录下的 `diskforge_log.txt`，正常情况下每次启动追加
//! （方便跨多次运行做前后对比），但超过 5MB 会重新开始，避免无限增长。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// 日志文件的完整路径，供 UI 层展示给用户（比如导出成功后提示"日志见 xxx"）。
pub fn log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("diskforge_log.txt")))
        .unwrap_or_else(|| {
            // current_exe() 失败是极端情况，但原来这里退到"当前工作目录"，
            // 而 cwd 会随启动方式变化（双击 vs 快捷方式的"起始位置" vs 命令行 cd 到哪），
            // 同一个程序不同启动方式日志文件会出现在不同地方，不好找。
            // 退到系统临时目录是一个固定、几乎总是可写的位置，行为更一致。
            std::env::temp_dir().join("diskforge_log.txt")
        })
}

/// 程序启动时调用一次。打开日志文件失败也不影响程序正常运行，只是退化成只有
/// `eprintln!`（比如日志文件所在目录只读的极端情况）。
pub fn init() {
    let path = log_path();
    // 简单的日志轮转：不这样做的话，`diskforge_log.txt` 会随着每次启动、每次扫描
    // 无限增长下去（尤其是现在 scan.rs/mft_scan.rs 里的诊断信息也都会写进来）。
    // 超过 5MB 就重新开一个空文件，而不是无限 append。5MB 纯文本日志已经够看
    // 好几十次启动+扫描的历史了，没必要留着更早的。
    const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
    let truncate = std::fs::metadata(&path).map(|m| m.len() >= MAX_LOG_BYTES).unwrap_or(false);
    let file = OpenOptions::new().create(true).append(!truncate).write(truncate).truncate(truncate).open(&path).ok();
    match &file {
        Some(_) => eprintln!("[applog] 诊断日志会写到: {}{}", path.display(), if truncate { "（已超过 5MB，重新开始）" } else { "" }),
        None => eprintln!("[applog] 打开日志文件失败（仅控制台可见诊断信息）: {}", path.display()),
    }
    let _ = LOG_FILE.set(Mutex::new(file));
    log(&format!(
        "==== DiskForge WMS 启动 (unix_ts={}) ====",
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
