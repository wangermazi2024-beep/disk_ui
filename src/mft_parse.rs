//! 纯 MFT 记录解析逻辑，**无 `cfg(windows)` 限制**。
//!
//! 这个模块从 `mft_scan.rs` 抽出来，目的有两个：
//! 1. 让解析逻辑可以在 Linux 上跑单元测试（用合成的 MFT 字节流喂进去），
//!    验证字段提取是否正确、是否丢属性、Fixup 算法是否对。
//! 2. 让 `mft_scan.rs` 只剩 Windows 专有的 I/O 代码（CreateFileW / DeviceIoControl），
//!    职责更清晰。
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
/// 不是有效的 FILE 记录（magic 不对 / 长度不够 / 没有 $FILE_NAME 属性）。
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
    let mut name: Option<String> = None;
    let mut best_ns = 255u8; // 越小优先级越高，见下方选择逻辑
    let mut real_size: u64 = 0;
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
            if content_start + value_len <= record.len() && value_len >= 66 {
                let c = &record[content_start..content_start + value_len];
                let parent_ref = u64::from_le_bytes(c[0..8].try_into().unwrap());
                let name_len_chars = c[64] as usize;
                let ns = c[65]; // 0=POSIX 1=WIN32 2=DOS(8.3短名) 3=WIN32&DOS
                let name_bytes_len = name_len_chars * 2;
                if 66 + name_bytes_len <= c.len() {
                    // 优先取 WIN32(1) 或 POSIX(0) 名字；纯 DOS 短名(2) 只在没有更好选择时用。
                    let priority = match ns {
                        1 | 0 | 3 => 0,
                        _ => 1,
                    };
                    if priority < best_ns {
                        best_ns = priority;
                        let u16s: Vec<u16> = c[66..66 + name_bytes_len]
                            .chunks_exact(2)
                            .map(|b| u16::from_le_bytes([b[0], b[1]]))
                            .collect();
                        name = Some(
                            os_string_from_wide(&u16s)
                                .to_string_lossy()
                                .into_owned(),
                        );
                        parent_record = parent_ref & 0x0000_FFFF_FFFF_FFFF; // 低 48 位是记录号
                        // $FILE_NAME 属性里也有一个 real_size（allocation size + real size），
                        // 但 $DATA(0x80) 里的更准确，这里不覆盖。
                    }
                }
            }
        } else if attr_type == ATTR_DATA {
            // 未命名的 $DATA 才代表文件主体大小；命名的是备用数据流(ADS)，跳过。
            let name_len = record[off + 9];
            if name_len == 0 {
                real_size = if non_resident {
                    u64::from_le_bytes(record[off + 48..off + 56].try_into().unwrap())
                } else {
                    u32::from_le_bytes(record[off + 16..off + 20].try_into().unwrap()) as u64
                };
            }
        }

        off += attr_len;
    }

    let name = name?;
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

