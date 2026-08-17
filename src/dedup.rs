//! 真正基于内容的重复文件检测：大小分组 → header 哈希预筛 → 全文件哈希最终
//! 确认。
//!
//! # 这一版改了什么、为什么
//!
//! 第一版（261.8 秒那次）能把"按大小分组"之外的假阳性过滤掉，但慢；第二版
//! 去掉了多余的 footer 预筛，还是要 258.1 秒——查完 GitHub 上几个主流开源
//! 去重工具的实现、也搜了 Beyond Compare 官方对"CRC/哈希 vs 逐字节比较"的
//! 说明之后，定位到几个真正的性能杀手：
//!
//! 1. **每个"大小分组"都单独开一轮 `thread::scope` + `spawn`**（第一版的问题，
//!    已用常驻 [`WorkerPool`] 解决）。
//! 2. **多余的 footer 预筛**（第二版的问题，已去掉，见下面流水线说明）。
//! 3. **最终确认阶段用 SHA-256，这一版最大的改动**。第二版特意选了 SHA-256
//!    是为了让结果能被 `Get-FileHash`/`certutil` 这类系统自带工具核对，但
//!    实测数据摆在眼前：一次 C 盘扫描里 742075 个候选文件，最后确认出
//!    157870 组疑似重复——**重复率高到离谱**，意味着绝大多数进入最终确认
//!    阶段的文件本来就真的是重复文件，没有侥幸被提前筛掉的空间，
//!    读完+算完整个文件的哈希是绕不开的工作量。这种情况下"用多快的哈希算法"
//!    直接决定了总耗时，而 SHA-256（没有 SHA 硬件指令加速的 CPU 上）比
//!    SIMD 加速的 BLAKE3 慢好几倍。这一版把最终确认也换成了 BLAKE3——
//!    "能不能被系统自带工具核对"和"这一步到底要跑多久"之间，这一版选了后者。
//!    如果确实需要用系统工具核对某个具体文件，BLAKE3 官方提供了 `b3sum`
//!    命令行工具（和这里用的是同一个算法库），或者干脆只需要针对某一小撮
//!    你关心的文件手动跑一下 `certutil`/`Get-FileHash` 做个人工抽查——不需要
//!    我们对每一个文件都用一个慢几倍的算法，只为了"理论上能被核对"这个
//!    多数情况下用不上的好处。
//! 4. **进度条"卡在 100% 不动"是真的卡住，不是假的**——不是日志拖慢的（上一版
//!    确实有个每组重复文件都记一行日志的问题，已经去掉了，见 `categorize.rs`），
//!    是进度条本身的展示逻辑有问题：以前只有一个"总进度"，用 header 预筛阶段
//!    的文件数当分母，全文件确认阶段是在这批文件的一个子集上又跑一遍、也会
//!    往同一个计数器里加——如果重复率不高，这个子集很小，进度条多走几个百分
//!    点就完事了，不容易注意到；但这次重复率高达 21%（157870/742075），
//!    确认阶段处理的文件数几乎和预筛阶段一样多，进度条走到"预筛阶段的
//!    100%"之后，还有几乎同样多的工作在后面，只是全都被夹到了"100%"这个
//!    数字里显示不出来，看起来就是卡住了。这一版把进度拆成两个独立阶段
//!    （见 [`HashPhase`]），各自从 0 数到各自的 100%，界面上也会明确提示
//!    "现在在哪个阶段"，不会再有这种误导。
//!
//! # 两段哈希流水线（参考 fclones / fddf / yadf 等主流开源去重工具的取舍）
//!
//! 查了几个 GitHub 上口碑较好的开源去重工具（`pkolaczk/fclones`、
//! `birkenfeld/fddf`、`jRimbault/yadf`）的实现和设计笔记，也查了 Beyond
//! Compare 官方对"CRC 对比 vs 逐字节比较"取舍的公开说明，取了个折中：
//!
//!   1. **大小分组**（调用方负责，免费的第一轮筛选）。
//!   2. **header 哈希**：读文件开头 64KB 算哈希。现实数据里的假阳性（"大小
//!      相同、内容其实不一样"）绝大多数在这一步就会被刷掉——开头都不一样，
//!      后面根本不用看，这一步的成本很低，值得保留（不像 footer 那轮，
//!      yadf 的经验是"性价比不划算"）。
//!   3. **全文件哈希最终确认**：header 相同的文件，直接读完整个文件确认。
//!      正常情况下走到这一步的文件数量取决于数据本身的重复率——重复率低，
//!      这一步处理的文件很少，几乎不影响总耗时；重复率高（比如这次的 C 盘
//!      数据），这一步就是真正的大头，哈希算法选得快不快直接决定总耗时，
//!      见上面第 3 点。Beyond Compare 官方文档也提到同样的道理："CRC 对比
//!      必须读完整个文件才能算出校验值，而逐字节比较可以在发现第一个不同
//!      字节的地方就提前退出"——对于真正相同的文件，这两种方式工作量其实
//!      一样（都得读完），逐字节比较的优势只体现在"其实不是重复文件"的
//!      情况上，而这些情况已经被 header 预筛过滤掉了大半，所以这里继续用
//!      哈希而不是逐字节比较，能顺带拿到一个可以复用（展示给用户/供以后
//!      符号链接功能核验）的哈希值，逐字节比较拿不到这个副产品。
//!
//! # 哈希算法的选择：header 预筛和最终确认都用 BLAKE3
//!
//! - `birkenfeld/fddf`（GitHub 上一个专门的 Rust 去重小工具）默认就是全程
//!   用 BLAKE3——SIMD 加速，是目前非加密哈希里数一数二快的选择，256 位输出，
//!   实际使用中不用担心碰撞（比标准库自带的 `DefaultHasher`/SipHash 快得多，
//!   也比没有硬件指令加速时的 SHA-256 快得多）。
//! - header 阶段只取哈希的前 8 字节当分组 key（这一步只是"预筛"，不需要
//!   完整 256 位，省内存）；最终确认阶段用完整的 256 位十六进制字符串。
//!
//! # 已知局限 / 后续可以做的事
//!
//! - **没有做 fclones 那种"按物理扇区顺序读取"的 HDD 优化**。机械硬盘上顺序读
//!   比随机读快得多，fclones 会先探测磁盘类型、对 HDD 按文件在磁盘上的物理
//!   位置排序再读。这里线程数直接按 CPU 核心数走，SSD 上没问题，机械硬盘上
//!   如果发现"线程数太多反而更慢"（磁头来回抢着跳），是因为触发了这个已知
//!   局限，值得作为下一步优化。
//! - **没有做 fclones 那种"哈希结果持久化缓存"**（`--cache` 选项，把哈希值
//!   连同文件的 mtime/大小一起存下来，下次扫描如果文件没变就跳过重新计算）。
//!   本应用里"重复文件"标签页本来就是"打开一次算一次、标签页开着就不重算"
//!   （见 `app.rs` 的 `open_duplicate_tab`），跨标签页/跨会话的持久化缓存
//!   是明显的下一步优化方向，尤其是如果以后要支持"重新扫描刷新"这种操作。
//! - **哈希值目前只进日志（而且只记第一条做示例，不是每条都记），UI 上看不到**。
//!   更好的位置是在重复文件列表里加一列"哈希"直接展示（`DuplicateGroup.hash_hex`
//!   已经带着这个值），可以直接在界面上复制/核对，等以后做这块 UI 的时候
//!   接上就行，`DuplicateGroup` 这个数据结构不用改。
//!
//! # 给以后接符号链接功能的人看
//!
//! 哪怕全文件哈希都一样，理论上仍有极小概率的哈希碰撞（256 位哈希，现实中
//! 不会遇到，但"理论上不为零"和"绝对为零"是两回事）。真的要执行删除/创建
//! 符号链接这种不可逆操作之前，必须在动手前对这一组文件再做一次逐字节比较
//! 作为最后一道保险——这个模块只负责"找出候选"，不负责"担保绝对相同"，
//! 后面接符号链接功能的时候不能省略这一步。

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

