//! 磁盘分区枚举与信息查询。
//!
//! 在 Windows 上通过 `GetLogicalDriveStringsW` 枚举所有盘符，再对每个盘符调用
//! `GetVolumeInformationW`（拿卷标 / 文件系统）和 `GetDiskFreeSpaceExW`
//!（拿总容量 / 可用空间），得到一份完整的 `DiskInfo` 列表。
//!
//! 调用方（`app.rs`）从这里挑出 C 盘作为默认显示分区——这样就不需要把 `C:\`
//! 这种字符串硬编码在 UI 里，以后要多盘支持只需要把列表里其他盘也展示出来即可。
//!
//! 非 Windows 平台整体被 `cfg` 掉，返回空列表 / None，保证代码能编译过
//!（本机 Linux 上无法真正测试，但语法/类型层面一定要能编通）。

/// 单个磁盘分区的元信息。
///
/// 所有字段都用 `u64` / `String`，方便直接 clone 不需要担心生命周期。
#[derive(Clone, Debug, Default)]
pub struct DiskInfo {
    /// 盘符，大写字母，例如 `C`。
    pub drive_letter: char,
    /// 卷标，例如 `本地磁盘C`；如果该分区没设卷标，这里就是空串。
    pub volume_label: String,
    /// 文件系统类型，例如 `NTFS` / `FAT32` / `exFAT`。
    pub file_system: String,
    /// 分区总容量（字节）。
    pub total_bytes: u64,
    /// 可用空间（字节）= 未分配。
    pub free_bytes: u64,
    /// 已用空间（字节）= 已分配 = `total - free`。
    pub used_bytes: u64,
}

impl DiskInfo {
    /// 给 UI 用的展示名，和 Windows 资源管理器保持一致：
    /// - 有卷标时：`卷标 (X:)`，例如 `新加卷 (E:)`
    /// - 无卷标时：`本地磁盘 (X:)`，例如 `本地磁盘 (C:)`
    ///
    /// 注意括号格式：盘符必须包在括号里，否则像 `本地磁盘 C:` 这种写法
    /// 在 UI 里容易被误读成"本地磁盘" + "C:" 两段。
    pub fn display_name(&self) -> String {
        if self.volume_label.is_empty() {
            format!("本地磁盘 ({}:)", self.drive_letter)
        } else {
            format!("{} ({}:)", self.volume_label, self.drive_letter)
        }
    }

    /// 根路径字符串，例如 `C:\`，用于扫描入口和 CreateFileW 之类的 API。
    pub fn root_path(&self) -> String {
        format!("{}:\\", self.drive_letter)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Windows 实现
// ─────────────────────────────────────────────────────────────────────────
#[cfg(windows)]
pub fn enumerate_drives() -> Vec<DiskInfo> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDriveStringsW};

    // GetDriveTypeW 返回值。windows-sys 0.59 把它们放在
    // Win32::System::WindowsProgramming 下，需要单独开 feature，
    // 这里直接定义成常量避免改 Cargo.toml。
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;

    let mut result = Vec::new();

    // GetLogicalDriveStringsW 返回形如 "C:\\\0D:\\\0\0" 的双 null 结尾字符串数组。
    // 多留一些缓冲区，避免盘符多时被截断。
    let mut buffer = [0u16; 520];
    let len = unsafe { GetLogicalDriveStringsW(buffer.len() as u32, buffer.as_mut_ptr()) };
    if len == 0 {
        eprintln!(
            "[disk_info] GetLogicalDriveStringsW 失败: GetLastError={}",
            unsafe { GetLastError() }
        );
        return result;
    }
    if len as usize >= buffer.len() {
        eprintln!("[disk_info] 盘符字符串缓冲区不够大，需要 {}", len);
        return result;
    }

    // 按双 null 结尾解析每一条 "X:\"
    let mut start = 0usize;
    while start < len as usize {
        let end = buffer[start..len as usize]
            .iter()
            .position(|&c| c == 0)
            .map(|p| start + p)
            .unwrap_or(len as usize);
        if start >= end {
            break;
        }
        let drive_str = String::from_utf16_lossy(&buffer[start..end]);
        let drive_letter = drive_str.chars().next().unwrap_or('?');
        let drive_letter_up = drive_letter.to_ascii_uppercase();

        // 用宽字符形式再要一份带 null 结尾的，给 GetDriveTypeW 用
        let wide: Vec<u16> = drive_str.encode_utf16().chain(std::iter::once(0)).collect();
        let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
        if drive_type != DRIVE_FIXED && drive_type != DRIVE_REMOVABLE {
            eprintln!(
                "[disk_info] 跳过非固定盘 {} (type={}，仅保留 FIXED/REMOVABLE)",
                drive_str, drive_type
            );
            start = end + 1;
            continue;
        }

        if let Some(info) = query_disk_info(drive_letter_up) {
            eprintln!(
                "[disk_info] 发现分区 {} 总={:.2}GB 已用={:.2}GB 可用={:.2}GB {} \"{}\"",
                drive_str,
                info.total_bytes as f64 / 1e9,
                info.used_bytes as f64 / 1e9,
                info.free_bytes as f64 / 1e9,
                info.file_system,
                info.volume_label
            );
            result.push(info);
        } else {
            eprintln!("[disk_info] 查询分区 {} 信息失败", drive_str);
        }
        start = end + 1;
    }