// ─────────────────────────────────────────────────────────────────────────
// 单元测试：用合成的 MFT 字节流验证解析逻辑，**Linux 上也能跑**。
// ─────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    /// 把一个 u64 写到 buf 的指定偏移上（小端）。
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

    /// 构造一个最小的 MFT FILE 记录字节流：
    /// - "FILE" magic
    /// - usa_offset / usa_count 指向一个空 USA（count=1，只有 USN 本身，没有扇区要修正）
    /// - flags = in_use | is_dir
    /// - 三个 resident 属性：$STANDARD_INFORMATION / $FILE_NAME / $DATA
    /// - 属性结束标记 0xFFFFFFFF
    ///
    /// 这样构造出来的记录可以跑通 apply_fixup（USA count=1 时不进循环）
    /// 和 parse_record（能拿到全部字段）。
    fn build_test_record(
        name: &str,
        parent_record: u64,
        is_dir: bool,
        real_size: u64,
        modified_ft: u64,
        attributes: u32,
    ) -> Vec<u8> {
        // 1024 字节的记录是 NTFS 默认大小，足够装下我们这几个属性。
        let mut buf = vec![0u8; 1024];

        // ── FILE 头 ──────────────────────────────────────────
        buf[0..4].copy_from_slice(b"FILE");
        // usa_offset = 0x30（FILE 头之后的标准位置）
        // usa_count = 1（只有 USN 本身，不修正任何扇区 → apply_fixup 不会进循环）
        put_u16(&mut buf, 4, 0x30); // usa_offset
        put_u16(&mut buf, 6, 1); // usa_count
        // 写一个 USN 值（随便填）
        put_u16(&mut buf, 0x30, 0x0001);
        // flags: in_use(0x01) + is_dir(0x02 if dir)
        let flags: u16 = 0x01 | if is_dir { 0x02 } else { 0x00 };
        put_u16(&mut buf, 22, flags);
        // first_attr_offset = 0x38（USA 之后）
        put_u16(&mut buf, 20, 0x38);
        // base_record_ref = 0（这是 base record 本身）
        put_u64(&mut buf, 32, 0);

        let mut off = 0x38usize;

        // ── $STANDARD_INFORMATION 属性 ─────────────────────
        let std_attr_content_len: usize = 0x48; // 72 字节，标准长度
        let std_attr_total_len: usize = 24 /*resident 头*/ + std_attr_content_len;
        put_u32(&mut buf, off, ATTR_STANDARD_INFORMATION); // attr_type
        put_u32(&mut buf, off + 4, std_attr_total_len as u32); // attr_len
        buf[off + 8] = 0; // non_resident = 0
        put_u32(&mut buf, off + 16, std_attr_content_len as u32); // value_len
        put_u16(&mut buf, off + 20, 24); // value_off
        // 内容：CreationTime / LastModificationTime / LastMftChangeTime / LastAccessTime
        let content_off = off + 24;
        put_u64(&mut buf, content_off + 0x00, 0); // CreationTime
        put_u64(&mut buf, content_off + 0x08, modified_ft); // LastModificationTime
        put_u64(&mut buf, content_off + 0x10, 0); // LastMftChangeTime
        put_u64(&mut buf, content_off + 0x18, 0); // LastAccessTime
        put_u64(&mut buf, content_off + 0x20, real_size); // AllocatedSize
        put_u64(&mut buf, content_off + 0x28, real_size); // RealSize
        put_u32(&mut buf, content_off + 0x30, attributes); // Flags
        put_u32(&mut buf, content_off + 0x34, 0); // UsedSize
        off += std_attr_total_len;

        // ── $FILE_NAME 属性 ─────────────────────────────────
        // 内容布局：
        //   +0x00 ParentReference (8B)
        //   +0x08 CreationTime (8B)
        //   +0x10 ModificationTime (8B)
        //   +0x18 MftChangeTime (8B)
        //   +0x20 AccessTime (8B)
        //   +0x28 AllocatedSize (8B)
        //   +0x30 RealSize (8B)
        //   +0x38 Flags (4B)
        //   +0x3C ReparseValue (4B)
        //   +0x40 NameNamespace (1B)
        //   +0x41 NameLengthChars (1B)
        //   ... 实际是 +0x40 name_ns, +0x41 name_len (NTFS 文档里稍有出入，
        //       我们以代码里 parse_record 的偏移为准：c[64]=name_len, c[65]=ns)
        //   +0x42 ... padding to align
        //   +0x44 ... 不对，parse_record 用的是 c[64] 和 c[65]，
        //       所以 name_len 在 +0x40，ns 在 +0x41。
        //   等等，让我再看一下 parse_record：
        //     let name_len_chars = c[64] as usize;
        //     let ns = c[65];
        //   所以 64=name_len, 65=ns。
        //   名字从 c[66] 开始，每字符 2 字节 UTF-16LE。
        let name_u16: Vec<u16> = name.encode_utf16().collect();
        let name_bytes_len = name_u16.len() * 2;
        let fn_content_len: usize = 66 + name_bytes_len;
        // resident 属性总长度需要 8 字节对齐
        let fn_attr_total_len: usize = (24 + fn_content_len + 7) & !7;
        put_u32(&mut buf, off, ATTR_FILE_NAME); // attr_type
        put_u32(&mut buf, off + 4, fn_attr_total_len as u32); // attr_len
        buf[off + 8] = 0; // non_resident = 0
        put_u32(&mut buf, off + 16, fn_content_len as u32); // value_len
        put_u16(&mut buf, off + 20, 24); // value_off
        let fn_content_off = off + 24;
        put_u64(&mut buf, fn_content_off + 0, parent_record); // ParentReference
        // 时间字段全部填 0
        put_u64(&mut buf, fn_content_off + 8, 0);
        put_u64(&mut buf, fn_content_off + 16, 0);
        put_u64(&mut buf, fn_content_off + 24, 0);
        put_u64(&mut buf, fn_content_off + 32, 0);
        put_u64(&mut buf, fn_content_off + 40, 0);
        put_u64(&mut buf, fn_content_off + 48, 0);
        // flags / reparse
        put_u32(&mut buf, fn_content_off + 56, 0);
        put_u32(&mut buf, fn_content_off + 60, 0);
        // name_len (1B) at offset 64
        buf[fn_content_off + 64] = name_u16.len() as u8;
        // name_namespace (1B) at offset 65: 1 = WIN32
        buf[fn_content_off + 65] = 1;
        // 名字 UTF-16LE 从 offset 66 开始
        for (i, &c) in name_u16.iter().enumerate() {
            put_u16(&mut buf, fn_content_off + 66 + i * 2, c);
        }
        off += fn_attr_total_len;

        // ── $DATA 属性（只有文件才有，文件夹也常有但 real_size=0） ──
        // resident $DATA（小文件）：内容长度 = real_size，但我们只关心 header 里的 real_size。
        // 对于非驻留（大文件），real_size 在 off+48。这里用 resident 模拟。
        let data_attr_total_len: usize = 32; // 24 头 + 8 字节内容（够装下 real_size 字段位置）
        put_u32(&mut buf, off, ATTR_DATA); // attr_type
        put_u32(&mut buf, off + 4, data_attr_total_len as u32); // attr_len
        buf[off + 8] = 0; // non_resident = 0
        buf[off + 9] = 0; // name_len = 0（未命名 $DATA）
        put_u32(&mut buf, off + 16, real_size as u32); // value_len = real_size (resident)
        put_u16(&mut buf, off + 20, 24); // value_off
        off += data_attr_total_len;

        // ── 属性结束标记 ───────────────────────────────────
        put_u32(&mut buf, off, ATTR_END);
        put_u32(&mut buf, off + 4, 0);
        off += 8;

        // 截断到实际使用的长度（保持 8 字节对齐）
        buf.truncate(off);
        // 补齐到至少 512 字节（apply_fixup 在 sector_end > len 时会 break，不会出错）
        while buf.len() < 512 {
            buf.push(0);
        }
        buf
    }

    #[test]
    fn test_parse_file_record() {
        // 构造一个文件记录：name="hello.txt", parent=42, real_size=123456
        let modified_ft: u64 = 132_000_000_000_000_000; // 某个 FILETIME
        let attributes: u32 = 0x20; // ARCHIVE
        let mut record = build_test_record("hello.txt", 42, false, 123_456, modified_ft, attributes);

        // apply_fixup 应该通过（USA count=1 不进循环）
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
        // 中文名字（UTF-16 多字节）
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
        // apply_fixup 可能通过（USA count 检查），但 parse_record 应返回 None
        let _ = apply_fixup(&mut buf, 512);
        assert!(parse_record(&buf).is_none(), "non-FILE magic should return None");
    }

    #[test]
    fn test_parse_too_short() {
        let buf = vec![0u8; 10];
        assert!(parse_record(&buf).is_none(), "too-short record should return None");
    }

    #[test]
    fn test_apply_fixup_usn_mismatch() {
        // 构造一个 record，让 USA 检查失败
        let mut buf = vec![0u8; 1024];
        buf[0..4].copy_from_slice(b"FILE");
        // usa_offset = 0x30, usa_count = 2（要检查 1 个扇区）
        put_u16(&mut buf, 4, 0x30);
        put_u16(&mut buf, 6, 2);
        // USN 值 = 0x1234
        put_u16(&mut buf, 0x30, 0x1234);
        // 扇区 512 末尾的 2 字节 = 0x5678（不匹配 USN）
        put_u16(&mut buf, 510, 0x5678);
        // apply_fixup 应失败
        assert!(!apply_fixup(&mut buf, 512), "USN mismatch should fail fixup");
    }

    #[test]
    fn test_apply_fixup_usn_match() {
        // 构造一个 record，让 USA 检查通过
        let mut buf = vec![0u8; 1024];
        buf[0..4].copy_from_slice(b"FILE");
        put_u16(&mut buf, 4, 0x30);
        put_u16(&mut buf, 6, 2);
        // USN 值 = 0x1234
        put_u16(&mut buf, 0x30, 0x1234);
        // 扇区 512 末尾的 2 字节也填 0x1234（匹配）
        put_u16(&mut buf, 510, 0x1234);
        // 原 2 字节放在 USA 数组的第 2 项（offset 0x32）
        put_u16(&mut buf, 0x32, 0xABCD);
        assert!(apply_fixup(&mut buf, 512), "USN match should pass fixup");
        // 修正后，扇区末尾的 2 字节应该被替换成原值
        let restored = u16::from_le_bytes([buf[510], buf[511]]);
        assert_eq!(restored, 0xABCD, "fixup should restore original bytes");
    }

    /// 解析多条记录，验证不丢字段（模拟一个迷你的 MFT）。
    #[test]
    fn test_parse_multiple_records_no_loss() {
        let records_data = vec![
            ("file_a.txt", 5u64, false, 100u64, 132_000_000_000_000_000u64, 0x20u32),
            ("file_b.log", 5, false, 200, 132_100_000_000_000_000, 0x20),
            ("subdir", 5, true, 0, 132_200_000_000_000_000, 0x10),
            ("file_c.bin", 5, false, 300, 132_300_000_000_000_000, 0xA0), // ARCHIVE|NORMAL
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
        assert_eq!(parsed_count, records_data.len(), "应该解析出全部记录，一个都不能丢");
    }
}
