//! DiskLens MFT 验证工具：在 Windows 上同时跑 MFT 直读 + 标准遍历 + 系统报告，
//! 三方对比打印报告，肉眼看 MFT 路径有没有丢文件 / 大小有没有偏差。
//!
//! ## 使用方法
//!
//! 1. 编译：`cargo build --release --bin verify_mft`
//! 2. **右键 → 以管理员身份运行** 一个 cmd / PowerShell（直读 $MFT 强制要求管理员权限）
//! 3. 在管理员控制台里执行：
//!     `verify_mft.exe C:`
//!    或者直接运行（默认验证 C 盘）：
//!     `verify_mft.exe`
//!
//! ## 输出
//!
//! 会依次打印：
//! - 系统报告（GetDiskFreeSpaceExW）：总容量 / 已用 / 可用
//! - MFT 直读统计：总记录数 / 有效记录数 / 文件数 / 文件夹数 / 大小汇总
//! - 标准遍历统计：文件数 / 文件夹数 / 大小汇总
//! - 三方对比表 + 差异说明
//!
//! 注意：MFT 直读的"大小汇总"是所有文件 $DATA 属性的 logical size 之和，
//! 不含 NTFS 元数据 / 簇内部碎片 / 卷影副本等；而系统报告的"已用空间"含这些，
//! 所以两者天然会有 5%~20% 的差距，这是预期内的，**不代表丢文件**。
//! 真正应该一致的是「MFT 文件数 / 文件夹数」vs「标准遍历文件数 / 文件夹数」
//! （标准遍历可能因为权限拒绝而少一些，MFT 是全量记录）。

// 非 Windows 平台给一个空的 main，让这个 bin 也能编过（Linux 上跑只会打印一行提示）。
#[cfg(not(windows))]
fn main() {
    eprintln!("verify_mft only runs on Windows. This is a non-Windows build.");
    eprintln!("请在 Windows 上以管理员身份运行：verify_mft.exe [drive_letter]");
    std::process::exit(1);
}

// 这些 import 在两个平台上都成立（scan / mpsc / PathBuf / ScanMessage 都是跨平台的），
// 不要给它们加 cfg(windows)，否则 `run_standard_only` 在 Linux 上会找不到符号。
use std::env;
use std::path::PathBuf;
use std::sync::mpsc;

// mft_scan 模块本身是 cfg(windows) 的，所以这个 import 也要 cfg(windows)。
#[cfg(windows)]
use disk_ui::mft_scan;
use disk_ui::scan::{self, ScanMessage};

