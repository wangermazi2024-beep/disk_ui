//! 磁盘分区枚举（Windows: GetLogicalDriveStringsW + GetVolumeInformationW + GetDiskFreeSpaceExW）。

#[derive(Clone, Debug, Default)]
pub struct DiskInfo {
    pub drive_letter: char,
    pub volume_label: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
}

impl DiskInfo {
    pub fn display_name(&self) -> String {
        if self.volume_label.is_empty() {
            format!("本地磁盘 ({}:)", self.drive_letter)
        } else {
            format!("{} ({}:)", self.volume_label, self.drive_letter)
        }
    }
    pub fn root_path(&self) -> String { format!("{}:\\", self.drive_letter) }
}

#[cfg(windows)]
pub fn enumerate_drives() -> Vec<DiskInfo> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDriveStringsW};
    const DRIVE_FIXED: u32 = 3;
    let mut result = Vec::new();
    let mut buf = [0u16; 520];
    let len = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) };
    if len == 0 || len as usize >= buf.len() { return result; }
    let mut start = 0usize;
    while start < len as usize {
        let end = buf[start..len as usize].iter().position(|&c| c == 0).map(|p| start + p).unwrap_or(len as usize);
        if start >= end { break; }
        let drive_str = String::from_utf16_lossy(&buf[start..end]);
        let drive_letter = drive_str.chars().next().unwrap_or('?').to_ascii_uppercase();
        let wide: Vec<u16> = drive_str.encode_utf16().chain(std::iter::once(0)).collect();
        if unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_FIXED {
            if let Some(info) = query_disk_info(drive_letter) {
                eprintln!("[disk_info] {}: 总={:.2}GB 已用={:.2}GB {} \"{}\"",
                    drive_str, info.total_bytes as f64/1e9, info.used_bytes as f64/1e9,
                    info.file_system, info.volume_label);
                result.push(info);
            }
        }
        start = end + 1;
    }
    eprintln!("[disk_info] 枚举到 {} 个分区", result.len());
    result
}

#[cfg(windows)]
pub fn query_disk_info(drive_letter: char) -> Option<DiskInfo> {
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW};
    let root: Vec<u16> = format!("{}:\\", drive_letter).encode_utf16().chain(std::iter::once(0)).collect();
    let mut total = 0u64; let mut free = 0u64; let mut caller = 0u64;
    if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut caller, &mut total, &mut free) } == 0 {
        return None;
    }
    let mut label = [0u16; 260]; let mut fs = [0u16; 260];
    let mut serial = 0u32; let mut max_len = 0u32; let mut flags = 0u32;
    let ok = unsafe { GetVolumeInformationW(root.as_ptr(), label.as_mut_ptr(), 260, &mut serial, &mut max_len, &mut flags, fs.as_mut_ptr(), 260) };
    let (label_s, fs_s) = if ok != 0 {
        let ll = label.iter().position(|&c| c == 0).unwrap_or(0);
        let fl = fs.iter().position(|&c| c == 0).unwrap_or(0);
        (String::from_utf16_lossy(&label[..ll]), String::from_utf16_lossy(&fs[..fl]))
    } else { (String::new(), String::new()) };
    Some(DiskInfo { drive_letter, volume_label: label_s, file_system: fs_s, total_bytes: total, free_bytes: free, used_bytes: total.saturating_sub(free) })
}

#[cfg(not(windows))]
pub fn enumerate_drives() -> Vec<DiskInfo> { Vec::new() }
#[cfg(not(windows))]
pub fn query_disk_info(_: char) -> Option<DiskInfo> { None }
