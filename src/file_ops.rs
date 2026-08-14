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

/// 打开系统原生的"属性"对话框。
///
/// 一开始用的是 `ShellExecuteW` 直接传 `"properties"` 谓词，看起来是最简单的
/// 写法、也是网上最常见的例子，但实测对 C:\TEST 这种普通文件夹会失败，返回值
/// 0x1f（`SE_ERR_NOASSOC`——"找不到关联的应用程序"）。原因是 `ShellExecuteW`
/// 的 `properties` 谓词走的是"按文件扩展名/ProgID 查注册表里登记的静态谓词"
/// 这条路，文件夹本身没有关联的"应用程序"，自然查不到；这也是为什么资源
/// 管理器右键"属性"这个功能，微软官方文档专门强调普通调用方式覆盖不到、
/// 必须用 `ShellExecuteExW` 配 `SEE_MASK_INVOKEIDLIST` 才行——这个标志让
/// Shell 改成通过目标的"快捷菜单处理器"（`IContextMenu`）去调用谓词，
/// 而不是查注册表里的静态关联，跟资源管理器右键菜单走的是同一条路，
/// 文件、文件夹、甚至没有关联程序的文件类型都能正常弹出属性对话框。
#[cfg(windows)]
pub fn open_properties(path: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW};

    if path.is_empty() {
        return;
    }
    let verb: Vec<u16> = "properties".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect();

    // 结构体里有个 hIcon/hMonitor union 字段，我们完全用不上（没设
    // SEE_MASK_ICON/SEE_MASK_HMONITOR），零初始化整个结构体最省事，不用去抠
    // union 具体叫什么名字——全零对这些字段（要么是数值 0，要么是空指针）
    // 都是合法值，不会有未定义行为。
    let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.fMask = SEE_MASK_INVOKEIDLIST;
    sei.hwnd = std::ptr::null_mut();
    sei.lpVerb = verb.as_ptr();
    sei.lpFile = file.as_ptr();
    sei.lpParameters = std::ptr::null();
    sei.lpDirectory = std::ptr::null();
    sei.nShow = 1; // SW_SHOWNORMAL；SEE_MASK_INVOKEIDLIST 弹的是对话框，这个值基本不影响什么，按惯例填。

    let ok = unsafe { ShellExecuteExW(&mut sei) };
    crate::applog::log(&format!("[file_ops] 打开属性对话框: {path} (ShellExecuteExW={ok})"));
    if ok == 0 {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        crate::applog::log(&format!("[file_ops] 打开属性对话框失败: {path} (GetLastError={err})"));
    }
}

#[cfg(not(windows))]
pub fn open_properties(_path: &str) {}