#[cfg(windows)]
fn main() {
    eprintln!("=== DiskLens MFT 验证工具 ===");
    eprintln!();

    // 解析参数：第一个参数是盘符或路径，默认 C:
    let arg = env::args().nth(1).unwrap_or_else(|| "C:".into());
    let drive_letter = arg
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
        .expect("无法解析盘符");
    let root_path = format!("{}:\\", drive_letter);
    eprintln!("[verify] 验证目标: {} (drive_letter={})", root_path, drive_letter);
    eprintln!();

    // ── 1. 系统报告 ────────────────────────────────────────────
    eprintln!("─── 1. 系统报告 (GetDiskFreeSpaceExW) ───");
    let (total, free) = match mft_scan::get_disk_space(drive_letter) {
        Some((t, f)) => (t, f),
        None => {
            eprintln!("[verify] GetDiskFreeSpaceExW 失败");
            std::process::exit(1);
        }
    };
    let used = total.saturating_sub(free);
    eprintln!(
        "[verify] 总容量: {:.2} GB ({})",
        total as f64 / 1e9,
        total
    );
    eprintln!(
        "[verify] 已用:   {:.2} GB ({})",
        used as f64 / 1e9,
        used
    );
    eprintln!(
        "[verify] 可用:   {:.2} GB ({})",
        free as f64 / 1e9,
        free
    );
    eprintln!();

    // 检查管理员权限
    if !mft_scan::is_elevated() {
        eprintln!("[verify] ⚠ 当前进程非管理员！直读 $MFT 会失败。");
        eprintln!("[verify] 请右键 → 以管理员身份运行 → 重新执行 verify_mft.exe");
        eprintln!();
        eprintln!("[verify] 跳过 MFT 直读验证，仅跑标准遍历。");
        run_standard_only(&root_path, drive_letter, total, used);
        return;
    }

    // ── 2. MFT 直读统计 ────────────────────────────────────────
    eprintln!("─── 2. MFT 直读统计 ($MFT) ───");
    eprintln!("[verify] 开始 MFT 直读，可能需要 5~30 秒...");
    let (tx, rx) = mpsc::channel();
    let drive_letter_for_thread = drive_letter;
    let mft_handle = std::thread::spawn(move || {
        mft_scan::scan_drive_via_mft(drive_letter_for_thread, &tx)
    });

    // 收进度
    let mut last_progress = 0u64;
    while let Ok(msg) = rx.recv() {
        match msg {
            ScanMessage::Progress(n) => {
                if n / 50_000 > last_progress / 50_000 {
                    eprintln!("[verify] MFT 进度: 已解析 {} 条记录", n);
                    last_progress = n;
                }
            }
            ScanMessage::Done(_, _) => break,
            ScanMessage::Error(e) => {
                eprintln!("[verify] MFT 扫描错误: {}", e);
                break;
            }
        }
    }

    let mft_result = mft_handle.join().expect("MFT thread panicked");
    let mft_root = match mft_result {
        Ok(r) => r.root,
        Err(e) => {
            eprintln!("[verify] MFT 直读失败: {}", e);
            eprintln!();
            eprintln!("[verify] 仅跑标准遍历作为对照。");
            run_standard_only(&root_path, drive_letter, total, used);
            return;
        }
    };

    eprintln!(
        "[verify] MFT 文件数:   {}",
        mft_root.file_count
    );
    eprintln!(
        "[verify] MFT 文件夹数: {}",
        mft_root.folder_count
    );
    eprintln!(
        "[verify] MFT 大小汇总: {:.2} GB ({})",
        mft_root.size as f64 / 1e9,
        mft_root.size
    );
    eprintln!();

    // ── 3. 标准遍历统计 ────────────────────────────────────────
    eprintln!("─── 3. 标准遍历统计 (std::fs::read_dir) ───");
    eprintln!("[verify] 开始标准遍历，可能需要 1~5 分钟...");
    let (tx2, rx2) = mpsc::channel();
    let root_for_scan = PathBuf::from(&root_path);
    scan::spawn_scan(root_for_scan, tx2);

    let std_root = loop {
        match rx2.recv() {
            Ok(ScanMessage::Progress(n)) => {
                if n % 5000 == 0 {
                    eprintln!("[verify] 标准遍历进度: 已发现 {} 项", n);
                }
            }
            Ok(ScanMessage::Done(node, _)) => break *node,
            Ok(ScanMessage::Error(e)) => {
                eprintln!("[verify] 标准遍历失败: {}", e);
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("[verify] 通道错误: {}", e);
                std::process::exit(1);
            }
        }
    };

    eprintln!(
        "[verify] 标准遍历 文件数:   {}",
        std_root.file_count
    );
    eprintln!(
        "[verify] 标准遍历 文件夹数: {}",
        std_root.folder_count
    );
    eprintln!(
        "[verify] 标准遍历 大小汇总: {:.2} GB ({})",
        std_root.size as f64 / 1e9,
        std_root.size
    );
    eprintln!();

    // ── 4. 三方对比 ────────────────────────────────────────────
    eprintln!("─── 4. 三方对比报告 ───");
    eprintln!();
    eprintln!("┌──────────────┬────────────────┬────────────────┬────────────────┐");
    eprintln!("│ 指标         │ 系统报告       │ MFT 直读       │ 标准遍历       │");
    eprintln!("├──────────────┼────────────────┼────────────────┼────────────────┤");
    eprintln!(
        "│ 文件数       │ {:>14} │ {:>14} │ {:>14} │",
        "—",
        mft_root.file_count,
        std_root.file_count
    );
    eprintln!(
        "│ 文件夹数     │ {:>14} │ {:>14} │ {:>14} │",
        "—",
        mft_root.folder_count,
        std_root.folder_count
    );
    eprintln!(
        "│ 大小 (字节)  │ {:>14} │ {:>14} │ {:>14} │",
        used,
        mft_root.size,
        std_root.size
    );
    eprintln!(
        "│ 大小 (GB)    │ {:>14.2} │ {:>14.2} │ {:>14.2} │",
        used as f64 / 1e9,
        mft_root.size as f64 / 1e9,
        std_root.size as f64 / 1e9
    );
    eprintln!("└──────────────┴────────────────┴────────────────┴────────────────┘");
    eprintln!();

    // 差异分析
    let file_diff = mft_root.file_count as i64 - std_root.file_count as i64;
    let folder_diff = mft_root.folder_count as i64 - std_root.folder_count as i64;
    let size_diff_pct = if std_root.size > 0 {
        (mft_root.size as f64 - std_root.size as f64) / std_root.size as f64 * 100.0
    } else {
        0.0
    };

    eprintln!("[verify] 差异分析:");
    eprintln!(
        "  文件数差 (MFT - 标准遍历)   = {} {}",
        file_diff,
        if file_diff > 0 { "(MFT 多看到一些 — 正常，标准遍历被权限挡住)" }
            else if file_diff < 0 { "⚠ 标准遍历多看到一些 — 不正常，请检查！" }
            else { "(完全一致)"
        }
    );
    eprintln!(
        "  文件夹数差 (MFT - 标准遍历) = {} {}",
        folder_diff,
        if folder_diff >= 0 { "(MFT >= 标准遍历，正常)" }
            else { "⚠ 标准遍历多看到一些 — 不正常！"
        }
    );
    eprintln!(
        "  大小差 (MFT vs 标准遍历)    = {:+.2}%",
        size_diff_pct
    );
    eprintln!(
        "  大小差 (系统已用 vs MFT)    = {:+.2}%  (预期 5%~25%，含簇碎片/元数据/卷影)",
        (used as f64 - mft_root.size as f64) / mft_root.size.max(1) as f64 * 100.0
    );
    eprintln!();

    // 判定
    if file_diff >= 0 && folder_diff >= 0 && size_diff_pct.abs() < 10.0 {
        eprintln!("✅ 验证通过：MFT 直读没有丢文件，大小在合理误差范围内。");
    } else if file_diff < 0 || folder_diff < 0 {
        eprintln!("❌ 验证失败：MFT 直读比标准遍历少看到文件/文件夹，可能有 bug！");
        eprintln!("   请把上面的报告发给开发者。");
    } else {
        eprintln!("⚠ 大小差异较大（>10%），但文件数一致。可能是 ADS / 稀疏文件较多，属正常。");
    }
}