/// header 预筛读取的字节数。64KB 是从主流去重工具的经验值里取的：
/// 小到几乎不增加 I/O 成本，大到足够刷掉绝大多数假阳性。
const WINDOW_BYTES: usize = 64 * 1024;

/// 当前在跑哪个阶段——用来在进度回调里区分"预筛"和"最终确认"，两个阶段各自
/// 独立计数（各自从 0 到各自的 100%），不会像以前那样共用一个计数器，
/// 在重复率高的数据集上进度条走到"预筛阶段的 100%"之后又要看着它"卡住不动"
/// 一大截（其实是在跑第二阶段，只是数字上体现不出来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashPhase {
    /// 第一步：读文件开头一小段算哈希，快速排除"大小相同但内容一开始就不同"
    /// 的假阳性。
    Prefilter,
    /// 第二步：读完整个文件算哈希，确认到底是不是真的重复。数据重复率越高，
    /// 这一步要处理的文件越多、耗时占比越大。
    Confirm,
}

/// 一组确认重复的文件。`file_indices` 是调用方传进来的 `paths` 切片里的下标。
pub struct DuplicateGroup {
    pub size: u64,
    /// 全文件 BLAKE3（十六进制小写，256 位）。
    pub hash_hex: String,
    pub file_indices: Vec<usize>,
}

