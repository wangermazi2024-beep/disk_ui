//! 真正会碰用户文件系统的两个操作：删除到回收站、打开系统"属性"对话框。
//! 单独拆一个模块（而不是塞进 ui/tree_list.rs），因为这两个是纯 Win32 调用，
//! 不依赖 egui，之后 compact_tree.rs（扩展名分类/重复文件查找）右键菜单
//! 想要同样的"删除"/"属性"时可以直接复用，不用再抄一遍。

/// 把一个文件/文件夹删除到回收站（不是永久删除）。
///
/// 用 `SHFileOperationW` + `FOF_ALLOWUNDO`，这是 Windows Shell 标准的"移到回收站"
/// 方式——`std::fs::remove_file`/`remove_dir_all` 是直接永久删除，不会经过回收站，
/// 绝对不能用在这里。`FOF_NOCONFIRMATION` 关掉系统自带的二次确认弹窗，是因为
/// 我们在应用自己的 UI 里已经有一个确认弹窗了，两层确认反而啰嗦；
/// `FOF_NOERRORUI` 关掉系统自带的错误弹窗，改成把错误原因返回给调用方，
/// 由应用自己的状态栏/弹窗统一展示，风格和其它报错保持一致。
///
/// `pFrom` 要求是"用 \0 分隔、以两个 \0 结尾"的路径列表，哪怕只删一个文件也要这么拼——
/// 这是 `SHFileOperationW` 从 Win32 API 设计之初就有的老接口约定，不这么拼会读到脏内存。
#[cfg(windows)]
pub fn delete_to_recycle_bin(path: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
        FO_DELETE, SHFILEOPSTRUCTW,
    };

    if path.is_empty() {
        return Err("路径为空".to_string());
    }
    // 双 \0 结尾的宽字符缓冲区。
    let mut from: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().collect();
    from.push(0);
    from.push(0);

    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    let ret = unsafe { SHFileOperationW(&mut op) };
    crate::applog::log(&format!("[file_ops] 删除到回收站: {path} (ret={ret}, aborted={})", op.fAnyOperationsAborted));
    if ret != 0 {
        return Err(format!("删除失败（错误码 0x{ret:X}）"));
    }
    if op.fAnyOperationsAborted != 0 {
        return Err("操作被取消".to_string());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn delete_to_recycle_bin(_path: &str) -> Result<(), String> {
    Err("仅支持 Windows".to_string())
}

/// 打开系统原生的"属性"对话框（`ShellExecuteW` 的 `properties` 谓词，
/// 和在资源管理器里右键选"属性"是同一个系统弹窗，不是自己画一个仿制的）。
#[cfg(windows)]
pub fn open_properties(path: &str) {
    use std::os::windows::ffi::OsStrExt;
    if path.is_empty() {
        return;
    }
    let verb: Vec<u16> = "properties".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect();
    // ShellExecuteW(hwnd, verb, file, params, dir, show_cmd)
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(),
            std::ptr::null(), std::ptr::null(), 1,
        )
    };
    crate::applog::log(&format!("[file_ops] 打开属性对话框: {path} (ShellExecuteW={result:?})"));
    if (result as isize) <= 32 {
        crate::applog::log(&format!("[file_ops] 打开属性对话框失败: {path}"));
    }
}

#[cfg(not(windows))]
pub fn open_properties(_path: &str) {}
