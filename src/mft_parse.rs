//! 纯 MFT 记录解析逻辑，**无 `cfg(windows)` 限制**。
//!
//! 这个模块从 `mft_scan.rs` 抽出来，目的有两个：
//! 1. 让解析逻辑可以在 Linux 上跑单元测试（用合成的 MFT 字节流喂进去），
//!    验证字段提取是否正确、是否丢属性、Fixup 算法是否对。
//! 2. 让 `mft_scan.rs` 只剩 Windows 专有的 I/O 代码（CreateFileW / DeviceIoControl /
//!    AdjustTokenPrivileges），职责更清晰。
//!
//! 解析逻辑本身是纯字节操作，没有任何 Windows API 调用，所以可以跨平台编译。
//! 真正在 Windows 上跑时，`mft_scan::scan_drive_via_mft` 会用这里的
//! `apply_fixup` + `parse_record` 来处理读入内存的 $MFT 字节流。

use std::ffi::OsString;

// 在非 Windows 上也编过——OsString::from_wide 是 Windows 专有的，
// 这里给一个最小的 fallback：把 u16 序列当成 UTF-16 解码成 String，
// 再包装成 OsString。Linux 测试时只会用到这个 fallback 路径。
#[cfg(not(windows))]
mod utf16_fallback {
    use std::ffi::OsString;
    /// 把 u16 序列当成 UTF-16 解码后转成 OsString。
    /// 非 Windows 平台 OsString 内部就是 bytes（Unix）或 WTF-8（Wasm），
    /// 直接 from String 即可。
    pub fn from_wide(u16s: &[u16]) -> OsString {
        let s: String = String::from_utf16_lossy(u16s);
        OsString::from(s)
    }
}

#[cfg(windows)]
fn os_string_from_wide(u16s: &[u16]) -> OsString {
    use std::os::windows::ffi::OsStringExt;
    OsString::from_wide(u16s)
}
#[cfg(not(windows))]
fn os_string_from_wide(u16s: &[u16]) -> OsString {
    utf16_fallback::from_wide(u16s)
}

pub const ATTR_STANDARD_INFORMATION: u32 = 0x10;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
pub const ATTR_END: u32 = 0xFFFF_FFFF;
pub const ROOT_RECORD_INDEX: u64 = 5; // NTFS 规定根目录固定是第 5 条 MFT 记录。

/// 单条 MFT 记录里我们关心的字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    pub parent_record: u64,
    pub name: String,
    pub is_dir: bool,
    pub in_use: bool,
    pub is_base_record: bool,
    pub real_size: u64,
    /// 来自 $STANDARD_INFORMATION 的 LastModificationTime（FILETIME）。
    pub modified_ft: u64,
    /// 来自 $STANDARD_INFORMATION 的 Flags（FILE_ATTRIBUTE_*）。
    pub attributes: u32,
}

/// $STANDARD_INFORMATION 内容布局（resident）：
///   +0x00 CreationTime      (FILETIME 8B)
///   +0x08 LastModificationTime (FILETIME 8B)
///   +0x10 LastMftChangeTime (FILETIME 8B)
///   +0x18 LastAccessTime    (FILETIME 8B)
///   +0x20 AllocatedSize     (8B)
///   +0x28 RealSize          (8B)
///   +0x30 Flags             (4B, FILE_ATTRIBUTE_*)
///   +0x34 UsedSize          (4B)
///   ...
/// 我们只要 LastModificationTime 和 Flags。
pub const STD_INFO_OFFSET_MODIFIED: usize = 0x08;
pub const STD_INFO_OFFSET_FLAGS: usize = 0x30;
pub const STD_INFO_MIN_LEN: usize = 0x38; // 至少要读到 0x34 + 4

