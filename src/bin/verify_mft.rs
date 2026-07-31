//! 验证工具：对比 MFT 直读 vs 标准遍历。

#[cfg(not(windows))]
fn main() {
    eprintln!("verify_mft 只能在 Windows 上运行");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use disk_ui::mft_scan;
    use disk_ui::scan::{self, ScanMessage};

    let arg = std::env::args().nth(1).unwrap_or_else(|| "C:".into());
    let drive = arg.chars().next().filter(|c|c.is_ascii_alphabetic()).map(|c|c.to_ascii_uppercase()).unwrap();
    eprintln!("=== 验证 {} ===", drive);

    let (total, free) = mft_scan::get_disk_space(drive).expect("get_disk_space 失败");
    let used = total - free;
    eprintln!("系统报告: 总={:.2}GB 已用={:.2}GB 可用={:.2}GB", total as f64/1e9, used as f64/1e9, free as f64/1e9);

    if !mft_scan::is_elevated() {
        eprintln!("⚠ 非管理员！请右键以管理员身份运行");
        return;
    }

    // MFT 直读
    eprintln!("\n--- MFT 直读 ---");
    let (tx, rx) = mpsc::channel();
    let d = drive;
    let h = std::thread::spawn(move || mft_scan::scan_volume(d, &tx));
    while let Ok(msg) = rx.recv() {
        if let ScanMessage::Progress(n) = msg { if n % 50000 == 0 { eprintln!("  进度: {}", n); } }
    }
    let mft_root = h.join().unwrap().expect("MFT 失败");
    eprintln!("MFT: files={}, folders={}, logical={:.2}GB, physical={:.2}GB",
        mft_root.file_count, mft_root.folder_count,
        mft_root.logical_size as f64/1e9, mft_root.physical_size as f64/1e9);

    // 标准遍历
    eprintln!("\n--- 标准遍历 ---");
    let (tx2, rx2) = mpsc::channel();
    scan::spawn_scan(PathBuf::from(format!("{}:\\", drive)), tx2);
    let std_root = loop {
        match rx2.recv() {
            Ok(ScanMessage::Done(n, _)) => break *n,
            Ok(ScanMessage::Error(e)) => { eprintln!("标准遍历失败: {}", e); return; }
            _ => {}
        }
    };
    eprintln!("标准: files={}, folders={}, logical={:.2}GB",
        std_root.file_count, std_root.folder_count, std_root.logical_size as f64/1e9);

    // 对比
    eprintln!("\n--- 对比 ---");
    eprintln!("文件数差(MFT-标准): {}", mft_root.file_count as i64 - std_root.file_count as i64);
    eprintln!("大小差(MFT vs 系统): {:+.2}%", (mft_root.logical_size as f64 - used as f64) / used as f64 * 100.0);
}