// ============================================================================
// 计划中：文件去重 / 迁移到其他盘（尚未实现，右键菜单目前只有禁用的占位按钮）
// ============================================================================
//
// 目标场景（来自需求）：
//   1. C 盘内部去重——同一个文件在 C 盘出现多份（比如"重复文件查找"那个分析
//      标签页找出来的结果），只留一份真实数据，其余位置换成链接，腾出空间但
//      每个位置看起来还在、内容还一样。
//   2. C 盘文件/文件夹迁移到 D 盘——腾 C 盘空间，原路径留一个链接，依赖这个
//      路径的程序/快捷方式/注册表项还能正常工作，不用到处改路径。
//   3. 去重 + 迁移一起做——重复文件干脆只在 D 盘留一份，C 盘所有位置都换成
//      指向 D 盘那一份的链接。
//
// Windows 上做链接有三种机制（硬链接/符号链接/目录联接），但故意只选**符号链接
// 一种**，其余两种都不用——不是不知道它们，是权衡过后排除的：
//   - 硬链接只能同盘、只能文件，场景 2/3（跨盘）用不了，逼得代码里得按"同盘/
//     跨盘"分两条路径、UI 上还要么让用户选、要么自己悄悄判断，多一种机制就多
//     一次"这次到底走哪条路"的心智负担，对用户来说这个选择没有意义（他们只
//     关心"腾出空间/搬到 D 盘"，不关心底层是硬链接还是符号链接）。
//   - 目录联接只能目录、不能文件，场景 1（文件去重）用不了，一样存在"这次该用
//     哪种"的问题。
//   统一只用符号链接（能跨盘、文件目录都支持），三个场景一套逻辑，用户不用
//   感知任何差异，也不会出现"同样是点右键创建链接，这次是硬链接、下次是
//   符号链接，行为却不一样"的困惑。代价是：本来能用硬链接做到的"同盘也不用
//   担心目标被删就断链"这个好处放弃了，统一按符号链接的语义处理，见下面
//   "断链风险"那条。
//
// 具体到三个场景怎么用符号链接：
//
// 场景 1（C 盘内部去重）：
//   保留其中一份物理数据，其余重复位置删除后用 `CreateSymbolicLinkW` 指向
//   保留的那份。虽然同盘的话硬链接本可以做得更"正"（多个目录项指向同一份
//   数据，谁删都不影响别人），但为了只维护一种机制、用户心智一致，这里统一
//   还是用符号链接——代价是"保留的那份被删掉/挪走，其余位置的链接会断"，
//   这个风险在 UI 上要提示清楚（比如提示"请勿删除或移动 XXX，其余 N 个位置
//   依赖它"）。
//
// 场景 2（单个文件/文件夹从 C 迁到 D，原地留个"影子"）：
//   先把文件/文件夹完整复制到 D 盘新位置，校验通过（至少比对大小，最好能算
//   一下 hash 摘要）之后再删除 C 盘原文件/文件夹，最后用 `CreateSymbolicLinkW`
//   在原路径创建指向 D 盘新位置的符号链接（文件夹要带上
//   `SYMBOLIC_LINK_FLAG_DIRECTORY` 标志）。千万不能"先删后建"——复制过程中
//   程序崩了、磁盘满了会直接丢数据。
//
// 场景 3（去重 + 跨盘迁移一起做）：
//   重复文件只把其中一份复制到 D 盘，C 盘那几份原来的位置全部删除后用
//   `CreateSymbolicLinkW` 指过去，和场景 1 一样有断链风险，只是这次断链的
//   源头（D 盘那份）离用户平时活动的 C 盘更远，更容易被无意中动到，UI 提示
//   要更明确。
//
// 不管哪个场景，动手删除/挪动原文件之前都必须：
//   - 新副本先完整复制过去并校验，绝不能"先删后建"。
//   - 检查目标盘剩余空间够不够，不够提前拦下来，不要复制到一半才发现。
//   - 处理"目标路径已经有同名文件/链接"的冲突（覆盖？改名？让用户选？）。
//   - 符号链接创建失败时（没开开发者模式、没有管理员权限、老版本 Windows 不
//     认 `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE` 这个 flag）要给用户
//     明确、可操作的提示（比如指引去哪里打开开发者模式），不能默默什么都不做。
//     Rust 标准库 `std::os::windows::fs::symlink_file` 处理这个兼容性问题的
//     办法是：先带上 `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE` 调用，如果
//     返回 `ERROR_INVALID_PARAMETER`（老系统不认这个 flag）就去掉这一位重试。
//
// 用到的 Win32 API 都在 `windows_sys::Win32::Storage::FileSystem`，Cargo.toml
// 里 `Win32_Storage_FileSystem` feature 已经开了，不用加新依赖：
//   - `CreateSymbolicLinkW(link_path, target_path, flags)` —— 唯一要用到的
//     创建函数。`flags` 里 `SYMBOLIC_LINK_FLAG_DIRECTORY`（0x1）表示目标是
//     目录，`SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE`（0x2）见上面
//     兼容性处理，两者可以按位或在一起同时使用。

/// 占位：创建符号链接（去重/迁移到其他盘）尚未实现。
///
/// 函数签名先定好，方便以后直接填实现、不用到时候再回头改调用点——`source` 是
/// 现在磁盘上的真实位置（会被删除、原地换成符号链接的那个），`target_dir` 是
/// 复制过去的目标目录（比如 D 盘上的某个归档目录）。目前只返回 `Err`：调用到
/// 这个函数说明流程走到了没实现的分支（UI 上这个按钮是禁用状态），不应该被
/// 真正调用到；返回 `Err` 而不是假装成功、悄悄什么都不做，是为了防止以后哪天
/// 不小心把按钮启用了却忘了填实现，结果用户点了却毫无反应还以为是自己电脑的
/// 问题。
#[allow(dead_code)]
pub fn create_symlink_placeholder(_source: &str, _target_dir: &str) -> Result<(), String> {
    Err("去重/迁移到其他盘功能尚未实现".to_string())
}