/// 对单条 MFT 记录做 Fixup 修正。
///
/// NTFS 每个扇区（通常 512 字节）的最后 2 字节在落盘时会被替换成
/// USA（Update Sequence Array）的 USN 值，原值放到 USA 里。读出来时
/// 必须把 USN 检查通过后，再把原值放回去，否则记录的某些字段会读到
/// 错误的 USN 值而不是真实数据。
///
/// `record` 整条记录的字节（含 FILE 头）；`bytes_per_sector` 通常 512。
/// 返回 true 表示 Fixup 通过，可以继续解析；false 表示记录损坏，应丢弃。
pub fn apply_fixup(record: &mut [u8], bytes_per_sector: u32) -> bool {
    if record.len() < 8 {
        return false;
    }
    let usa_offset = u16::from_le_bytes([record[4], record[5]]) as usize;
    let usa_count = u16::from_le_bytes([record[6], record[7]]) as usize; // 含 USN 本身
    if usa_count == 0 || usa_offset + usa_count * 2 > record.len() {
        return false;
    }
    let usn = [record[usa_offset], record[usa_offset + 1]];
    let sector = bytes_per_sector.max(512) as usize;
    for i in 1..usa_count {
        let sector_end = i * sector; // 每个扇区最后 2 字节
        if sector_end > record.len() {
            break;
        }
        let check = &record[sector_end - 2..sector_end];
        if check != usn {
            // 该扇区的 USN 不匹配：记录已损坏/不完整，放弃这条记录。
            return false;
        }
        let orig_off = usa_offset + i * 2;
        record[sector_end - 2] = record[orig_off];
        record[sector_end - 1] = record[orig_off + 1];
    }
    true
}

