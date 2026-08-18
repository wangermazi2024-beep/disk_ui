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

// ============================================================================
// 文件/文件夹占用检测 + 自动重试
// ============================================================================
//
// 背景：创建符号链接（去重/迁移到其他盘）的第一步通常是"先把原文件删掉"，
// 如果这个文件正被别的程序打开（哪怕只是被资源管理器选中预览、被某个编辑器
// 打开、被杀毒软件正在扫描），删除会直接失败——用户看到一个 Win32 错误码，
// 完全不知道是谁占用的、也不知道该怎么办。这里查了几种解决方案：
//
// 1. **Restart Manager API**（这次实现的）——Windows 官方提供、专门用来回答
//    "这个文件正被哪些进程/服务占用"这个问题的 API，Windows 资源管理器自己
//    删文件弹出"文件正在使用"对话框、Windows Installer 更新前检测占用，
//    用的都是这一套（`RmStartSession` → `RmRegisterResources` →
//    `RmGetList` → `RmEndSession`）。相比让用户自己去翻任务管理器猜，
//    直接告诉用户"被 XX.exe（PID 1234）占用"体验好得多。局限：它依赖
//    Windows 自己维护的"谁打开了这个文件"这份记录，绝大多数场景都能覆盖，
//    但个别驱动级别的极端占用方式可能查不到（下面第 3 点是补充方案）。
// 2. **自动重试**（这次也实现的）——很多占用其实是瞬时的：杀毒软件正在
//    扫描这个文件、索引服务刚碰了一下、资源管理器缩略图缓存正在读——这种
//    "转瞬即逝"的占用，等个几百毫秒重试一次往往就好了，不需要真的去查
//    是谁占用、也不需要用户介入。真正持续被占用（比如文件在 Word 里开着）
//    重试才会真的失败，这时候再查占用进程告诉用户。
// 3. **没有实现、但值得记录的备选方案**：
//    - `NtQuerySystemInformation`（`SystemHandleInformation`）遍历全系统
//      句柄表，逐个进程比对——这是 Sysinternals `handle.exe`/Process Explorer
//      背后的原理，比 Restart Manager 更底层、能查到的场景更全，但这是半
//      官方/未完全文档化的 API（微软没有正式承诺其稳定性），实现复杂度也
//      高得多（要枚举所有进程、对每个句柄查类型再解析成文件路径）。除非
//      发现 Restart Manager 在实际使用中确实有覆盖不到的场景，不建议为了
//      "更全"去换成这个更脆弱的方案。
//    - `MOVEFILE_DELAY_UNTIL_REBOOT`（`MoveFileExW` 的一个标志）——如果确认
//      是被占用、用户也不想等/不方便关掉占用它的程序，可以把删除操作注册成
//      "下次开机时执行"，这是 Windows Installer 处理"正在使用中的系统文件"
//      的经典手段。对我们的场景（用户主动去重/迁移）不算优先级很高的方案，
//      但作为"重试也不行、占用进程又是关键系统服务不方便强制关闭"时的兜底
//      选项，值得以后需要的时候加上。
//    - 直接调用 Restart Manager 的 `RmShutdown`/`RmRestart` 去强制关闭占用
//      该文件的应用——技术上可行（Windows Installer 就是这么干的），但这是
//      "未经用户明确同意就关掉人家正在用的程序"，用户体验和数据安全风险都
//      不小（比如强制关掉一个正在编辑但没保存的 Word 文档），这次没有做，
//      以后如果要做，必须先在 UI 上明确告知用户"将要关闭以下程序"并拿到
//      确认，不能静默执行。

/// 占用当前文件/文件夹的一个进程（或服务）。
pub struct LockingProcess {
    pub pid: u32,
    /// 服务返回服务的长名称，普通程序返回用户能看懂的程序名（不是可执行文件
    /// 路径），这是 Restart Manager 自己给出的"友好名称"。
    pub app_name: String,
    /// 如果这个"进程"其实是个 Windows 服务，这里是服务的短名字（可以用来
    /// 提示用户"net stop 这个服务名"或者去服务管理器里找）；不是服务就是
    /// `None`。
    pub service_name: Option<String>,
}

