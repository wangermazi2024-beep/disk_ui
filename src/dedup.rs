//! 真正基于内容的重复文件检测：大小分组 → header 哈希预筛 → footer 哈希预筛
//! → 全文件 SHA-256 最终确认。
//!
//! # 这一版改了什么、为什么
//!
//! 上一版能把"按大小分组"之外的假阳性过滤掉，但实测在一次 C 盘全盘扫描上
//! 跑到了 261.8 秒——查了 GitHub 上几个主流开源去重工具的实现之后，定位到
//! 两个真正的性能杀手，都不是"哈希算法本身太慢"：
//!
//! 1. **上一版每个"大小分组"都单独开一轮 `thread::scope` + `spawn`**。
//!    C 盘这种量级能有十几万个大小分组，等于创建/销毁了几十万次操作系统线程，
//!    光是线程创建/销毁本身的开销就能压过实际做哈希计算的时间——这是主要
//!    瓶颈。这一版换成常驻的 [`WorkerPool`]：整个去重流程只创建一次线程池
//!    （数量按 CPU 核心数来），所有阶段共用，不再"处理一批就建一批线程、用完
//!    就销毁"。
//! 2. **原来一步到位对每个候选文件读整个文件算哈希**。现实数据里"大小相同、
//!    内容不同"的文件占绝大多数（纯属巧合），大部分文件只要读一小段就能确认
//!    "不是重复"，没必要读完整个文件。
//!
//! # 三段哈希流水线（参考 fclones/fddf 等主流开源去重工具的设计）
//!
//! 查了几个 GitHub 上口碑较好的开源去重工具（`pkolaczk/fclones`、
//! `birkenfeld/fddf`）的实现，它们的公共套路是"多级哈希，一级比一级贵，
//! 每一级都尽量早地把不可能重复的文件刷掉"：
//!
//!   1. **大小分组**（调用方负责，免费的第一轮筛选）。
//!   2. **header 哈希**：读文件开头 64KB 算哈希。现实数据里的假阳性绝大多数
//!      在这一步就会被刷掉——开头都不一样，后面根本不用看。
//!   3. **footer 哈希**：header 相同、文件又比较大（> 2×64KB，否则读两次不
//!      划算，直接进第 4 步）的，再读文件末尾 64KB 算一次——这是 fclones 的
//!      设计，防住"开头是同一个模板/文件头，后面内容其实不一样"这种情况
//!      （常见于某些容器格式、日志文件、数据库文件）。
//!   4. **全文件 SHA-256 最终确认**：只有前两轮都刷不掉的文件才会走到这里，
//!      正常情况下这类文件在全体候选里只占很小一部分。
//!
//! # 哈希算法的选择：预筛用 BLAKE3，最终确认用 SHA-256
//!
//! 两种哈希的取舍是刻意分开的，不是图省事直接复用同一个：
//!
//! - **header/footer 预筛用 BLAKE3**：`birkenfeld/fddf`（GitHub 上一个专门的
//!   Rust 去重小工具）用的就是 BLAKE3——SIMD 加速，是目前非加密哈希里数一数二
//!   快的选择，比标准库自带的 `DefaultHasher`（SipHash）快得多，这两级只是
//!   "预筛"，用多快的哈希都不影响最终结果的正确性，只影响筛得快不快。
//! - **最终确认用 SHA-256**（`sha2` crate）：慢一些，但换来的是这份结果能被
//!   **其他工具独立验证**——Windows 自带 PowerShell 的
//!   `Get-FileHash -Algorithm SHA256` 或者 `certutil -hashfile <path> SHA256`
//!   都能直接算出同样的值，不用额外装任何东西。这是本次改动特意做的取舍：
//!   如果最终确认也用 BLAKE3，虽然更快，但普通人手头没有能立刻拿来对比验证
//!   的工具，"能不能验证算法是否准确"这件事本身的价值，在最终确认这一步上
//!   比"再快一点"更重要（反正走到这一步的文件已经是少数，SHA-256 慢一点对
//!   总耗时影响很小）。
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
//!
//! # 给以后接符号链接功能的人看
//!
//! 哪怕全文件 SHA-256 都一样，理论上仍有极小概率的哈希碰撞（256 位哈希，
//! 现实中不会遇到，但"理论上不为零"和"绝对为零"是两回事）。真的要执行
//! 删除/创建符号链接这种不可逆操作之前，必须在动手前对这一组文件再做一次
//! 逐字节比较作为最后一道保险——这个模块只负责"找出候选"，不负责"担保绝对
//! 相同"，后面接符号链接功能的时候不能省略这一步。

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