/// 解析单条 MFT FILE 记录，提取我们关心的字段。
///
/// 输入是已经过 `apply_fixup` 的字节切片。返回 `None` 表示这条记录
/// 不是有效的 FILE 记录（magic 不对 / 长度不够）。
///
/// **注意**：即使没有 `$FILE_NAME` 属性（某些系统元数据文件可能没有），
/// 也返回 `Some`，name 用 `"<FRN_NNN>"` 占位——这样不丢记录，建树时
/// 它们会挂到根目录或被忽略（取决于 parent_record）。
pub fn parse_record(record: &[u8]) -> Option<RawEntry> {
    if record.len() < 48 || &record[0..4] != b"FILE" {
        return None;
    }
    let flags = u16::from_le_bytes([record[22], record[23]]);
    let in_use = flags & 0x0001 != 0;
    let is_dir = flags & 0x0002 != 0;
    let first_attr_offset = u16::from_le_bytes([record[20], record[21]]) as usize;
    let base_record_ref = u64::from_le_bytes(record[32..40].try_into().unwrap());
    let is_base_record = (base_record_ref & 0x0000_FFFF_FFFF_FFFF) == 0;

    let mut parent_record: u64 = 0;
    // 收集所有 $FILE_NAME 属性，最后按优先级选最好的一个。
    // 这样能处理"先遇到 DOS 名、后遇到 WIN32 名"的情况，避免 PROGRA~1 覆盖 Program Files。
    // 每个元素: (namespace, name, parent_ref, real_size_from_filename)
    let mut file_names: Vec<(u8, String, u64, u64)> = Vec::new();
    let mut real_size: u64 = 0; // 来自 $DATA 属性（最准确，但可能在大文件的扩展记录里）
    let mut modified_ft: u64 = 0;
    let mut attributes: u32 = 0;

    let mut off = first_attr_offset;
    while off + 16 <= record.len() {
        let attr_type = u32::from_le_bytes(record[off..off + 4].try_into().unwrap());
        if attr_type == ATTR_END {
            break;
        }
        let attr_len = u32::from_le_bytes(record[off + 4..off + 8].try_into().unwrap()) as usize;
        if attr_len == 0 || off + attr_len > record.len() {
            break;
        }
        let non_resident = record[off + 8] != 0;
        // resident 属性的 value_off / value_len 在固定偏移上；non-resident 不一样。
        let value_off = u16::from_le_bytes(record[off + 20..off + 22].try_into().unwrap()) as usize;
        let value_len =
            u32::from_le_bytes(record[off + 16..off + 20].try_into().unwrap()) as usize;
        let content_start = off + value_off;

        if attr_type == ATTR_STANDARD_INFORMATION && !non_resident {
            if content_start + STD_INFO_MIN_LEN <= record.len() && value_len >= STD_INFO_MIN_LEN {
                modified_ft = u64::from_le_bytes(
                    record[content_start + STD_INFO_OFFSET_MODIFIED
                        ..content_start + STD_INFO_OFFSET_MODIFIED + 8]
                        .try_into()
                        .unwrap(),
                );
                attributes = u32::from_le_bytes(
                    record[content_start + STD_INFO_OFFSET_FLAGS
                        ..content_start + STD_INFO_OFFSET_FLAGS + 4]
                        .try_into()
                        .unwrap(),
                );
            }
        } else if attr_type == ATTR_FILE_NAME && !non_resident {
            // $FILE_NAME 内容布局：
            //   +0  ParentReference (8B)
            //   +8  CreationTime (8B)
            //   +16 ModificationTime (8B)
            //   +24 MftChangeTime (8B)
            //   +32 AccessTime (8B)
            //   +40 AllocatedSize (8B)
            //   +48 RealSize (8B)  ← 文件真实大小，作为 $DATA 在扩展记录时的 fallback
            //   +56 Flags (4B)
            //   +60 ReparseValue (4B)
            //   +64 NameLength (1B, 字符数)
            //   +65 Namespace (1B)
            //   +66 Name (UTF-16LE)
            if content_start + value_len <= record.len() && value_len >= 66 {
                let c = &record[content_start..content_start + value_len];
                let parent_ref = u64::from_le_bytes(c[0..8].try_into().unwrap());
                let name_len_chars = c[64] as usize;
                let ns = c[65]; // 0=POSIX 1=WIN32 2=DOS(8.3短名) 3=WIN32&DOS
                let name_bytes_len = name_len_chars * 2;
                if 66 + name_bytes_len <= c.len() && name_len_chars > 0 {
                    let u16s: Vec<u16> = c[66..66 + name_bytes_len]
                        .chunks_exact(2)
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    let name_str = os_string_from_wide(&u16s).to_string_lossy().into_owned();
                    // 从 $FILE_NAME 拿 real_size 作为 fallback
                    //（当 $DATA 在扩展记录里时，$FILE_NAME 的大小是唯一的来源）
                    let fname_real_size = if c.len() >= 56 {
                        u64::from_le_bytes(c[48..56].try_into().unwrap())
                    } else {
                        0
                    };
                    file_names.push((ns, name_str, parent_ref & 0x0000_FFFF_FFFF_FFFF, fname_real_size));
                }
            }
        } else if attr_type == ATTR_DATA {
            // 未命名的 $DATA 才代表文件主体大小；命名的是备用数据流(ADS)，跳过。
            let name_len = record[off + 9];
            if name_len == 0 {
                real_size = if non_resident {
                    // non-resident $DATA: real_size 在 off+48
                    if off + 56 <= record.len() {
                        u64::from_le_bytes(record[off + 48..off + 56].try_into().unwrap())
                    } else {
                        0
                    }
                } else {
                    u32::from_le_bytes(record[off + 16..off + 20].try_into().unwrap()) as u64
                };
            }
        }

        off += attr_len;
    }

    // 从收集到的 $FILE_NAME 列表里选最好的一个。
    // 优先级：WIN32(1) > WIN32&DOS(3) > POSIX(0) > DOS(2) > 其他
    // 这样 "Program Files" (ns=1 或 3) 胜过 "PROGRA~1" (ns=2)。
    let (name, fname_parent, fname_real_size) = if file_names.is_empty() {
        (format!("<FRN_{}>", 0u64), 0u64, 0u64)
    } else {
        let ns_priority = |ns: u8| -> u32 {
            match ns {
                1 => 0, // WIN32 - 最高优先级（长文件名）
                3 => 1, // WIN32&DOS - 第二（既是长名又是短名）
                0 => 2, // POSIX - 第三（通常也是长名）
                2 => 3, // DOS - 最低（8.3 短名，如 PROGRA~1）
                _ => 4,
            }
        };
        let best = file_names
            .iter()
            .min_by_key(|(ns, _, _, _)| ns_priority(*ns))
            .unwrap();
        (best.1.clone(), best.2, best.3)
    };

    // 如果 parent_record 还没设置，用 $FILE_NAME 的 parent
    if parent_record == 0 && fname_parent != 0 {
        parent_record = fname_parent;
    }
    // FIX: 如果 $DATA 的 real_size 是 0（大文件的 $DATA 在扩展记录里），
    // 用 $FILE_NAME 的 real_size 作为 fallback。
    // 这样 Hermes.exe (204MB) 即使 $DATA 在扩展记录，也能从 $FILE_NAME 拿到大小。
    if real_size == 0 && fname_real_size > 0 {
        real_size = fname_real_size;
    }

    Some(RawEntry {
        parent_record,
        name,
        is_dir,
        in_use,
        is_base_record,
        real_size,
        modified_ft,
        attributes,
    })
}