    eprintln!("[disk_info] 枚举到 {} 个固定/可移动分区", result.len());
    result
}

#[cfg(windows)]
pub fn query_disk_info(drive_letter: char) -> Option<DiskInfo> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW};

    // 根路径 "X:\"，宽字符 + null
    let root_path: Vec<u16> = format!("{}:\\", drive_letter)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // 拿容量信息
    let mut total_bytes: u64 = 0;
    let mut free_bytes: u64 = 0;
    let mut caller_free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            root_path.as_ptr(),
            &mut caller_free,
            &mut total_bytes,
            &mut free_bytes,
        )
    };
    if ok == 0 {
        eprintln!(
            "[disk_info] GetDiskFreeSpaceExW 失败 for {}: GetLastError={}",
            drive_letter,
            unsafe { GetLastError() }
        );
        return None;
    }

    // 拿卷标 + 文件系统
    let mut volume_label_buf = [0u16; 260];
    let mut file_system_buf = [0u16; 260];
    let mut serial = 0u32;
    let mut max_len = 0u32;
    let mut flags = 0u32;
    let ok = unsafe {
        GetVolumeInformationW(
            root_path.as_ptr(),
            volume_label_buf.as_mut_ptr(),
            volume_label_buf.len() as u32,
            &mut serial,
            &mut max_len,
            &mut flags,
            file_system_buf.as_mut_ptr(),
            file_system_buf.len() as u32,
        )
    };
    let (volume_label, file_system) = if ok != 0 {
        let label_len = volume_label_buf.iter().position(|&c| c == 0).unwrap_or(0);
        let fs_len = file_system_buf.iter().position(|&c| c == 0).unwrap_or(0);
        let label = String::from_utf16_lossy(&volume_label_buf[..label_len]);
        let fs = String::from_utf16_lossy(&file_system_buf[..fs_len]);
        (label, fs)
    } else {
        eprintln!(
            "[disk_info] GetVolumeInformationW 失败 for {}: GetLastError={}",
            drive_letter,
            unsafe { GetLastError() }
        );
        (String::new(), String::new())
    };

    Some(DiskInfo {
        drive_letter,
        volume_label,
        file_system,
        total_bytes,
        free_bytes,
        used_bytes: total_bytes.saturating_sub(free_bytes),
    })
}

/// 默认只显示 C 盘：从枚举结果里挑出 C 盘；如果找不到 C，就退回第一个固定盘。
/// 这样以后增加"盘符选择"功能时只需要改这里或者把整个 `enumerate_drives()` 的
/// 结果暴露给 UI 即可，不用改扫描入口。
#[cfg(windows)]
pub fn default_partition() -> Option<DiskInfo> {
    let drives = enumerate_drives();
    if drives.is_empty() {
        eprintln!("[disk_info] 没枚举到任何固定/可移动分区，将退回 demo 数据");
        return None;
    }
    if let Some(c) = drives.iter().find(|d| d.drive_letter == 'C') {
        eprintln!("[disk_info] 默认选择 C 盘: {}", c.display_name());
        return Some(c.clone());
    }
    eprintln!(
        "[disk_info] 没找到 C 盘，退回第一个分区: {}",
        drives[0].display_name()
    );
    Some(drives[0].clone())
}

// ─────────────────────────────────────────────────────────────────────────
// 非 Windows 平台的空实现，保证 Linux 上能 cargo check 过
// ─────────────────────────────────────────────────────────────────────────
#[cfg(not(windows))]
pub fn enumerate_drives() -> Vec<DiskInfo> {
    eprintln!("[disk_info] 非 Windows 平台，enumerate_drives 返回空列表");
    Vec::new()
}

#[cfg(not(windows))]
pub fn query_disk_info(_drive_letter: char) -> Option<DiskInfo> {
    None
}

#[cfg(not(windows))]
pub fn default_partition() -> Option<DiskInfo> {
    None
}

// ─────────────────────────────────────────────────────────────────────────
// 单元测试（跨平台）
// ─────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_name_with_label() {
        let info = DiskInfo {
            drive_letter: 'E',
            volume_label: "新加卷".into(),
            file_system: "NTFS".into(),
            total_bytes: 0,
            free_bytes: 0,
            used_bytes: 0,
        };
        assert_eq!(info.display_name(), "新加卷 (E:)");
    }

    #[test]
    fn test_display_name_without_label() {
        // FIX A: 空卷标时应该返回 "本地磁盘 (C:)" 而不是 "本地磁盘 C:"
        let info = DiskInfo {
            drive_letter: 'C',
            volume_label: String::new(),
            file_system: "NTFS".into(),
            total_bytes: 0,
            free_bytes: 0,
            used_bytes: 0,
        };
        assert_eq!(info.display_name(), "本地磁盘 (C:)");
    }

    #[test]
    fn test_root_path() {
        let info = DiskInfo {
            drive_letter: 'D',
            volume_label: String::new(),
            file_system: String::new(),
            total_bytes: 0,
            free_bytes: 0,
            used_bytes: 0,
        };
        assert_eq!(info.root_path(), "D:\\");
    }
}