#[cfg(windows)]
fn run_standard_only(root_path: &str, drive_letter: char, total: u64, used: u64) {
    eprintln!("─── 标准遍历统计 (std::fs::read_dir) ───");
    eprintln!("[verify] 开始标准遍历 {}，可能需要 1~5 分钟...", root_path);
    let (tx, rx) = mpsc::channel();
    scan::spawn_scan(PathBuf::from(root_path), tx);

    let std_root = loop {
        match rx.recv() {
            Ok(ScanMessage::Progress(n)) => {
                if n % 5000 == 0 {
                    eprintln!("[verify] 标准遍历进度: 已发现 {} 项", n);
                }
            }
            Ok(ScanMessage::Done(node, _)) => break *node,
            Ok(ScanMessage::Error(e)) => {
                eprintln!("[verify] 标准遍历失败: {}", e);
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("[verify] 通道错误: {}", e);
                std::process::exit(1);
            }
        }
    };

    eprintln!("[verify] 标准遍历 文件数:   {}", std_root.file_count);
    eprintln!("[verify] 标准遍历 文件夹数: {}", std_root.folder_count);
    eprintln!(
        "[verify] 标准遍历 大小汇总: {:.2} GB ({})",
        std_root.size as f64 / 1e9,
        std_root.size
    );
    eprintln!();
    eprintln!("─── 对比（无 MFT 数据） ───");
    eprintln!(
        "  系统已用: {:.2} GB, 标准遍历汇总: {:.2} GB, 差: {:+.2}%",
        used as f64 / 1e9,
        std_root.size as f64 / 1e9,
        (used as f64 - std_root.size as f64) / std_root.size.max(1) as f64 * 100.0
    );
    eprintln!();
    eprintln!("提示：要跑 MFT 直读对比，请右键 → 以管理员身份运行 verify_mft.exe");

    let _ = (drive_letter, total);
}