/// 查询哪些进程/服务正占用着给定的一批文件（或文件夹——文件夹本身一般不会
/// 被"占用"，但如果调用方想查文件夹下某个具体文件，直接传那个文件路径）。
///
/// 用的是 Windows 官方 Restart Manager API，见上面模块内的说明。查询本身
/// 不会关闭/打断任何进程，纯只读操作，随便调用没有副作用。
#[cfg(windows)]
pub fn find_locking_processes(paths: &[&str]) -> Result<Vec<LockingProcess>, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::RestartManager::{
        RmEndSession, RmGetList, RmRegisterResources, RmStartSession, RM_PROCESS_INFO,
    };

    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Restart Manager 的会话 key 缓冲区。文档里这个长度是
    // `CCH_RM_SESSION_KEY + 1`（微软定义为 32+1），这里多给一些余量，不去
    // 依赖具体拿到的常量值是否精确对得上——缓冲区只嫌大不嫌小，Rm 只会往里
    // 写一个远小于缓冲区长度的 GUID 字符串。
    let mut session_key = [0u16; 64];
    let mut session_handle: u32 = 0;
    let start_ret = unsafe { RmStartSession(&mut session_handle, 0, session_key.as_mut_ptr()) };
    if start_ret != ERROR_SUCCESS {
        return Err(format!("RmStartSession 失败（错误码 {start_ret}）"));
    }
    // 不管中间哪一步失败，只要 session 开成功了就必须调用 RmEndSession 收尾，
    // 用一个"守卫"结构体保证——即使中途用 `?`/`return` 提前退出，Drop 也会
    // 把 session 关掉，不会泄漏 Restart Manager 的会话资源（一个用户会话
    // 同一时间最多只能开 64 个 Restart Manager 会话，用完不关迟早会把这个
    // 上限占满）。
    struct SessionGuard(u32);
    impl Drop for SessionGuard {
        fn drop(&mut self) {
            unsafe { RmEndSession(self.0) };
        }
    }
    let _guard = SessionGuard(session_handle);

    let wide_paths: Vec<Vec<u16>> = paths
        .iter()
        .map(|p| std::ffi::OsStr::new(p).encode_wide().chain(std::iter::once(0)).collect())
        .collect();
    let path_ptrs: Vec<*const u16> = wide_paths.iter().map(|p| p.as_ptr()).collect();

    let reg_ret = unsafe {
        RmRegisterResources(
            session_handle, path_ptrs.len() as u32, path_ptrs.as_ptr(),
            0, std::ptr::null(), 0, std::ptr::null(),
        )
    };
    if reg_ret != ERROR_SUCCESS {
        return Err(format!("RmRegisterResources 失败（错误码 {reg_ret}）"));
    }

    // 先用一个小缓冲区问一次"到底有多少个"（`RmGetList` 在缓冲区不够大时
    // 会告诉你实际需要多少），再按这个数字分配足够大的缓冲区正式取一次——
    // 这是 Restart Manager API 的标准用法（MSDN 示例、上面查到的开源实现
    // `LockCheck`/`.NET Matters` 专栏的写法都是这个套路）。
    let mut needed: u32 = 0;
    let mut got: u32 = 0;
    let mut reboot_reasons: u32 = 0;
    let first_ret = unsafe { RmGetList(session_handle, &mut needed, &mut got, std::ptr::null_mut(), &mut reboot_reasons) };
    // ERROR_MORE_DATA（234）是正常情况——第一次问的时候本来就没打算真的
    // 拿到列表，只是为了问一下 `needed` 是多少；`got` 传 0 意味着调用方
    // 提供的缓冲区容量是 0，所以只要注册了资源、有任何进程占用，这里几乎
    // 总会返回 ERROR_MORE_DATA。真的返回 ERROR_SUCCESS 说明没有任何进程
    // 占用（`needed` 会是 0）。
    const ERROR_MORE_DATA: u32 = 234;
    if first_ret != ERROR_SUCCESS && first_ret != ERROR_MORE_DATA {
        return Err(format!("RmGetList（探测数量）失败（错误码 {first_ret}）"));
    }
    if needed == 0 {
        return Ok(Vec::new());
    }

    let mut buf: Vec<RM_PROCESS_INFO> = Vec::with_capacity(needed as usize);
    // `RM_PROCESS_INFO` 全部字段要么是定长数组要么是数值/句柄，全零是合法的
    // 初始状态（不含任何指针/需要析构的字段），`zeroed()` 安全。
    for _ in 0..needed {
        buf.push(unsafe { std::mem::zeroed() });
    }
    got = needed;
    let second_ret = unsafe { RmGetList(session_handle, &mut needed, &mut got, buf.as_mut_ptr(), &mut reboot_reasons) };
    if second_ret != ERROR_SUCCESS {
        return Err(format!("RmGetList（取列表）失败（错误码 {second_ret}）"));
    }

    let mut result = Vec::with_capacity(got as usize);
    for info in buf.iter().take(got as usize) {
        let app_name = String::from_utf16_lossy(&info.strAppName)
            .trim_end_matches('\0').to_string();
        let service_name = String::from_utf16_lossy(&info.strServiceShortName)
            .trim_end_matches('\0').to_string();
        result.push(LockingProcess {
            pid: info.Process.dwProcessId,
            app_name: if app_name.is_empty() { format!("(PID {})", info.Process.dwProcessId) } else { app_name },
            service_name: if service_name.is_empty() { None } else { Some(service_name) },
        });
    }
    Ok(result)
}