/// header/footer 预筛读取的字节数。64KB 是从主流去重工具的经验值里取的：
/// 小到几乎不增加 I/O 成本，大到足够刷掉绝大多数假阳性。
const WINDOW_BYTES: usize = 64 * 1024;

/// 一组确认重复的文件。`file_indices` 是调用方传进来的 `paths` 切片里的下标。
pub struct DuplicateGroup {
    pub size: u64,
    /// 全文件 SHA-256（十六进制小写），特意选这个算法而不是更快的 BLAKE3——
    /// 见模块顶部注释，为的是能直接用系统自带工具交叉核对。
    pub sha256_hex: String,
    pub file_indices: Vec<usize>,
}

/// 主入口：给一批"已经按大小分好组"的候选文件，跑完整个哈希确认流程。
///
/// `size_groups`：`(文件大小, 下标列表)`，下标指向 `paths`；调用方负责先按
/// 大小分组、只把组内 >= 2 个文件的分组传进来（只有 1 个文件的分组没有比较
/// 意义，传进来也会被忽略，但白占一次遍历，不如调用方自己先筛掉）。
///
/// `on_progress(done, total)` 在处理过程中会被调用若干次（按完成数量节流，
/// 不是每处理一个文件就调一次，避免几十万次回调本身变成新的开销）；`total`
/// 只统计了 header 阶段的文件数——footer/全文件确认阶段是在这批文件的一个
/// 子集上跑的，`done` 有可能因此超过这个 `total`（大部分情况下 header 阶段
/// 就能刷掉绝大多数候选，超出的部分很小），调用方展示进度条时应该用
/// `done.min(total)` 夹一下，避免看起来"超过 100%"。
pub fn find_duplicates(
    paths: &[String],
    size_groups: Vec<(u64, Vec<usize>)>,
    on_progress: &dyn Fn(u64, u64),
) -> Vec<DuplicateGroup> {
    let total: u64 = size_groups.iter().map(|(_, idxs)| idxs.len() as u64).sum();
    if total == 0 {
        return Vec::new();
    }
    let pool = WorkerPool::new(worker_thread_count());
    let mut done = 0u64;

    // ---- 阶段一：header 哈希（BLAKE3，读开头 WINDOW_BYTES 字节）----
    let header_jobs: Vec<(usize, u64)> = size_groups
        .iter()
        .flat_map(|(size, idxs)| idxs.iter().map(move |&i| (i, *size)))
        .collect();
    let header_hashes = run_stage(&pool, &header_jobs, paths, total, &mut done, on_progress, |path, size| {
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

    // header 已经相同、且组内还有 >= 2 个文件的，按"要不要再走 footer 预筛"
    // 分成两批：文件比较小（<= 2×WINDOW_BYTES，读两次不划算，或者 header
    // 已经覆盖了全部内容）的直接进最终确认；比较大的先囤着，下面统一走一次
    // footer 预筛。
    let mut confirm_jobs: Vec<(usize, u64)> = Vec::new();
    let mut footer_jobs: Vec<(usize, u64)> = Vec::new();
    for ((size, _h), idxs) in by_header {
        if idxs.len() < 2 {
            continue;
        }
        if size <= 2 * WINDOW_BYTES as u64 {
            confirm_jobs.extend(idxs.iter().map(|&i| (i, size)));
        } else {
            footer_jobs.extend(idxs.iter().map(|&i| (i, size)));
        }
    }

    // ---- 阶段二：footer 哈希（BLAKE3，读结尾 WINDOW_BYTES 字节）----
    if !footer_jobs.is_empty() {
        let footer_hashes = run_stage(&pool, &footer_jobs, paths, total, &mut done, on_progress, |path, size| {
            hash_window(path, size.saturating_sub(WINDOW_BYTES as u64), WINDOW_BYTES)
        });
        let mut by_footer: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
        for &(idx, size) in &footer_jobs {
            if let Some(&h) = footer_hashes.get(&idx) {
                by_footer.entry((size, h)).or_default().push(idx);
            }
        }
        for ((size, _h), idxs) in by_footer {
            if idxs.len() >= 2 {
                confirm_jobs.extend(idxs.iter().map(|&i| (i, size)));
            }
        }
    }

    // ---- 阶段三：全文件 SHA-256 最终确认 ----
    // 一次性对所有存活到这一步的候选跑完，不再按"来自哪个 header/footer 分组"
    // 拆成一批批地单独跑——批次越碎，每批单独走一次结果通道创建/收集的固定
    // 开销占比就越高，这正是上一版慢的核心原因，这一版要避免重蹈覆辙。
    let mut results = Vec::new();
    if !confirm_jobs.is_empty() {
        let full_hashes = run_stage(&pool, &confirm_jobs, paths, total, &mut done, on_progress, hash_full_sha256);
        let mut by_full: HashMap<(u64, String), Vec<usize>> = HashMap::new();
        for &(idx, size) in &confirm_jobs {
            if let Some(h) = full_hashes.get(&idx) {
                by_full.entry((size, h.clone())).or_default().push(idx);
            }
        }
        for ((size, sha256_hex), file_indices) in by_full {
            if file_indices.len() >= 2 {
                results.push(DuplicateGroup { size, sha256_hex, file_indices });
            }
        }
    }
    results
}

/// 把一批 `(下标, 文件大小)` 任务丢进线程池并行跑 `f(path, size)`，收集结果。
///
/// 结果通过 `mpsc` 通道收集，不是"把结果数组切片分给各线程各写各的"那种静态
/// 分片——静态分片要求提前知道每个任务耗时差不多才能均衡负载，但这里各文件
/// 大小、located 在磁盘哪个位置都不一样，耗时差异可能很大；线程池 + 共享队列
/// + 通道收集结果是"谁先干完谁去拿下一个任务"，天然做负载均衡，不会出现
/// "一个线程分到的那一份全是大文件、其他线程早就干完在干等"的情况。
fn run_stage<T, F>(
    pool: &WorkerPool,
    jobs: &[(usize, u64)],
    paths: &[String],
    total_overall: u64,
    done_so_far: &mut u64,
    on_progress: &dyn Fn(u64, u64),
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
    // send），量级上去了本身也会变成明显开销。
    const REPORT_EVERY: u64 = 500;
    for received in 1..=n as u64 {
        if let Ok((idx, r)) = rx.recv() {
            if let Some(v) = r {
                out.insert(idx, v);
            }
        }
        *done_so_far += 1;
        if received % REPORT_EVERY == 0 || received == n as u64 {
            on_progress(*done_so_far, total_overall);
        }
    }
    out
}

/// 读文件里从 `offset` 开始的 `len` 字节，算 BLAKE3 哈希，只取前 8 字节转成
/// `u64` 当分组 key——这一步只是"预筛"，不是最终确认结果，不需要完整 256 位，
/// 截断成 64 位足够把"内容不同的文件"筛掉，分组用的 `HashMap` key 也更省内存。
/// 真正确认文件身份用的是 [`hash_full_sha256`] 算出的完整 SHA-256。
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

/// 读完整个文件算 SHA-256（十六进制小写字符串），走到这一步的文件正常情况下
/// 在全体候选里只占很小一部分（大部分假阳性在 header/footer 预筛阶段就被
/// 刷掉了），所以这里用慢一些但能被外部工具验证的 SHA-256，不心疼这点成本。
fn hash_full_sha256(path: &str, _size: u64) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut f = File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 256 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return None,
        }
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// 线程数：CPU 核心数 × 2（和项目里 `scan.rs` 目录扫描用的公式一致）。
/// 这批工作大部分时间在等磁盘 I/O、不是在算哈希，线程数比核心数多一些能让
/// "一个线程在等磁盘返回数据"的空隙被别的线程用来干活，不会白白空转。
fn worker_thread_count() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_mul(2).max(2)
}

/// 极简线程池，只用标准库、不引入新依赖。设计基本照抄《Rust 程序设计语言》
/// 官方教程"多线程 Web 服务器"那一章的实现——是网上被验证过最多次的 std-only
/// 线程池写法，没有用什么冷门技巧，正确性容易推理，出问题也容易对照教程排查。
///
/// 关键是"常驻"：线程在 `WorkerPool::new` 里一次性创建好，整个 [`find_duplicates`]
/// 调用期间反复复用来跑 header/footer/全文件确认这三个阶段的任务，不是
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
