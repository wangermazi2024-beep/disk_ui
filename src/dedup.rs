//! 真正基于内容的重复文件检测：大小分组 → 局部哈希预筛 → 全文件哈希确认。
//!
//! 改之前 `categorize::build_duplicate_tree` 只按文件大小分组，大小相同不代表
//! 内容相同——实测一次 C 盘扫描能给出 42000+ 组"候选"，这么多里绝大多数其实
//! 是"大小凑巧相同、内容风马牛不相及"的假阳性，没法直接拿去做真正的去重操作
//! （删除/建符号链接），也没法指望用户一组组手工核实。这里补上内容比对，
//! 流程是主流去重工具（fclones / rdfind / dupeGuru 等）公认的标准做法：
//!
//!   1. 按文件大小分组（调用方已经做了，免费的第一轮筛选：大小都不同，
//!      内容不可能相同）。
//!   2. 组内文件数 ≥2 时，读一小段"局部哈希"（默认文件开头 64KB，文件本身
//!      比这个还小就整份都读了，读到的就是全部内容）。现实数据里"大小相同、
//!      内容不同"的文件占绝大多数（纯属巧合），这些文件开头几十 KB 基本都会
//!      不一样，这一步几乎不增加多少 I/O 成本就能刷掉绝大部分假阳性，
//!      不用为它们付出"读完整个文件"的代价。
//!   3. 局部哈希还相同的（真正可能重复、或者开头恰好一样的极少数情况），
//!      才去读整个文件算"全文件哈希"确认。
//!   4. 按全文件哈希（局部哈希阶段已经确认是全部内容的，直接用局部哈希）
//!      分组，得到的结果比"只按大小分组"精确得多。
//!
//! 哈希函数用的是 `std::hash::Hasher`（`DefaultHasher`，SipHash 的一种），
//! 不是 MD5/SHA-256 这类加密哈希——这里的目的是"快速筛掉不同内容"，不是
//! 防伪造/防碰撞攻击，加密哈希算得慢、纯属浪费。用标准库自带的还有个好处：
//! 不用新加依赖。真要是哪天发现哈希计算本身（而不是磁盘 I/O）成了瓶颈，
//! 换成 xxHash/BLAKE3 之类的 crate 是很容易的下一步优化——`hash_prefix`/
//! `hash_full` 这两个函数就是唯一需要改的地方，调用方（`group_by_content`
//! 以及再上层的 `categorize.rs`）完全不用跟着动。
//!
//! **重要，为后面接符号链接功能的人看**：这一套流程算出来的仍然是"候选"，
//! 哪怕全文件哈希都一样，理论上也还有极小概率的哈希碰撞（64 位哈希，按生日
//! 悖论估算，同一个大小分组里要塞进几十亿个文件才会有明显的碰撞概率，现实中
//! 不会遇到，但"理论上不为零"和"绝对为零"是两回事）。真的要执行删除/创建
//! 符号链接这种不可逆操作之前，必须在动手前对这一组文件再做一次逐字节比较
//! 作为最后一道保险——这个模块目前只负责"找出候选"，不负责"担保绝对相同"，
//! 后面实现符号链接的时候不能省略这一步。
//!
//! 性能上的取舍：这一步比之前"纯按大小分组、完全不读文件内容"要慢，因为它
//! 现在要真的读磁盘——但只读一小段（64KB）通常就能确认"不是重复"，只有小
//! 部分文件需要读完整个文件，比"把所有同大小文件都读一遍全文件哈希"要快得
//! 多。多线程并行读取/计算哈希（下面 `parallel_map`），线程数按 CPU 核心数
//! 定，和项目里 `scan.rs` 目录扫描用的思路一致。

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::Hasher;
use std::io::Read;

/// 局部哈希读取的字节数。64KB 是从主流去重工具的经验值里取的：小到几乎不
/// 增加 I/O 成本，大到足够刷掉绝大多数假阳性。
const PARTIAL_HASH_BYTES: usize = 64 * 1024;