/// 主入口：给一批"已经按大小分好组"的候选文件，跑完整个哈希确认流程。
///
/// `size_groups`：`(文件大小, 下标列表)`，下标指向 `paths`；调用方负责先按
/// 大小分组、只把组内 >= 2 个文件的分组传进来（只有 1 个文件的分组没有比较
/// 意义，传进来也会被忽略，但白占一次遍历，不如调用方自己先筛掉）。
///
/// `on_progress(phase, done, total)` 在处理过程中会被调用若干次（按完成数量
/// 节流，不是每处理一个文件就调一次，避免几十万次回调本身变成新的开销）。
/// 两个阶段（见 [`HashPhase`]）各自独立计数，`done`/`total` 都是"当前这个
/// 阶段"的数字，不会互相污染，调用方可以直接拿去画进度条、不用再夹一道
/// `min` 防止"超过 100%"。
pub fn find_duplicates(
    paths: &[String],
    size_groups: Vec<(u64, Vec<usize>)>,
    on_progress: &dyn Fn(HashPhase, u64, u64),
) -> Vec<DuplicateGroup> {
    let total: u64 = size_groups.iter().map(|(_, idxs)| idxs.len() as u64).sum();
    if total == 0 {
        return Vec::new();
    }
    let pool = WorkerPool::new(worker_thread_count());

    // ---- 阶段一：header 哈希（BLAKE3，读开头 WINDOW_BYTES 字节）----
    let header_jobs: Vec<(usize, u64)> = size_groups
        .iter()
        .flat_map(|(size, idxs)| idxs.iter().map(move |&i| (i, *size)))
        .collect();
    let header_hashes = run_stage(&pool, &header_jobs, paths, HashPhase::Prefilter, on_progress, |path, size| {
        hash_window(path, 0, (size as usize).min(WINDOW_BYTES))
    });

    let mut by_header: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    for &(idx, size) in &header_jobs {
        if let Some(&h) = header_hashes.get(&idx) {
            by_header.entry((size, h)).or_default().push(idx);
        }
        // 拿不到哈希（读取失败：权限不够/文件被占用/扫描之后文件被删了之类）的
        // 直接跳过，不参与任何一组——宁可漏判候选，也不能瞎猜。
    }

    // header 已经相同、且组内还有 >= 2 个文件的，直接进最终哈希确认——不再
    // 像更早的版本那样为大文件多加一轮 "footer 哈希" 预筛（yadf 的经验：
    // 多一轮预筛在 SSD 上反而更慢，见模块顶部注释）。
    let mut confirm_jobs: Vec<(usize, u64)> = Vec::new();
    for ((size, _h), idxs) in by_header {
        if idxs.len() >= 2 {
            confirm_jobs.extend(idxs.iter().map(|&i| (i, size)));
        }
    }

    // ---- 阶段二：全文件哈希最终确认（BLAKE3，不再是 SHA-256——见模块顶部
    //      "这一版改了什么"第 3 点，重复率高的数据集这一步的哈希算法快慢
    //      直接决定总耗时）----
    // 一次性对所有存活到这一步的候选跑完，不再按"来自哪个 header 分组"拆成
    // 一批批地单独跑——批次越碎，每批单独走一次结果通道创建/收集的固定开销
    // 占比就越高，这正是第一版慢的核心原因之一，这一版要避免重蹈覆辙。
    let mut results = Vec::new();
    if !confirm_jobs.is_empty() {
        let full_hashes = run_stage(&pool, &confirm_jobs, paths, HashPhase::Confirm, on_progress, hash_full_blake3);
        let mut by_full: HashMap<(u64, String), Vec<usize>> = HashMap::new();
        for &(idx, size) in &confirm_jobs {
            if let Some(h) = full_hashes.get(&idx) {
                by_full.entry((size, h.clone())).or_default().push(idx);
            }
        }
        for ((size, hash_hex), file_indices) in by_full {
            if file_indices.len() >= 2 {
                results.push(DuplicateGroup { size, hash_hex, file_indices });
            }
        }
    }
    results
}