#[cfg(not(windows))]
pub fn find_locking_processes(_paths: &[&str]) -> Result<Vec<LockingProcess>, String> {
    Ok(Vec::new())
}

/// 把一批占用进程格式化成一行给用户看的文字，比如
/// "被 QQ.exe（PID 1234）、Everything.exe（PID 5678）占用"。
pub fn describe_locking_processes(procs: &[LockingProcess]) -> String {
    if procs.is_empty() {
        return String::new();
    }
    let names: Vec<String> = procs
        .iter()
        .map(|p| match &p.service_name {
            Some(svc) => format!("{}（服务 {svc}，PID {}）", p.app_name, p.pid),
            None => format!("{}（PID {}）", p.app_name, p.pid),
        })
        .collect();
    format!("被 {} 占用", names.join("、"))
}

/// 删除到回收站，遇到失败自动重试几次再放弃——很多占用是瞬时的（杀毒软件
/// 扫描、索引服务、缩略图缓存），没必要第一次失败就直接向用户报错。重试
/// 全部失败之后，尝试查一下到底是被谁占用的，把这个信息附加进错误消息里，
/// 而不是甩给用户一个看不懂的错误码——这是这次的主要目的：不只是"检测到
/// 占用"，而是直接告诉用户"是谁占用的"，方便用户去手动关掉。
pub fn delete_to_recycle_bin_with_retry(path: &str) -> Result<(), String> {
    const RETRIES: u32 = 3;
    let mut last_err = String::new();
    for attempt in 0..=RETRIES {
        match delete_to_recycle_bin(path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e;
                if attempt < RETRIES {
                    // 退避时间逐次拉长（300ms、600ms、900ms），给瞬时占用
                    // 更充分的时间自己解除，又不至于让用户等太久。
                    std::thread::sleep(std::time::Duration::from_millis(300 * (attempt as u64 + 1)));
                }
            }
        }
    }
    match find_locking_processes(&[path]) {
        Ok(procs) if !procs.is_empty() => {
            Err(format!("{last_err}；{}", describe_locking_processes(&procs)))
        }
        _ => Err(last_err), // 查不到占用进程（可能不是"占用"导致的失败，是别的原因），保留原始错误信息
    }
}