/// 对一批"已知大小相同"的文件路径做内容比对分组。
///
/// 返回值是分组后的下标列表（下标对应传入的 `paths` 切片），每一组内的
/// 下标对应的文件被判定为内容相同；只有组内 ≥2 个文件才会出现在结果里
/// （单独一个文件、或者读取失败摸不清内容的文件，不会出现在任何结果组里——
/// 宁可漏判候选，也不能因为读不到内容就瞎猜"它们应该是重复的"）。
///
/// 调用方（`categorize::build_duplicate_tree`）负责先按大小分组、把
/// 每一组 `paths.len() >= 2` 的文件路径传进来；这个函数不管大小，只管
/// 传进来的这一批文件内容上是否相同。
pub fn group_by_content(paths: &[String]) -> Vec<Vec<usize>> {
    if paths.len() < 2 {
        return Vec::new();
    }

    // 阶段一：并行算局部哈希，同时顺带判断"局部哈希是不是已经等于全文件哈希"
    // （文件比 PARTIAL_HASH_BYTES 小的情况，读到的就是全部内容）。
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let partial = parallel_map(&refs, hash_prefix);

    let mut by_partial: HashMap<(u64, bool), Vec<usize>> = HashMap::new();
    for (i, r) in partial.into_iter().enumerate() {
        if let Some(key) = r {
            by_partial.entry(key).or_default().push(i);
        }
        // r 是 None 说明这个文件读取失败（权限不够/正被占用/中途被删了之类），
        // 直接跳过，不参与任何一组。
    }

    let mut groups: Vec<Vec<usize>> = Vec::new();
    for ((_, is_full_content), idxs) in by_partial {
        if idxs.len() < 2 {
            continue;
        }
        if is_full_content {
            // 局部哈希已经覆盖了整个文件内容，不用再读一遍，这一组就是最终结果。
            groups.push(idxs);
            continue;
        }
        // 阶段二：局部哈希相同、文件又比阈值大——开头一截长得一样，但后面还
        // 没读过，必须读完整个文件才能下结论。
        let sub_refs: Vec<&str> = idxs.iter().map(|&i| paths[i].as_str()).collect();
        let full = parallel_map(&sub_refs, hash_full);
        let mut by_full: HashMap<u64, Vec<usize>> = HashMap::new();
        for (k, r) in full.into_iter().enumerate() {
            if let Some(h) = r {
                by_full.entry(h).or_default().push(idxs[k]);
            }
        }
        for (_, g) in by_full {
            if g.len() >= 2 {
                groups.push(g);
            }
        }
    }
    groups
}

/// 读文件开头 `PARTIAL_HASH_BYTES` 字节算哈希；如果文件本身比这个还小，
/// 读到的就是全部内容，返回值第二个字段（`is_full_content`）标记为 `true`，
/// 调用方可以直接把这个哈希当全文件哈希用，不用再读第二遍。
fn hash_prefix(path: &str) -> Option<(u64, bool)> {
    let mut f = File::open(path).ok()?;
    let mut buf = vec![0u8; PARTIAL_HASH_BYTES];
    let mut total = 0usize;
    while total < buf.len() {
        match f.read(&mut buf[total..]) {
            Ok(0) => break, // 读到文件末尾了，且这一次没读满缓冲区
            Ok(n) => total += n,
            Err(_) => return None,
        }
    }
    let mut hasher = DefaultHasher::new();
    hasher.write(&buf[..total]);
    // 判断是不是已经到文件末尾：要么这次没读满缓冲区（上面的循环提前 break 了），
    // 要么缓冲区刚好读满、再试着多读一个字节看还有没有更多内容。
    let is_full_content = total < buf.len() || !read_one_more(&mut f);
    Some((hasher.finish(), is_full_content))
}

/// 尝试再读一个字节，返回是否真的读到了数据（用来判断文件是不是已经读完了）。
fn read_one_more(f: &mut File) -> bool {
    let mut probe = [0u8; 1];
    matches!(f.read(&mut probe), Ok(n) if n > 0)
}

/// 读完整个文件算哈希。只有局部哈希阶段判定"还不能排除是重复"的文件才会
/// 走到这一步，正常情况下这类文件在全体候选里只占很小一部分。
fn hash_full(path: &str) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    let mut hasher = DefaultHasher::new();
    let mut buf = [0u8; 256 * 1024];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.write(&buf[..n]),
            Err(_) => return None,
        }
    }
    Some(hasher.finish())
}

/// 把 `f` 并行应用到 `items` 的每一个元素上，返回按原顺序对齐的结果。
///
/// 工作量在调用之前就已知（一批文件路径，不像目录扫描那样"处理着处理着又
/// 冒出新任务"），所以不需要 `scan.rs` 里那套工作队列 + Condvar，直接把
/// `items` 和用来装结果的 `results` 切片按线程数切成几段、一个线程处理
/// 一段——各写各的那一段结果，互不重叠，编译器就能保证没有数据竞争，
/// 不需要锁。线程数按 CPU 核心数来，和 `scan.rs` 目录扫描的思路一致。
fn parallel_map<T, F>(items: &[&str], f: F) -> Vec<Option<T>>
where
    T: Send,
    F: Fn(&str) -> Option<T> + Sync,
{
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    let num_threads = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4).clamp(1, n);
    let chunk_len = n.div_ceil(num_threads);

    let mut results: Vec<Option<T>> = (0..n).map(|_| None).collect();
    std::thread::scope(|scope| {
        for (item_chunk, result_chunk) in items.chunks(chunk_len).zip(results.chunks_mut(chunk_len)) {
            let f = &f;
            scope.spawn(move || {
                for (item, slot) in item_chunk.iter().zip(result_chunk.iter_mut()) {
                    *slot = f(*item);
                }
            });
        }
    });
    results
}