/// 把一批 `(下标, 文件大小)` 任务丢进线程池并行跑 `f(path, size)`，收集结果。
///
/// 结果通过 `mpsc` 通道收集，不是"把结果数组切片分给各线程各写各的"那种静态
/// 分片——静态分片要求提前知道每个任务耗时差不多才能均衡负载，但这里各文件
/// 大小、位于磁盘哪个位置都不一样，耗时差异可能很大；线程池 + 共享队列 +
/// 通道收集结果是"谁先干完谁去拿下一个任务"，天然做负载均衡，不会出现
/// "一个线程分到的那一份全是大文件、其他线程早就干完在干等"的情况。
fn run_stage<T, F>(
    pool: &WorkerPool,
    jobs: &[(usize, u64)],
    paths: &[String],
    phase: HashPhase,
    on_progress: &dyn Fn(HashPhase, u64, u64),
    f: F,
) -> HashMap<usize, T>
where
    T: Send + 'static,
    F: Fn(&str, u64) -> Option<T> + Send + Sync + 'static,
{
    let n = jobs.len();
    let mut out = HashMap::with_capacity(n);
    if n == 0 {
        return out;
    }
    let f = Arc::new(f);
    let (tx, rx) = mpsc::channel::<(usize, Option<T>)>();
    for &(idx, size) in jobs {
        let path = paths[idx].clone();
        let tx = tx.clone();
        let f = Arc::clone(&f);
        pool.execute(move || {
            let r = f(&path, size);
            let _ = tx.send((idx, r));
        });
    }
    drop(tx); // 发送端就剩下面这些克隆，全部丢给了线程池；这里 drop 掉本体，
              // 等所有任务真正跑完、各自的 tx 克隆都被丢弃后，下面的 recv 循环
              // 才会在收完 n 条消息后自然结束（其实用不上这个信号，因为下面是
              // 按计数 n 收的，但显式 drop 更清楚地表达"这里不再发送了"）。

    // 按完成数量节流进度回调，不是每条消息都回调一次——几十万个文件的话，
    // 每条都回调等于几十万次函数调用（可能还包括一次 UI 线程间的 channel
    // send），量级上去了本身也会变成明显开销。这个阶段自己的 done 从 0 开始
    // 数到 n，不再依赖外部传入/累加的"总进度"——两个阶段互不干扰，调用方
    // 展示的时候不用再猜"这个数字有没有把另一阶段的进度也算进来"。
    const REPORT_EVERY: u64 = 500;
    let mut done = 0u64;
    for received in 1..=n as u64 {
        if let Ok((idx, r)) = rx.recv() {
            if let Some(v) = r {
                out.insert(idx, v);
            }
        }
        done += 1;
        if received % REPORT_EVERY == 0 || received == n as u64 {
            on_progress(phase, done, n as u64);
        }
    }
    out
}

/// 读文件里从 `offset` 开始的 `len` 字节，算 BLAKE3 哈希，只取前 8 字节转成
/// `u64` 当分组 key——这一步只是"预筛"，不是最终确认结果，不需要完整 256 位，
/// 截断成 64 位足够把"内容不同的文件"筛掉，分组用的 `HashMap` key 也更省内存。
/// 真正确认文件身份用的是 [`hash_full_blake3`] 算出的完整 256 位 BLAKE3。
fn hash_window(path: &str, offset: u64, len: usize) -> Option<u64> {
    if len == 0 {
        return Some(0);
    }
    let mut f = File::open(path).ok()?;
    if offset > 0 {
        f.seek(SeekFrom::Start(offset)).ok()?;
    }
    let mut buf = vec![0u8; len];
    let mut total = 0usize;
    while total < len {
        match f.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => return None,
        }
    }
    let hash = blake3::hash(&buf[..total]);
    let bytes = hash.as_bytes();
    Some(u64::from_le_bytes(bytes[..8].try_into().unwrap()))
}