/// 诊断版：返回 (RawEntry, 解析到的属性类型列表, 是否有 $FILE_NAME)。
/// 用来排查"no_file_name"问题——看那些记录到底有哪些属性。
#[allow(dead_code)]
pub fn parse_record_with_diag(record: &[u8]) -> Option<(RawEntry, Vec<u32>, bool)> {
    if record.len() < 48 || &record[0..4] != b"FILE" {
        return None;
    }
    // 先用标准 parse_record 拿到 entry
    let entry = parse_record(record)?;

    // 再单独扫一遍收集属性类型列表和 has_file_name
    let first_attr_offset = u16::from_le_bytes([record[20], record[21]]) as usize;
    let mut attr_types: Vec<u32> = Vec::new();
    let mut has_file_name = false;
    let mut off = first_attr_offset;
    while off + 16 <= record.len() {
        let attr_type = u32::from_le_bytes(record[off..off + 4].try_into().unwrap());
        if attr_type == ATTR_END {
            break;
        }
        attr_types.push(attr_type);
        if attr_type == ATTR_FILE_NAME {
            has_file_name = true;
        }
        let attr_len = u32::from_le_bytes(record[off + 4..off + 8].try_into().unwrap()) as usize;
        if attr_len == 0 || off + attr_len > record.len() {
            break;
        }
        off += attr_len;
    }
    Some((entry, attr_types, has_file_name))
}

