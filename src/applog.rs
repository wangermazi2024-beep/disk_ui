//! 统一日志。以前是手搓的"eprintln! + 写文件"，现在过一遍 `log` crate——
//! Rust 生态里用得最多的日志门面（facade，crates.io 下载量长期排在最前面，
//! 常年是被依赖次数最多的库之一，几乎所有会打日志的库链的都是这一个，不是
//! 各自发明一套）。接上它的好处不只是"跟主流"：以后项目里任何地方——包括
//! 依赖的第三方库内部——只要调用标准的 `log::info!`/`log::warn!`/`log::error!`
//! 这些宏，都会自动被下面这个 `DualLogger` 捕获、写到同一份文件里、用同一套
//! 格式（时间戳等），不用每个模块自己再攒一遍"要不要也写文件"这种逻辑。
//!
//! 双写：GUI 主程序双击运行时没有控制台，`eprintln!` 直接就看不到了，所以
//! 同时写文件；日志文件固定放在 exe 所在目录下的 `diskforge_log.txt`，正常
//! 情况下每次启动追加（方便跨多次运行做前后对比），但超过 5MB 会重新开始，
//! 避免无限增长。
//!
//! 每行都带时间戳（`HH:MM:SS.毫秒`，本地时间）——以前没有时间戳，出问题只能
//! 靠日志的先后顺序猜"这大概是几秒前的事"；现在有了后台线程算重复文件哈希
//! 这种真正耗时的操作之后，时间戳基本是排查"卡在哪一步"的必需品。
//!
//! `applog::log()`/`dlog!` 宏这两个项目里原来就在用、散落在很多文件里的调用
//! 方式完全不变——内部直接写文件（`write_line`），不经过 `log` 门面那一层，
//! 这样即使哪天 `log::set_boxed_logger` 因为某种原因没装成功，我们自己的日志
//! 调用也不会跟着默默失效。`log` 门面单独接一个 `DualLogger` 实例，专门用来
//! 接住"标准 `log::xxx!` 宏"这条路的调用，两条路最终写的是同一个文件、同一套
//! 时间戳格式，只是级别前缀会不一样（走 `log::xxx!` 的会带 `[INFO]`/`[WARN]`
//! 这些级别标记，我们自己的 `applog::log()` 不带，行为和以前完全一致）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use log::{LevelFilter, Log, Metadata, Record};

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

/// 接给 `log` crate 门面用的实现，只负责把标准 `log::xxx!` 宏的调用转发到
/// 和 `applog::log()` 一样的落地函数（`write_line`），格式统一。
struct DualLogger;

impl Log for DualLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true // 级别过滤交给 log::set_max_level 在源头挡掉，这里不用重复判断
    }
    fn log(&self, record: &Record) {
        write_line(&format!("[{}] {}", record.level(), record.args()));
    }
    fn flush(&self) {
        if let Some(lock) = LOG_FILE.get() {
            if let Ok(mut guard) = lock.lock() {
                if let Some(f) = guard.as_mut() {
                    let _ = f.flush();
                }
            }
        }
    }
}

fn timestamp() -> String {
    chrono::Local::now().format("%H:%M:%S%.3f").to_string()
}

/// 真正落地的地方：console + 文件，每行都带时间戳前缀。
fn write_line(msg: &str) {
    let line = format!("[{}] {msg}", timestamp());
    eprintln!("{line}");
    if let Some(lock) = LOG_FILE.get() {
        if let Ok(mut guard) = lock.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush(); // 每行都 flush，方便运行中途直接打开文件查看，不用等程序退出
            }
        }
    }
}

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
    // 无限增长下去（尤其是现在重复文件比对这种一次能产生成千上万行日志的操作
    // 也会写进来）。超过 5MB 就重新开一个空文件，而不是无限 append。
    const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
    let truncate = std::fs::metadata(&path).map(|m| m.len() >= MAX_LOG_BYTES).unwrap_or(false);
    let file = OpenOptions::new().create(true).append(!truncate).write(truncate).truncate(truncate).open(&path).ok();
    match &file {
        Some(_) => eprintln!("[applog] 诊断日志会写到: {}{}", path.display(), if truncate { "（已超过 5MB，重新开始）" } else { "" }),
        None => eprintln!("[applog] 打开日志文件失败（仅控制台可见诊断信息）: {}", path.display()),
    }
    let _ = LOG_FILE.set(Mutex::new(file));

    // 接上 log crate 门面。只应该在整个进程里调用一次，失败（比如别的什么代码
    // 抢先装了别的 logger，这里几乎不会发生）也不影响 applog::log()/dlog!
    // 正常工作——见模块顶部注释。
    //
    // 级别定成 Info、不是 Trace：eframe/winit 自己内部也是用 `log` crate
    // 打诊断信息的（`event_result: Ok(Wait)`、`request_redraw for WindowId(...)`
    // 这类每一帧都会打一堆的框架内部调试信息，级别是 Trace/Debug）。之前设成
    // Trace 相当于把这些框架内部噪音也全部转发进了我们自己的日志文件，
    // 一堆无用信息把真正有用的日志淹没掉，还平白多了很多次磁盘写入。
    // Info 只放行"真正想让人看见"的日志（我们自己的 `applog::log()`/`dlog!`
    // 走的是 write_line 直接落地，不受这个过滤影响，不用担心被误伤）。
    if log::set_boxed_logger(Box::new(DualLogger)).is_ok() {
        log::set_max_level(LevelFilter::Info);
    } else {
        eprintln!("[applog] log::set_boxed_logger 失败（可能已经被别的代码设置过），标准 log::xxx! 宏不会写进日志文件，但 applog::log()/dlog! 不受影响。");
    }

    log(&format!(
        "==== DiskForge WMS 启动 (unix_ts={}) ====",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    ));
}

/// 写一行日志：`eprintln!` 到控制台（有控制台时可见）+ 追加到日志文件，带时间戳。
/// 一般不直接调用，用下面的 `dlog!` 宏。
pub fn log(msg: &str) {
    write_line(msg);
}

/// 批量写日志：给"一次要写很多行"的场景用。只在最后统一 flush 一次——`log()`/
/// `dlog!` 为了"程序中途崩了也能看到已经写的日志"特意每行都 flush，但这个代价
/// 在"一次写几千行"的场景下会变成新的瓶颈本身（每次 flush 都是一次系统调用）。
///
/// 目前项目里没有地方在用（重复文件比对那边原来用它一条条记录每组的哈希/路径，
/// 后来发现这一步本身就是"进度条走到 100% 却还卡住不动"的元凶，已经去掉了，
/// 见 `categorize.rs` 里的说明）——先留着这个函数，以后如果有别的地方需要
/// 一次性写大量日志，不用重新造轮子。
#[allow(dead_code)]
pub fn log_batch(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let ts = timestamp();
    if let Some(lock) = LOG_FILE.get() {
        if let Ok(mut guard) = lock.lock() {
            if let Some(f) = guard.as_mut() {
                for line in lines {
                    let _ = writeln!(f, "[{ts}] {line}");
                }
                let _ = f.flush();
            }
        }
    }
    // 控制台不重复整批打印——几千行糊在终端里没人看得过来，只提示一行，
    // 想看细节直接打开日志文件（`log_path()`）。
    eprintln!("[{ts}] (批量写入 {} 行到日志文件，控制台不逐行显示)", lines.len());
}

/// 用法和 `eprintln!` 完全一样：`dlog!("...{}...", x)`。
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        $crate::applog::log(&format!($($arg)*))
    };
}
