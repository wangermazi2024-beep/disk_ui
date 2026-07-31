//! Windows 批量目录枚举。
//!
//! 原实现的性能问题：`std::fs::read_dir` 拿到条目后，对**每一个文件**都额外调用一次
//! `CreateFileW` + `GetFileInformationByHandle`（为了拿 nNumberOfLinks/FileIndex 做硬链接去重），
//! 外加压缩/稀疏文件再调一次 `GetCompressedFileSizeW`。
//! 在 79 万文件的盘上，这是 79 万次额外的内核对象创建/销毁，是 17s vs windirstat 10s 的主因
//! （FindFirstFile/NtQueryDirectoryFile 批量枚举本身很快，慢在"逐文件开 handle"）。
//!
//! 真正的解决办法：用 `GetFileInformationByHandleEx` + `FileIdBothDirectoryInfo` 信息类，
//! 对**整个目录只开一次 handle**，然后用一个大缓冲区（64KB）循环把该目录下所有条目
//! 一次性批量拿出来——每条记录里已经包含：
//!   - FileName（文件名）
//!   - EndOfFile（逻辑大小，等价于 $DATA.FileSize）
//!   - AllocationSize（物理/占用大小，对压缩、稀疏文件同样准确，等价于 $DATA.AllocatedLength）
//!   - FileAttributes
//!   - CreationTime / LastWriteTime
//!   - FileId（64 位，NTFS 文件参考号，卷内唯一，可直接当硬链接去重 key，不需要再开 handle）
//!
//! 这样单个目录只有 1 次 CreateFileW（打开目录本身）+ 少数几次 GetFileInformationByHandleEx
//! （每次批量拿几百到几千条），彻底消除了"每文件一次 handle"的开销，
//! 同时物理大小也不再需要单独调用 GetCompressedFileSizeW。
//!
//! 如果该 API 不可用（极老系统 / 非 NTFS 卷 / ReFS 某些情况），上层会 fallback 到
//! `std::fs::read_dir`（见 scan.rs 中的 fallback 分支)。

use std::path::Path;

#[derive(Clone)]
pub struct RawDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub logical: u64,
    pub physical: u64,
    pub attrs: u32,
    pub modified_ft: u64,
    pub created_ft: u64,
    pub accessed_ft: u64,
    /// NTFS 文件参考号（卷内唯一），用于硬链接去重；拿不到时为 0。
    pub file_id: u64,
}

#[cfg(windows)]
pub fn enum_dir_batch(path: &Path) -> std::io::Result<Vec<RawDirEntry>> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandleEx, FileIdBothDirectoryRestartInfo,
        FileIdBothDirectoryInfo, FILE_ID_BOTH_DIR_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str())
        .chain(std::iter::once(0))
        .collect();

    // 目录本身只开一次 handle（FILE_LIST_DIRECTORY 权限即可，不需要 GENERIC_READ）。
    let handle: HANDLE = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }

    // 64KB 缓冲区：单次系统调用通常能装下几百到上千条目录项，比逐文件 FindNextFile 少几个数量级的调用次数。
    const BUF_SIZE: usize = 64 * 1024;
    let mut buf: Vec<u8> = vec![0u8; BUF_SIZE];
    let mut out = Vec::new();
    let mut first_call = true;

    loop {
        let class = if first_call { FileIdBothDirectoryRestartInfo } else { FileIdBothDirectoryInfo };
        first_call = false;

        // 每次调用前清零：避免把上一轮调用残留在缓冲区尾部的陈旧字节，
        // 误当成新条目解析（例如本轮返回的数据比上一轮少的情况）。
        buf.iter_mut().for_each(|b| *b = 0);

        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                class,
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
            )
        };
        if ok == 0 {
            let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            const ERROR_NO_MORE_FILES: u32 = 18;
            unsafe { CloseHandle(handle) };
            if err == ERROR_NO_MORE_FILES {
                return Ok(out);
            }
            if out.is_empty() {
                return Err(std::io::Error::from_raw_os_error(err as i32));
            }
            return Ok(out);
        }

        let mut offset: usize = 0;
        loop {
            // 固定头部（不含变长 FileName）至少要完整落在缓冲区内才能安全读取。
            let header_size = std::mem::size_of::<FILE_ID_BOTH_DIR_INFO>();
            if offset + header_size > BUF_SIZE {
                break;
            }
            let entry_ptr = unsafe { buf.as_ptr().add(offset) as *const FILE_ID_BOTH_DIR_INFO };
            let entry: &FILE_ID_BOTH_DIR_INFO = unsafe { &*entry_ptr };

            let name_len_bytes = entry.FileNameLength as usize;
            // 变长文件名部分也必须完整落在缓冲区内，否则这条记录不可信，直接停止本轮解析
            // （不当成正常数据用，防止读出垃圾大小/名字污染统计）。
            if offset + header_size + name_len_bytes > BUF_SIZE {
                break;
            }
            let name_ptr = entry.FileName.as_ptr();
            let name_u16: &[u16] = unsafe {
                std::slice::from_raw_parts(name_ptr, name_len_bytes / 2)
            };
            let name = String::from_utf16_lossy(name_u16);

            if name != "." && name != ".." {
                const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
                let is_dir = entry.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                let file_id = unsafe {
                    // FileId 是一个 128 位的 LARGE_INTEGER 风格联合体在部分绑定里，
                    // windows-sys 里 FILE_ID_BOTH_DIR_INFO.FileId 是 i64。
                    entry.FileId as u64
                };
                out.push(RawDirEntry {
                    name,
                    is_dir,
                    logical: entry.EndOfFile as u64,
                    physical: entry.AllocationSize as u64,
                    attrs: entry.FileAttributes,
                    modified_ft: filetime_to_u64(entry.LastWriteTime),
                    created_ft: filetime_to_u64(entry.CreationTime),
                    accessed_ft: filetime_to_u64(entry.LastAccessTime),
                    file_id,
                });
            }

            if entry.NextEntryOffset == 0 {
                break;
            }
            offset += entry.NextEntryOffset as usize;
            if offset >= BUF_SIZE {
                break;
            }
        }
    }
}

#[cfg(windows)]
fn filetime_to_u64(ft: i64) -> u64 {
    // LARGE_INTEGER -> 展开为标准 Windows FILETIME(100ns since 1601) 的 u64 表示。
    if ft < 0 { 0 } else { ft as u64 }
}