// ─────────────────────────────────────────────────────────────────────────
// 单元测试：用合成的 MFT 字节流验证解析逻辑，**Linux 上也能跑**。
// ─────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn put_u64(buf: &mut Vec<u8>, off: usize, v: u64) {
        let bytes = v.to_le_bytes();
        buf[off..off + 8].copy_from_slice(&bytes);
    }
    fn put_u32(buf: &mut Vec<u8>, off: usize, v: u32) {
        let bytes = v.to_le_bytes();
        buf[off..off + 4].copy_from_slice(&bytes);
    }
    fn put_u16(buf: &mut Vec<u8>, off: usize, v: u16) {
        let bytes = v.to_le_bytes();
        buf[off..off + 2].copy_from_slice(&bytes);
    }

    /// 构造一个最小的 MFT FILE 记录字节流。
    fn build_test_record(
        name: &str,
        parent_record: u64,
        is_dir: bool,
        real_size: u64,
        modified_ft: u64,
        attributes: u32,
    ) -> Vec<u8> {
        let mut buf = vec![0u8; 1024];

        // ── FILE 头 ──────────────────────────────────────────
        buf[0..4].copy_from_slice(b"FILE");
        put_u16(&mut buf, 4, 0x30); // usa_offset
        put_u16(&mut buf, 6, 1); // usa_count = 1（只有 USN 本身，不修正扇区）
        put_u16(&mut buf, 0x30, 0x0001); // USN 值
        let flags: u16 = 0x01 | if is_dir { 0x02 } else { 0x00 };
        put_u16(&mut buf, 22, flags);
        put_u16(&mut buf, 20, 0x38); // first_attr_offset
        put_u64(&mut buf, 32, 0); // base_record_ref = 0

        let mut off = 0x38usize;

        // ── $STANDARD_INFORMATION 属性 ─────────────────────
        let std_attr_content_len: usize = 0x48;
        let std_attr_total_len: usize = 24 + std_attr_content_len;
        put_u32(&mut buf, off, ATTR_STANDARD_INFORMATION);
        put_u32(&mut buf, off + 4, std_attr_total_len as u32);
        buf[off + 8] = 0; // resident
        put_u32(&mut buf, off + 16, std_attr_content_len as u32);
        put_u16(&mut buf, off + 20, 24); // value_off
        let content_off = off + 24;
        put_u64(&mut buf, content_off + 0x00, 0); // CreationTime
        put_u64(&mut buf, content_off + 0x08, modified_ft); // LastModificationTime
        put_u64(&mut buf, content_off + 0x10, 0);
        put_u64(&mut buf, content_off + 0x18, 0);
        put_u64(&mut buf, content_off + 0x20, real_size);
        put_u64(&mut buf, content_off + 0x28, real_size);
        put_u32(&mut buf, content_off + 0x30, attributes);
        put_u32(&mut buf, content_off + 0x34, 0);
        off += std_attr_total_len;

        // ── $FILE_NAME 属性 ─────────────────────────────────
        let name_u16: Vec<u16> = name.encode_utf16().collect();
        let name_bytes_len = name_u16.len() * 2;
        let fn_content_len: usize = 66 + name_bytes_len;
        let fn_attr_total_len: usize = (24 + fn_content_len + 7) & !7;
        put_u32(&mut buf, off, ATTR_FILE_NAME);
        put_u32(&mut buf, off + 4, fn_attr_total_len as u32);
        buf[off + 8] = 0;
        put_u32(&mut buf, off + 16, fn_content_len as u32);
        put_u16(&mut buf, off + 20, 24);
        let fn_content_off = off + 24;
        put_u64(&mut buf, fn_content_off + 0, parent_record);
        // 时间字段全 0
        for i in (8..64).step_by(8) {
            put_u64(&mut buf, fn_content_off + i, 0);
        }
        buf[fn_content_off + 64] = name_u16.len() as u8; // name_len
        buf[fn_content_off + 65] = 1; // ns = WIN32
        for (i, &c) in name_u16.iter().enumerate() {
            put_u16(&mut buf, fn_content_off + 66 + i * 2, c);
        }
        off += fn_attr_total_len;

        // ── $DATA 属性（resident） ──────────────────────────
        let data_attr_total_len: usize = 32;
        put_u32(&mut buf, off, ATTR_DATA);
        put_u32(&mut buf, off + 4, data_attr_total_len as u32);
        buf[off + 8] = 0; // resident
        buf[off + 9] = 0; // name_len = 0
        put_u32(&mut buf, off + 16, real_size as u32); // value_len
        put_u16(&mut buf, off + 20, 24);
        off += data_attr_total_len;

        // ── 属性结束标记 ───────────────────────────────────
        put_u32(&mut buf, off, ATTR_END);
        put_u32(&mut buf, off + 4, 0);
        off += 8;

        buf.truncate(off);
        while buf.len() < 512 {
            buf.push(0);
        }
        buf
    }

    #[test]
    fn test_parse_file_record() {
        let modified_ft: u64 = 132_000_000_000_000_000;
        let attributes: u32 = 0x20; // ARCHIVE
        let mut record = build_test_record("hello.txt", 42, false, 123_456, modified_ft, attributes);
        assert!(apply_fixup(&mut record, 512), "apply_fixup should succeed");
        let entry = parse_record(&record).expect("parse_record should return Some");
        assert_eq!(entry.name, "hello.txt");
        assert_eq!(entry.parent_record, 42);
        assert!(!entry.is_dir);
        assert!(entry.in_use);
        assert!(entry.is_base_record);
        assert_eq!(entry.real_size, 123_456);
        assert_eq!(entry.modified_ft, modified_ft);
        assert_eq!(entry.attributes, attributes);
    }

    #[test]
    fn test_parse_directory_record() {
        let modified_ft: u64 = 132_500_000_000_000_000;
        let attributes: u32 = 0x10; // DIRECTORY
        let mut record = build_test_record("MyFolder", 5, true, 0, modified_ft, attributes);
        assert!(apply_fixup(&mut record, 512));
        let entry = parse_record(&record).expect("parse_record should return Some");
        assert_eq!(entry.name, "MyFolder");
        assert_eq!(entry.parent_record, 5);
        assert!(entry.is_dir);
        assert_eq!(entry.attributes, 0x10);
    }

    #[test]
    fn test_parse_chinese_name() {
        let mut record = build_test_record("测试文件.txt", 7, false, 999, 0, 0x20);
        assert!(apply_fixup(&mut record, 512));
        let entry = parse_record(&record).expect("parse_record should return Some");
        assert_eq!(entry.name, "测试文件.txt");
        assert_eq!(entry.parent_record, 7);
    }

    #[test]
    fn test_parse_invalid_magic() {
        let mut buf = vec![0u8; 1024];
        buf[0..4].copy_from_slice(b"BAAD");
        let _ = apply_fixup(&mut buf, 512);
        assert!(parse_record(&buf).is_none());
    }

    #[test]
    fn test_parse_too_short() {
        let buf = vec![0u8; 10];
        assert!(parse_record(&buf).is_none());
    }

    #[test]
    fn test_apply_fixup_usn_mismatch() {
        let mut buf = vec![0u8; 1024];
        buf[0..4].copy_from_slice(b"FILE");
        put_u16(&mut buf, 4, 0x30);
        put_u16(&mut buf, 6, 2); // usa_count = 2（检查 1 个扇区）
        put_u16(&mut buf, 0x30, 0x1234); // USN
        put_u16(&mut buf, 510, 0x5678); // 扇区末尾不匹配
        assert!(!apply_fixup(&mut buf, 512));
    }

    #[test]
    fn test_apply_fixup_usn_match() {
        let mut buf = vec![0u8; 1024];
        buf[0..4].copy_from_slice(b"FILE");
        put_u16(&mut buf, 4, 0x30);
        put_u16(&mut buf, 6, 2);
        put_u16(&mut buf, 0x30, 0x1234); // USN
        put_u16(&mut buf, 510, 0x1234); // 扇区末尾匹配
        put_u16(&mut buf, 0x32, 0xABCD); // 原值
        assert!(apply_fixup(&mut buf, 512));
        let restored = u16::from_le_bytes([buf[510], buf[511]]);
        assert_eq!(restored, 0xABCD);
    }

    #[test]
    fn test_parse_multiple_records_no_loss() {
        let records_data = vec![
            ("file_a.txt", 5u64, false, 100u64, 132_000_000_000_000_000u64, 0x20u32),
            ("file_b.log", 5, false, 200, 132_100_000_000_000_000, 0x20),
            ("subdir", 5, true, 0, 132_200_000_000_000_000, 0x10),
            ("file_c.bin", 5, false, 300, 132_300_000_000_000_000, 0xA0),
        ];
        let mut parsed_count = 0;
        for (name, parent, is_dir, size, mft, attrs) in &records_data {
            let mut rec = build_test_record(name, *parent, *is_dir, *size, *mft, *attrs);
            assert!(apply_fixup(&mut rec, 512), "fixup failed for {}", name);
            let entry = parse_record(&rec).expect(&format!("parse failed for {}", name));
            assert_eq!(entry.name, *name);
            assert_eq!(entry.parent_record, *parent);
            assert_eq!(entry.is_dir, *is_dir);
            assert_eq!(entry.real_size, *size);
            assert_eq!(entry.modified_ft, *mft);
            assert_eq!(entry.attributes, *attrs);
            parsed_count += 1;
        }
        assert_eq!(parsed_count, records_data.len(), "应该解析出全部记录");
    }

    /// 测试文件大小 fallback：当 $DATA 属性不在 base record 里（大文件用扩展记录），
    /// 应该从 $FILE_NAME 的 RealSize 字段拿大小，而不是返回 0。
    #[test]
    fn test_real_size_fallback_from_file_name() {
        // 构造一个只有 $STANDARD_INFORMATION + $FILE_NAME（带 RealSize）但没有 $DATA 的记录
        // 模拟大文件的 base record（$DATA 在扩展记录里）
        let mut buf = vec![0u8; 1024];
        buf[0..4].copy_from_slice(b"FILE");
        put_u16(&mut buf, 4, 0x30); // usa_offset
        put_u16(&mut buf, 6, 1); // usa_count = 1
        put_u16(&mut buf, 0x30, 0x0001); // USN
        put_u16(&mut buf, 22, 0x01); // flags: in_use, not dir
        put_u16(&mut buf, 20, 0x38); // first_attr_offset
        put_u64(&mut buf, 32, 0); // base_record_ref = 0

        let mut off = 0x38usize;

        // $STANDARD_INFORMATION（简短）
        let std_len: usize = 0x48;
        let std_total: usize = 24 + std_len;
        put_u32(&mut buf, off, ATTR_STANDARD_INFORMATION);
        put_u32(&mut buf, off + 4, std_total as u32);
        buf[off + 8] = 0;
        put_u32(&mut buf, off + 16, std_len as u32);
        put_u16(&mut buf, off + 20, 24);
        off += std_total;

        // $FILE_NAME（带 RealSize = 214281216，模拟 Hermes.exe 204MB）
        let expected_size: u64 = 214_281_216;
        let name = "Hermes.exe";
        let name_u16: Vec<u16> = name.encode_utf16().collect();
        let name_bytes_len = name_u16.len() * 2;
        let fn_content_len: usize = 66 + name_bytes_len;
        let fn_total: usize = (24 + fn_content_len + 7) & !7;
        put_u32(&mut buf, off, ATTR_FILE_NAME);
        put_u32(&mut buf, off + 4, fn_total as u32);
        buf[off + 8] = 0;
        put_u32(&mut buf, off + 16, fn_content_len as u32);
        put_u16(&mut buf, off + 20, 24);
        let fn_off = off + 24;
        put_u64(&mut buf, fn_off + 0, 5); // parent = root (5)
        // 时间字段全 0
        // AllocatedSize at +40
        put_u64(&mut buf, fn_off + 40, expected_size);
        // RealSize at +48  ← 这是关键
        put_u64(&mut buf, fn_off + 48, expected_size);
        // Flags at +56
        put_u32(&mut buf, fn_off + 56, 0x20); // ARCHIVE
        put_u32(&mut buf, fn_off + 60, 0);
        buf[fn_off + 64] = name_u16.len() as u8;
        buf[fn_off + 65] = 1; // ns = WIN32
        for (i, &c) in name_u16.iter().enumerate() {
            put_u16(&mut buf, fn_off + 66 + i * 2, c);
        }
        off += fn_total;

        // 没有 $DATA 属性！直接放 ATTR_END
        put_u32(&mut buf, off, ATTR_END);
        put_u32(&mut buf, off + 4, 0);

        // apply_fixup + parse
        assert!(apply_fixup(&mut buf, 512));
        let entry = parse_record(&buf).expect("parse should succeed");
        assert_eq!(entry.name, "Hermes.exe");
        // 关键断言：real_size 应该从 $FILE_NAME 拿到，不是 0
        assert_eq!(
            entry.real_size, expected_size,
            "real_size 应该从 $FILE_NAME 的 RealSize fallback 拿到，而不是 0"
        );
    }

    /// 测试文件名优先级：WIN32 名应该胜过 DOS 8.3 短名。
    /// 模拟 "Program Files" 目录，同时有 ns=1 (WIN32) 和 ns=2 (DOS "PROGRA~1")。
    #[test]
    fn test_filename_prefers_win32_over_dos() {
        let mut buf = vec![0u8; 1024];
        buf[0..4].copy_from_slice(b"FILE");
        put_u16(&mut buf, 4, 0x30);
        put_u16(&mut buf, 6, 1);
        put_u16(&mut buf, 0x30, 0x0001);
        put_u16(&mut buf, 22, 0x03); // flags: in_use + directory
        put_u16(&mut buf, 20, 0x38);
        put_u64(&mut buf, 32, 0);

        let mut off = 0x38usize;

        // 先放 DOS 名 (ns=2, "PROGRA~1") — 故意放前面，测试优先级
        let dos_name = "PROGRA~1";
        let dos_u16: Vec<u16> = dos_name.encode_utf16().collect();
        let dos_bytes = dos_u16.len() * 2;
        let dos_content = 66 + dos_bytes;
        let dos_total = (24 + dos_content + 7) & !7;
        put_u32(&mut buf, off, ATTR_FILE_NAME);
        put_u32(&mut buf, off + 4, dos_total as u32);
        buf[off + 8] = 0;
        put_u32(&mut buf, off + 16, dos_content as u32);
        put_u16(&mut buf, off + 20, 24);
        let dos_off = off + 24;
        put_u64(&mut buf, dos_off + 0, 5);
        put_u64(&mut buf, dos_off + 48, 0);
        buf[dos_off + 64] = dos_u16.len() as u8;
        buf[dos_off + 65] = 2; // ns = DOS
        for (i, &c) in dos_u16.iter().enumerate() {
            put_u16(&mut buf, dos_off + 66 + i * 2, c);
        }
        off += dos_total;

        // 再放 WIN32 名 (ns=1, "Program Files")
        let win_name = "Program Files";
        let win_u16: Vec<u16> = win_name.encode_utf16().collect();
        let win_bytes = win_u16.len() * 2;
        let win_content = 66 + win_bytes;
        let win_total = (24 + win_content + 7) & !7;
        put_u32(&mut buf, off, ATTR_FILE_NAME);
        put_u32(&mut buf, off + 4, win_total as u32);
        buf[off + 8] = 0;
        put_u32(&mut buf, off + 16, win_content as u32);
        put_u16(&mut buf, off + 20, 24);
        let win_off = off + 24;
        put_u64(&mut buf, win_off + 0, 5);
        put_u64(&mut buf, win_off + 48, 0);
        buf[win_off + 64] = win_u16.len() as u8;
        buf[win_off + 65] = 1; // ns = WIN32
        for (i, &c) in win_u16.iter().enumerate() {
            put_u16(&mut buf, win_off + 66 + i * 2, c);
        }
        off += win_total;

        put_u32(&mut buf, off, ATTR_END);
        put_u32(&mut buf, off + 4, 0);

        assert!(apply_fixup(&mut buf, 512));
        let entry = parse_record(&buf).expect("parse should succeed");
        // 关键断言：应该选 WIN32 名 "Program Files"，不是 DOS 名 "PROGRA~1"
        assert_eq!(
            entry.name, "Program Files",
            "应该选 WIN32 长名，不是 DOS 8.3 短名 PROGRA~1"
        );
    }
}