/// 读完整个文件算 BLAKE3（十六进制小写字符串）。走到这一步的文件数量取决于
/// 数据本身的重复率（见模块顶部"这一版改了什么"第 3 点）——重复率低的时候
/// 这一步很快就过去了；重复率高的时候这一步就是耗时大头，所以特意选了
/// BLAKE3 而不是更慢的 SHA-256。
fn hash_full_blake3(path: &str, _size: u64) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 256 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => { hasher.update(&buf[..n]); }
            Err(_) => return None,
        }
    }
    Some(hasher.finalize().to_hex().to_string())
}

/// 线程数：CPU 核心数 × 2（和项目里 `scan.rs` 目录扫描用的公式一致）。
/// 这批工作大部分时间在等磁盘 I/O、不是在算哈希，线程数比核心数多一些能让
/// "一个线程在等磁盘返回数据"的空隙被别的线程用来干活，不会白白空转。
///
/// 这个公式对 SSD 合适，机械硬盘（HDD）上不一定：fclones 作者自己都说过
/// "磁盘随机访问延迟是主要瓶颈"，机械硬盘上开太多线程意味着磁头在不同文件
/// 之间来回跳着读，物理寻道的开销可能比"多线程重叠等待时间"省下来的还多。
/// fclones 会探测硬盘类型、按物理扇区顺序排列读取顺序来专门优化 HDD，这里
/// 没有做这一层（Windows 上探测 HDD/SSD 需要额外的 `DeviceIoControl` 调用，
/// 属于明显的后续优化项）。如果发现在机械硬盘上线程数越多反而越慢，先手动
/// 把这个公式换成固定的小数字（比如 2~4）试试，比继续加线程更可能有效果。
fn worker_thread_count() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_mul(2).max(2)
}

/// 极简线程池，只用标准库、不引入新依赖。设计基本照抄《Rust 程序设计语言》
/// 官方教程"多线程 Web 服务器"那一章的实现——是网上被验证过最多次的 std-only
/// 线程池写法，没有用什么冷门技巧，正确性容易推理，出问题也容易对照教程排查。
///
/// 关键是"常驻"：线程在 `WorkerPool::new` 里一次性创建好，整个 [`find_duplicates`]
/// 调用期间反复复用来跑 header 预筛/全文件确认这两个阶段的任务，不是
/// "来一批任务就创建一批线程、跑完就销毁"——上一版慢的头号原因就是每个大小
/// 分组都单独开一轮 `thread::scope`，分组数一多，光是线程创建/销毁的开销就
/// 压过了实际计算。
struct WorkerPool {
    sender: Option<mpsc::Sender<Job>>,
    workers: Vec<thread::JoinHandle<()>>,
}

type Job = Box<dyn FnOnce() + Send + 'static>;

impl WorkerPool {
    fn new(size: usize) -> Self {
        let size = size.max(1);
        let (sender, receiver) = mpsc::channel::<Job>();
        // 标准库的 `Receiver` 不能克隆给多个线程各拿一份，经典做法是包一层
        // `Arc<Mutex<_>>`：谁先抢到锁谁就拿走下一个任务去跑，跑的时候早就把
        // 锁放掉了（`recv()` 一返回，锁的作用域就结束），不会出现"一个线程
        // 在算哈希、别的线程还在干等锁"的情况。
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(size);
        for _ in 0..size {
            let receiver = Arc::clone(&receiver);
            workers.push(thread::spawn(move || loop {
                let job = { receiver.lock().unwrap().recv() };
                match job {
                    Ok(job) => job(),
                    // 发送端（`WorkerPool::sender`）被 drop 掉了，说明不会再有
                    // 新任务了，退出循环、线程自然结束。
                    Err(_) => break,
                }
            }));
        }
        Self { sender: Some(sender), workers }
    }

    fn execute<F: FnOnce() + Send + 'static>(&self, f: F) {
        if let Some(s) = &self.sender {
            let _ = s.send(Box::new(f));
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        // 先把发送端丢掉，worker 线程的 `recv()` 会陆续收到 `Err`（说明任务
        // 已经派发完、不会再有新的了）然后各自退出循环；再逐个 `join`，
        // 保证 `WorkerPool` 被 drop 的时候，所有工作线程都已经彻底退出，
        // 不会有"线程还在后台跑、但线程池对象已经没了"这种悬空状态。
        drop(self.sender.take());
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}
