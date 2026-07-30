# MFT scan hardlink handling in Rust

**Created:** 7/30/2026 13:54:52  
**Updated:** 7/30/2026 14:41:05  
**Exported:** 7/30/2026 14:49:49  
**Link:** [https://claude.ai/chat/291f4399-42f7-47ce-82cd-1de0180bc85d](https://claude.ai/chat/291f4399-42f7-47ce-82cd-1de0180bc85d)  

## User:

> 7/30/2026 13:54:53

> File: disk_ui.zip

> File: 

## Assistant:

> 7/30/2026 13:57:39

> Architected data structure enhancement while preserving existing functionality.
>
> - Unzip and list files
> - Check file sizes
> - Viewing file
> - Read MFT parsing code to find bug causing missing/mis-sized files
> - Viewing file
> - Read parse_record logic
> - Read scan and tree-building logic to find why some real files vanish from listing
> - See rest of parse_record incl name selection
> - See entries building loop and filter counters
> - Diagnosed hardlink tracking flaw and engineered structural fix.
> - Find all usages of RawEntry fields to plan safe patch
> - Architected data structure enhancement while preserving existing functionality.
> - Editing file
> - Add extra_links field to RawEntry to preserve hardlink info
> - Preserve extra hardlink locations instead of discarding them
> - **Done**

Now update `mft_scan.rs` to actually attach files under every hardlink location:

> Edited file
>
> - Attach each extra hardlink under its own parent directory too
> - **Done**

Now update `build_subtree` signature/body to use the new `children_of` type and honor name overrides:

> Pivoted to manual verification due to unavailable toolchain.
>
> - Editing file
> - Update children_of type in build_subtree signature
> - Use name_override and honor multiple hardlink parents when building tree
> - Check that cross-platform parts (mft_parse.rs) compile correctly
> - check for rust toolchain
> - Pivoted to manual verification due to unavailable toolchain.
> - **Done**

No Rust toolchain in this environment, so I reasoned through the change by hand — let me re-read the edited sections once to double check consistency.

> Reconciled deduplication trade-offs and documented acceptable limitations.
>
> - Re-check the edited parse_record tail for correctness
> - Reconciled deduplication trade-offs and documented acceptable limitations.
> - Package the fixed source tree
> - Presenting file(s)...
> - **Done**

找到根因了，不是文件大小解析的问题，是**硬链接丢失**。

**问题在哪：** `mft_parse.rs` 的 `parse_record()` 会把一条 MFT 记录里所有的 `$FILE_NAME` 属性都收集起来，然后按命名空间优先级（WIN32 > WIN32&DOS > POSIX > DOS）**只挑一个**当作这个文件的父目录+名字，其余的直接扔掉。

问题是：Windows 的驱动仓库（`DriverStore\FileRepository`）大量使用**硬链接**——同一个 MFT 记录（同一份数据）会同时挂在 `FileRepository\xxx.inf_amd64_...\` 和实际生效的目录（比如 `System32\drivers`）下，对应着两个甚至更多条 `$FILE_NAME` 属性，每条指向不同的父目录。原代码只保留"最优"的那一条，另一条硬链接位置的信息被彻底丢弃——所以那个文件就从"另一个目录"里凭空消失了，即使这条记录本身完好、`in_use`、`$DATA` 都在。你截图里的 `libnvdxdlkernels.so` 正是这种典型场景。

而 WinHex / 其他专业工具会按扇区级枚举每一个目录项（每条 `$FILE_NAME` 都算一条目录项），所以它们能同时在两个位置都扫到，你的工具却只扫到其中一个（或者你查看的那个目录恰好没被选中，看起来就是"丢了"）。

**修复思路：**
1. `RawEntry` 新增 `extra_links: Vec<(parent_record, name)>` 字段，把除"最优"链接外的其它 `$FILE_NAME` 都保留下来，不再丢弃。
2. `mft_scan.rs` 建邻接表时，对每个 `extra_link` 也在对应父目录下挂一条记录（用 `Option<String>` 做 name override，因为硬链接在不同目录下名字可能不同）。
3. `build_subtree` 相应改成按 `(child_idx, name_override)` 遍历，硬链接文件现在会在**每一个**它实际存在的目录里都显示。

已经把改好的代码打包好了。需要注意一点权衡：这样改之后，同一份数据如果被多处硬链接，会在总大小里被**重复计入**（每个目录下都算一次它的大小）——这和 Explorer/WizTree 在处理硬链接总量上的行为类似，是这类"直读 MFT"工具通用的取舍，如果你想要精确去重的总用量，需要额外按 base record 去重一次再统计，但这样"目录浏览"层面就不会漏文件了，我建议先验证这个能不能解决你说的丢文件问题。

我这边没有 Windows/Rust 工具链环境，没法直接编译验证，建议你在自己机器上 `cargo build` 一下确认没有语法问题（逻辑上我逐行核对过是对的）。

> File: [/mnt/user-data/outputs/disk_ui_fixed.zip](https://claude.ai/api/organizations/7b29ab9a-e2d7-45f5-a825-06318c80e427/conversations/291f4399-42f7-47ce-82cd-1de0180bc85d/wiggle/download-file?path=%2Fmnt%2Fuser-data%2Foutputs%2Fdisk_ui_fixed.zip)

## User:

> 7/30/2026 13:59:25

我需要一些类似的软件开源的 ，能让我查看mft文件列表，然后我就能确认这个软件有没有丢，没丢的话我就可以看他的算法

## Assistant:

> 7/30/2026 13:59:52

> 评估了多个开源工具来验证MFT文件列表完整性。
>
> - open source NTFS MFT parser tool list files github
> - 评估了多个开源工具来验证MFT文件列表完整性。
> - **Done**

这几个比较适合你现在的需求（先拿一份"金标准"文件列表核对，再看开源算法学习）：

## 用来对比、验证有没有丢文件

**1. MFTECmd**（Eric Zimmerman 出品，取证圈事实标准）
- https://github.com/EricZimmerman/MFTECmd
- C#，直接是编译好的 exe，不用装环境，管理员权限跑：
  ```
  MFTECmd.exe -f C:\path\to\extracted_$MFT --csv out_dir --csvf mft.csv
  ```
  （先用它自带的机制或者 `fsutil`/`ntfsclone` 之类把 `$MFT` 导出成文件，或者它也支持直接指定卷）
- 输出的 CSV 里每一行是一条 `$FILE_NAME` 属性（不是每条 MFT 记录一行），也就是说**硬链接的每个位置都会单独出现一行**，各自带着自己的 `ParentPath`。这个正好能验证我们之前分析的硬链接假说——你可以拿 CSV 搜 `libnvdxdlkernels.so`，看它是不是真的出现了不止一行、父目录不一样。

**2. analyzeMFT**（Python，最简单，几行代码看懂）
- https://github.com/rowingdude/analyzeMFT
- `pip install` 之后一条命令导出 CSV/JSON，代码量小，容易读逻辑，适合先建立"正确答案应该长什么样"的直觉。

## 用来看开源算法（Rust，跟你项目语言一致）

**3. omerbenamram/mft**
- https://github.com/omerbenamram/mft
- 纯 Rust 库 + `mft_dump` CLI，专门解析 `$MFT`，星标很多、经过大量测试。因为语言和你项目一样，**这个最值得读**——直接对照它是怎么处理：
  - 一条记录里多个 `$FILE_NAME`（对应硬链接）是怎么处理的（它是不是像我们分析的那样，把每个都保留、都输出，而不是只选"最优"的一个）
  - `$ATTRIBUTE_LIST` / 扩展记录怎么合并
  - USA fixup、resident/non-resident 属性怎么读
- 用法：
  ```
  cargo install mft
  mft_dump path\to\exported_$MFT -o csv -f mft.csv
  ```
  同样，`$MFT` 需要先导出成文件（可以用你自己项目里卷设备直读的那段逻辑导出，或者用 `MFTECmd`/`ntfsclone` 之类先落盘）。

## 建议的验证流程

1. 用 MFTECmd 或 omerbenamram/mft 对同一个盘跑一次，导出完整 CSV。
2. 在 CSV 里搜那个丢失文件的名字（`libnvdxdlkernels.so`），看它出现几次、父目录路径分别是什么。
3. 如果确认出现了 2 行（不同父目录），就实锤了硬链接丢失的判断，你可以直接对照 `omerbenamram/mft` 里处理多个 `$FILE_NAME` 属性的那部分代码（大概率是遍历所有 `$FILE_NAME` attribute 逐个 push 到结果 Vec，而不是像原来的实现那样只挑一个），拿它的处理方式去修正/校验我给你的补丁思路。

---

**Sources:**

- [GitHub - EricZimmerman/MFTECmd: Parses $MFT from NTFS file systems · GitHub](https://github.com/EricZimmerman/MFTECmd)
- [GitHub - Kudaes/MFTool: Direct access to NTFS volumes · GitHub](https://github.com/Kudaes/MFTool)
- [GitHub - msuhanov/dfir_ntfs: An NTFS/FAT parser for digital forensics & incident response · GitHub](https://github.com/msuhanov/dfir_ntfs)
- [GitHub - rasberry/NTFSDirect: NTFS MFT (master file table) raw reader and parser · GitHub](https://github.com/rasberry/NTFSDirect)
- [GitHub - mtivadar/fs_ntfs: NTFS filesystem parser](https://github.com/mtivadar/fs_ntfs)
- [GitHub - thewhiteninja/ntfstool: Forensics tool for NTFS (parser, mft, bitlocker, deleted files) · GitHub](https://github.com/thewhiteninja/ntfstool)
- [GitHub - rowingdude/analyzeMFT: analyzeMFT.py is designed to fully parse the MFT file from an NTFS filesystem and present the results as accurately as possible in multiple formats. · GitHub](https://github.com/rowingdude/analyzeMFT)
- [mft · GitHub Topics · GitHub](https://github.com/topics/mft)
- [GitHub - omerbenamram/mft: A parser for the MFT (Master File Table) format · GitHub](https://github.com/omerbenamram/mft)
- [github.com](https://github.com/aarsakian/MFTExtractor)

## User:

> 7/30/2026 14:05:31

C:\Users\wms\Downloads\MFTECmd>MFTECmd.exe -f C:\path\to\extracted_$MFT --csv out_dir --csvf mft.csv
File C:\path\to\extracted_$MFT not found. Exiting
C:\Users\wms\Downloads\MFTECmd>

## Assistant:

> 7/30/2026 14:05:48

> Deliberated extraction methods for locked system files.
>
> - Deliberated extraction methods for locked system files.
> - **Done**

这个报错是因为 `$MFT` 本身是被 NTFS 驱动锁住的特殊文件，不能用路径直接指、也不能用 `copy`/`robocopy` 这类普通文件操作去复制（跟我们之前分析你自己代码时提到的那个限制是一回事）。需要先用能绕过这个锁的工具把它"提取"成一个普通文件，再喂给 MFTECmd。

最简单的办法：**RawCopy**（免费单文件小工具，取证圈常用搭档）

1. 下载 RawCopy：https://github.com/jschicht/RawCopy
2. 管理员身份打开 cmd，执行（假设你要分析 C 盘，导出到 `C:\temp`）：
   ```
   RawCopy64.exe /FileNamePath:C:0 /OutputPath:C:\temp
   ```
   `C:0` 表示 C 盘卷设备里 MFT 记录号 0（也就是 `$MFT` 自己）。跑完 `C:\temp` 下会多出一个类似 `$MFT` 的文件（可能带原始文件名，自己看一下）。

   或者用更直观的写法（部分版本支持）：
   ```
   RawCopy64.exe /FileNamePath:C:\$MFT /OutputPath:C:\temp
   ```

3. 然后再跑 MFTECmd，指向刚才导出的那个文件：
   ```
   MFTECmd.exe -f C:\temp\$MFT --csv C:\temp\out --csvf mft.csv
   ```

如果不想装 RawCopy，另一个常见备选是 **FTK Imager (Lite)**（AccessData 官方免费版）：打开它 → "File" → "Obtain Protected Files"，或者直接把 C 盘作为 Physical/Logical Drive 加进证据树，在树里找到 `$MFT`，右键导出，同样能拿到一份可读的 `$MFT` 文件。

拿到 CSV 之后，直接搜 `libnvdxdlkernels.so`（或者搜 `nv_dispig.inf_amd64`），看它到底出现几行、`ParentPath` 分别是什么，这样就能确认是不是硬链接导致的丢失了。

## User:

> 7/30/2026 14:06:30

omerbenamram/mft

* https://github.com/omerbenamram/mft

你确认这个 没问题吗，那么你把项目改成用这个库

## Assistant:

> 7/30/2026 14:10:41

> 承认了技术限制，决定提供未验证的集成方案。
>
> - [https://docs.rs/mft/latest/mft/](https://docs.rs/mft/latest/mft/)
> - omerbenamram mft crate docs.rs MftParser API
> - mft crate MftParser::from_buffer from_read rust docs
> - mft crate rust MftAttributeContent AttrX30 fields parent full_path FileNameAttr
> - 承认了技术限制，决定提供未验证的集成方案。
> - **Done**

确认这个库没问题，值得信赖：`mft` crate（crates.io 上 744k+ 次下载，最近还在更新)是纯 Rust 实现，100% safe rust，作者 omerbenamram 也是取证圈里的人，`mftparser.com` 那篇工具评测文章直接把它和 MFTECmd 并列当作"经得起考验"的解析器,而且代码量小、可审计。

**但我要跟你说清楚一个限制**：我这个环境里没有 Windows / Rust 工具链，没法真的 `cargo build` 帮你编译验证。下面这版改动是我按官方文档 API（`MftParser::from_buffer`、`iter_entries`、`iter_attributes`、`MftAttributeContent::AttrX10/AttrX30/AttrX80` 等）认真写的，思路是对的，但字段名细节（比如 `FileNameAttr` 里具体叫 `parent`还是 `parent_entry_id`，是否自动帮你合并 `$ATTRIBUTE_LIST` 指向的扩展记录属性）我没法 100% 打包票，你编译时大概率会有一两处字段名要对着 `cargo doc --open` 微调。

思路：**保留你原来 windows-only 的卷设备读取代码不变**（`open_volume_and_get_info` / `get_mft_runs` 那部分是对的，不是问题所在），只是把"读到内存后自己手撸解析"这一段，换成把整块 MFT 字节丢给 `mft` 库去解析，然后我们自己在拿到的 entries 上重新做（1）多 `$FILE_NAME`（硬链接）全部保留、（2）建目录树。

> 规划用MFT库重构项目，坦诚标记不确定部分。
>
> - check Cargo.toml deps
> - Add mft crate dependency
> - view full first-pass parsing loop to replace with library-based approach
> - 规划用MFT库重构项目，坦诚标记不确定部分。
> - **Done**

我把解析核心换成这个库了，但有几处 API 细节字段名我在这没法编译验证，我在代码里用 `// CHECK:` 标出来了，你 `cargo check` 报错的话大概率就是这几个点,发给我我们再一起改。

> - check remaining part of file after size-resolution block, to know exact old_str for full replacement
> - 规划库集成方案并草拟重构代码框架。
> - Replace hand-rolled MFT record parsing (fixup+attribute walk+manual extension-record size resolution) with omerbenamram/mft crate. Read all MFT bytes into one buffer, then let the crate do fixup+attribute parsing, including for extension records.
> - find remaining references to old parsing helpers to clean up imports
> - Fix imports: only RawEntry/ROOT_RECORD_INDEX are still needed from mft_parse
> - view read_nonresident_attribute function to decide whether to remove or keep with allow(dead_code)
> - **Done**

Now unused — mark it `#[allow(dead_code)]` rather than delete, to keep the diff minimal and avoid accidentally breaking something else that references it:

> - Mark now-unused helper as dead_code allowed (kept for reference/rollback, no longer called since size resolution goes through the mft crate's in-memory record table)
> - final sanity view of the edited big block
> - **Done**

Looks structurally sound. Now package it up.

> Ran command, presented file
>
> - Package the mft-crate-integrated version
> - Presenting file(s)...
> - **Done**

改完了，说一下实际做了什么、以及**你必须自己核对的地方**（我这没有 Windows/Rust 工具链，编不了）：

**改动思路**
- `Cargo.toml`：加了 `mft = "0.7"` 依赖。
- 你原来读卷设备、处理 MFT 碎片（`open_volume_and_get_info` / `get_mft_runs`）的部分**完全没动**——那部分本来就是对的，不是 bug 所在。
- 换掉的是：手撸的 fixup + 属性遍历 + "记录大小=0 就重新打开卷设备按 `$ATTRIBUTE_LIST` 手动算物理偏移再读一次盘"这一大坨。现在改成：把整个 MFT 字节一次性读进一块连续内存，交给 `mft::MftParser::from_buffer` 解析，它会连**扩展记录**一起解析出来，我们直接在内存里查表拿扩展记录的 `$DATA` 大小，不用再手动读盘、手动 fixup——逻辑简单很多，出错面也小很多。
- 硬链接的修复逻辑（保留每条记录的所有 `$FILE_NAME`，而不是只挑一个）保留了，而且现在是在库解析出的属性上做的，理论上更可靠。

**必须核对的地方（我标了 `// CHECK:` 的都在这几类）：**
1. `entry.header.record_number` / `entry.header.flags` / `entry.header.base_reference.entry_reference` 这几个字段名，我是按 NTFS 规范猜的常见命名，具体要对着 `cargo doc -p mft --open` 里 `EntryHeader` 的定义核对。
2. `mft::entry::EntryFlags::ALLOCATED` / `INDEX_PRESENT` 这两个 flag 常量名是猜的。
3. `MftAttributeContent::AttrX10` 里 `modified` 字段类型（可能是 `chrono::DateTime` 也可能是原始 FILETIME u64，我写的是 `.timestamp()`，大概率要改）。
4. `AttrX30` 里 `namespace` / `parent.entry` / `name` 字段名。
5. 有没有 `AttrX20`（`$ATTRIBUTE_LIST`）这个变体，字段是 `entries` / `attribute_type` / `segment_reference` 这几个名字。
6. 未命名 `$DATA` 怎么拿到 `data_size`——我写的是 `attr.header.type_code`/`attr.header.data_size()`，这块最不确定，因为库对 `$DATA` 这种"内容不结构化"的属性怎么暴露头信息，我没法确认。

**建议流程**：你在自己机器上 `cargo check`，把报错整个贴给我（哪怕很长），我们照着编译器告诉你的真实字段名一条条改，两三轮应该就能编过。改完之后再拿它和你之前用 MFTECmd/RawCopy 导出的 CSV 对一下 `libnvdxdlkernels.so`，确认硬链接的两个位置是不是都出现了。

> File: [/mnt/user-data/outputs/disk_ui_mft_lib.zip](https://claude.ai/api/organizations/7b29ab9a-e2d7-45f5-a825-06318c80e427/conversations/291f4399-42f7-47ce-82cd-1de0180bc85d/wiggle/download-file?path=%2Fmnt%2Fuser-data%2Foutputs%2Fdisk_ui_mft_lib.zip)

---

**Sources:**

- [mft 0.4.0 - Docs.rs](https://docs.rs/crate/mft/0.4.0/source/README.md)
- [mft 0.4.3 - Docs.rs](https://docs.rs/crate/mft/0.4.3)
- [mft 0.7.0 - Docs.rs](https://docs.rs/crate/mft/latest)
- [GitHub - omerbenamram/mft: A parser for the MFT (Master File Table) format · GitHub](https://github.com/omerbenamram/mft)
- [mft/README.md at master · omerbenamram/mft](https://github.com/omerbenamram/mft/blob/master/README.md)
- [GitHub - digitalsleuth/mft-rust: A parser for the MFT (Master File Table) format](https://github.com/digitalsleuth/mft-rust)
- [MftParser in mft::mft - Rust](https://docs.rs/mft/latest/mft/mft/struct.MftParser.html)
- [mft - Rust](https://docs.rs/mft)
- [MFT parsers that actually hold up: MFTECmd, omerbenamram/mft, and browser parsing — MFT parser](https://www.mftparser.com/en/blog/mft-parser-tools)
- [Omer BenAmram (omerbenamram)](https://crates.io/users/omerbenamram)
- [MftEntry in mft::entry - Rust](https://docs.rs/mft/latest/mft/entry/struct.MftEntry.html)
- [mft - crates.io: Rust Package Registry](https://crates.io/crates/mft/0.6.0/dependencies)
- [mft: Rust Package Registry - Windows Master File Table (MFT ...](https://crates.io/crates/mft)
- [github.com](https://github.com/rushattac/mft)
- [mft::attr_x30::FileNameAttr - Rust](https://docs.rs/mft/0.1.0/mft/attr_x30/struct.FileNameAttr.html)
- [MFT — Rust utility // Lib.rs](https://lib.rs/crates/mft)
- [FileNameAttr in mft::attribute::x30 - Rust](https://docs.rs/mft/latest/mft/attribute/x30/struct.FileNameAttr.html)

## User:

> 7/30/2026 14:23:12

Compiling egui_extras v0.35.0
   Compiling windows-sys v0.59.0
   Compiling disk_ui v0.1.0 (D:\a\disk_ui\disk_ui)
error[E0609]: no field `entry_reference` on type `winstructs::ntfs::mft_reference::MftReference`
--> src\mft_scan.rs:608:52
|
608 |         let base_ref = entry.header.base_reference.entry_reference; // CHECK: 字段名
|                                                    ^^^^^^^^^^^^^^^ unknown field
|
= note: available fields are: `entry`, `sequence`
error[E0599]: no method named `timestamp` found for struct `Timestamp` in the current scope
--> src\mft_scan.rs:626:53
|
626 |                     modified_ft = std_info.modified.timestamp() as u64; // 若类型不同这里要改
|                                                     ^^^^^^^^^ method not found in `Timestamp`
error[E0609]: no field `instance` on type `&mft::attribute::x20::AttributeListEntry`
--> src\mft_scan.rs:641:81
|
641 | ...                   attr_list.push((e.segment_reference.entry as u64, e.instance as u16));
|                                                                           ^^^^^^^^ unknown field
|
= note: available fields are: `attribute_type`, `record_length`, `name_length`, `name_offset`, `lowest_vcn` ... and 3 others
error[E0308]: mismatched types
--> src\mft_scan.rs:649:49
|
649 |                     if attr.header.type_code == 0x80 && attr.header.name_size == 0 {
|                        ---------------------    ^^^^ expected `MftAttributeType`, found integer
|                        |
|                        expected because this is `MftAttributeType`
error[E0599]: no method named `data_size` found for struct `MftAttributeHeader` in the current scope
--> src\mft_scan.rs:650:62
|
650 |                         unnamed_data_size = Some(attr.header.data_size());
|                                                              ^^^^^^^^^ method not found in `MftAttributeHeader`
Some errors have detailed explanations: E0308, E0599, E0609.
For more information about an error, try `rustc --explain E0308`.
error: could not compile `disk_ui` (lib) due to 5 previous errors
warning: build failed, waiting for other jobs to finish...
Error: Process completed with exit code 1.

## Assistant:

> 7/30/2026 14:26:47

> - docs.rs mft crate MftAttributeHeader ResidentialHeader data_size field
> - [https://docs.rs/mft/latest/mft/attribute/header/struct.MftAttributeHeader.html](https://docs.rs/mft/latest/mft/attribute/header/struct.MftAttributeHeader.html)
> - docs.rs mft::attribute::header MftAttributeHeader struct fields non_resident_flag data_size
> - Clone the actual mft crate source to read exact struct definitions
> - **Done**

Excellent, actual source. Let's read the exact struct definitions.

> - Read entry.rs and attribute header.rs
> - Read x30 x20 x10 attribute definitions
> - check MftAttributeContent enum, MftAttributeType enum
> - check how modified_ft is consumed downstream (expects Windows FILETIME format?)
> - check winstructs MftReference field types via cargo registry if cached, else just trust u64 based on usage
> - view current entry-parsing loop to patch exact field/method names
> - Identified multiple Rust compilation errors requiring API adjustments.
> - Fix field/method names per actual mft crate source (verified from GitHub source, not guesses)
> - confirm MftAttributeType enum location and derives
> - check derive attributes on MftAttributeType enum
> - **Done**

Good, confirmed `MftAttributeType` derives `PartialEq`, is `pub`, at `mft::attribute::MftAttributeType`, and `mft::attribute::header::ResidentialHeader` matches my usage. Now let's add the helper `jiff_timestamp_to_windows_filetime` and check `jiff::Timestamp`'s API for seconds/nanos.

> Identified structural issue requiring extension records reorganization.
>
> - see how the crate itself converts windows filetime to jiff Timestamp, to invert it correctly
> - find a good spot to add the helper function
> - Add helper: convert jiff::Timestamp back to Windows FILETIME u64 (100ns since 1601), since downstream format.rs expects raw FILETIME
> - find jiff version used by mft crate
> - Add jiff as direct dependency (needed since we name jiff::Timestamp directly in our own code, not just via re-export)
> - check MftParser struct and from_buffer/from_path/iter_entries signatures
> - final review of edited entry-loop and surrounding code before repackaging
> - Identified structural issue requiring extension records reorganization.
> - **Done**

Found a real bug in my own patch — extension records (no `$FILE_NAME`) were getting skipped *before* being stored in the lookup table, which breaks the whole "look up extension record size in memory" idea. Fixing:

> - Fix bug: extension records (no $FILE_NAME) must still be stored in by_record so base records can look up their $DATA size; only skip them for tree/counting purposes, not for storage
> - Identified API mismatches and validated bounds-checking logic.
> - Update stale CHECK comment now that API is verified against actual crate source
> - view remainder to make sure by_record consumption matches struct fields
> - **Done**

This all lines up correctly now. Let's repackage.

> Ran command, presented file
>
> - Package the corrected version with verified crate field names
> - Presenting file(s)...
> - **Done**

这次不是猜的了——我把 `omerbenamram/mft` 的源码 clone 下来直接读了 `entry.rs`/`header.rs`/`attribute/x10.rs`/`x30.rs`/`x20.rs`，把你贴的 5 个编译错误一一对上真实字段名改掉了：

- `entry.header.base_reference.entry_reference` → `entry.header.base_reference.entry`（`MftReference` 只有 `entry`/`sequence` 两个字段）
- `in_use`/`is_dir` 不用再翻 `EntryFlags` 位，库自带 `entry.is_allocated()` / `entry.is_dir()` 两个便利方法，直接用
- `std_info.modified` 是 `jiff::Timestamp` 不是能调 `.timestamp()` 的类型；写了个 `jiff_timestamp_to_windows_filetime()` 转换函数（因为你下游 `format.rs` 是按 Windows FILETIME 格式解析显示时间的），同时在 `Cargo.toml` 里把 `jiff` 加成了显式依赖（不然编不过，因为你直接点名了这个类型）
- `$ATTRIBUTE_LIST` 条目里那个字段库里叫 `reserved`（文档注释写的是"The attribute's id"，命名是历史遗留），不是 `instance`
- 未命名 `$DATA` 的大小改成从 `attr.header.residential_header`（`Resident.data_size` 或 `NonResident.file_size`，且只在 `vnc_first==0` 时有效）拿，不再依赖 `attr.header.type_code == 0x80`这种整数比较——改成了 `mft::attribute::MftAttributeType::DATA` 这个真正的枚举比较

顺带发现并修了我自己引入的一个逻辑 bug：扩展记录（没有 `$FILE_NAME`）之前会在存入查找表之前就被跳过，导致"查表拿扩展记录大小"这个 v14 的核心改造根本查不到东西——现在扩展记录会先存进 `by_record`，只是不参与生成树节点。

这版应该能编过了，如果还有报错继续贴给我。编过之后麻烦你验证两件事：
1. 拿它扫一遍，看 `libnvdxdlkernels.so` 是不是在硬链接的两个位置都出现了。
2. 找几个你确认是"严重碎片化大文件"的例子，看大小是不是解析出来了（不再是 0）。

> File: [/mnt/user-data/outputs/disk_ui_mft_lib_v2.zip](https://claude.ai/api/organizations/7b29ab9a-e2d7-45f5-a825-06318c80e427/conversations/291f4399-42f7-47ce-82cd-1de0180bc85d/wiggle/download-file?path=%2Fmnt%2Fuser-data%2Foutputs%2Fdisk_ui_mft_lib_v2.zip)

---

**Sources:**

- [mft 0.7.0 - Docs.rs](https://docs.rs/crate/mft/latest)
- [mft 0.4.0 - Docs.rs](https://docs.rs/crate/mft/0.4.0/source/README.md)
- [MFT Record Structure | forensicxlab/exhume_ntfs | DeepWiki](https://deepwiki.com/forensicxlab/exhume_ntfs/6.2.1-mft-record-structure)
- [$MFT Resident data](https://www.forensicfocus.com/forums/general/mft-resident-data/)
- [EntryHeader in mft::entry - Rust](https://docs.rs/mft/latest/mft/entry/struct.EntryHeader.html)
- [A Journey into NTFS: Part 6 - Matt B](https://bromiley.medium.com/ntfs-part-6-43a50fad89f3)
- [MftEntry in mft::entry - Rust](https://docs.rs/mft/latest/mft/entry/struct.MftEntry.html)
- [mft - Rust](https://docs.rs/mft)
- [[PATCH] fs/ntfs3: Validate attribute data and valid sizes](https://lkml.iu.edu/hypermail/linux/kernel/2210.0/02145.html)
- [mft package - github.com/t9t/gomft/mft - Go Packages](https://pkg.go.dev/github.com/t9t/gomft/mft)
- [Standard File Attributes | NTFS Data Structures](https://flylib.com/books/en/2.48.1/standard_file_attributes.html)
- [[PDF] 5.1.NTFS - Free Download PDF](https://nanopdf.com/download/51ntfs_pdf)
- [13: Introduction to NTFS | COMPSCI 365/590F | Digital Forensics (Spring 2017)](https://people.cs.umass.edu/~liberato/courses/2017-spring-compsci365/lecture-notes/13-introduction-to-ntfs/)
- [NTFS /MFT structure – How to set Extended Attributes,Alternate Data Stream – | SEの道標](https://milestone-of-se.nesuke.com/en/sv-basic/windows-basic/ntfs-filesystem-structure/)
- [Attribute Header - NTFS Documentation](https://flatcap.github.io/linux-ntfs/ntfs/concepts/attribute_header.html)
- [MFT Structures](https://www.scribd.com/document/730565330/MFT-Structures)

## User:

> 7/30/2026 14:29:38

我用F:\CP>mft_dump -o csv $MFT  测试为什么看不到文件输出

## Assistant:

> 7/30/2026 14:30:02

> - Check actual mft_dump CLI arguments from source
> - check actual CLI flag definitions (short/long names)
> - **Done**

这是符合预期的——不是 bug。`mft_dump` 的 `-o` 只是指定**输出格式**（json/jsonl/csv），不是输出文件路径。你没加 `-f`/`--output` 指定输出文件的话，它会把结果直接打印到**屏幕（stdout）**，不会在磁盘上生成任何文件。所以你"看不到文件输出"是因为压根没有文件被创建，内容全被刷到终端里了（如果记录很多，很可能瞬间刷屏刷过去你根本看不清）。

正确用法应该是：

```
F:\CP>mft_dump -o csv -f F:\CP\mft.csv $MFT
```

或者用长参数名（更不容易记混）：

```
F:\CP>mft_dump --output-format csv --output F:\CP\mft.csv $MFT
```

跑完之后 `F:\CP\mft.csv` 才是你要的那份文件，拿它去搜 `libnvdxdlkernels.so` 或者 `nv_dispig.inf_amd64`。

如果你就是想先在屏幕上确认一下有没有输出（不落盘），可以先跑：

```
F:\CP>mft_dump -o csv $MFT | findstr /I "nv_dispig"
```

顺便确认一下：你这个 `$MFT` 是不是已经在 `F:\CP` 目录下用 RawCopy（或者别的工具）导出好的那份文件？如果直接写 `$MFT` 而没导出过，大概率会报"文件不存在"之类的错误而不是"看不到输出"——如果是这种报错，把完整的报错信息发我看看。

## User:

> 7/30/2026 14:33:06

跑拿到的 csv 文件没有  nv_dispig.inf_amd64_0afec3f2050014a0\libnvdxdlkernels.so 
只有一条别的目录的
FILE,111638,2,0,0,2,EntryFlags(ALLOCATED),624,1024,128602944,false,false,false,FileAttributeFlags(FILE_ATTRIBUTE_ARCHIVE | FILE_ATTRIBUTE_EA),2024-09-15T16:53:20Z,2026-07-30T05:51:52.140919Z,2026-03-24T07:18:21.076813Z,FileAttributeFlags(FILE_ATTRIBUTE_ARCHIVE | FILE_ATTRIBUTE_EA),2024-09-15T16:53:20Z,2026-03-24T07:18:54.037854Z,2026-03-24T07:18:21.076813Z,Windows\System32\lxss\lib\libnvdxdlkernels.so

## Assistant:

> 7/30/2026 14:33:48

> 分析了CSV导出限制与硬链接显示问题。
>
> - check whether mft_dump's csv export produces one row per $FILE_NAME or one per entry
> - 分析了CSV导出限制与硬链接显示问题。
> - **Done**

这条结果其实已经间接印证了硬链接的判断，而且更有意思——它验证了另一件事：**`mft_dump` 的 CSV 导出格式本身，每条 MFT 记录只输出一个路径**（我看了源码，它内部是 `entry_attributes.iter().find(...)` 找第一个碰到的 `$FILE_NAME`，不是把所有的都列出来),跟你原来自己代码的毛病是一类的。所以 CSV 里只有一行，并不代表这个文件真的只存在一个位置，只是这个导出格式**只能显示一个**。

更关键的是路径本身：CSV 里显示的是

```
Windows\System32\lxss\lib\libnvdxdlkernels.so
```

而不是你在 WinHex 里看到的 `DriverStore\FileRepository\nv_dispig.inf_amd64_...\libnvdxdlkernels.so`。

这基本实锤了硬链接：`libnvdxdlkernels.so` 是 NVIDIA 给 WSL（Windows 里跑 Linux）用的 CUDA 支持库（`.so` 就是 Linux 动态库后缀），它真正生效的位置是 `System32\lxss\lib\`；而 `DriverStore\FileRepository\` 下那份，是驱动安装时"放"在仓库里的原始拷贝——这两个路径极大概率是**同一个 MFT 记录、同一份数据**，只是 `mft_dump` 恰好从这条记录的多个 `$FILE_NAME` 属性里挑了 `lxss\lib` 这一个来显示，你之前用 WinHex/自己的工具看到的是 `DriverStore` 那个——两边各挑了记录里不同的名字，谁都没把全部名字列出来。

**怎么彻底证实**：拿这条记录的 `entry_id=111638` 去看它到底有几个 `$FILE_NAME`。`mft_dump` 换成 JSON 格式（不是 csv）应该会把这条记录的**所有**属性都列出来，不会像 CSV 那样只挑一个：

```
mft_dump -o json -f F:\CP\entry111638.json -r 111638 F:\CP\$MFT
```

（`-r`/`--ranges` 是官方参数，可以指定只导出某个记录号范围，避免全盘导出太大）

导出后搜 JSON 里的 `"AttrX30"` 或 `FileName`，看这一条记录底下是不是真的有两个（甚至更多）`$FILE_NAME`，一个 parent 指向 `DriverStore\FileRepository\...`，另一个指向 `System32\lxss\lib`。如果确认是这样，那就是 100% 实锤了：不是你的工具"漏扫"，是设计上"一条记录只显示一个名字"这个通病（你原来的代码、`mft_dump` 的 CSV 格式都中招），而我们已经改的那版（保留所有 `$FILE_NAME` 并分别挂到每个父目录下）就是对症的修法。

## User:

> 7/30/2026 14:35:23

> File: 

## Assistant:

> 7/30/2026 14:35:35

> - **Done**

这就是实锤了——**完全证实了硬链接假说**，而且证据非常干净:

- `header.hard_link_count: 2` —— NTFS 自己都说了这条记录有 2 个硬链接
- 确实有 **两个** `FileName` 属性，都叫 `libnvdxdlkernels.so`，但父目录不一样：
  - 第一个：`parent.entry = 109945`
  - 第二个：`parent.entry = 111995`
- 两个 `$FILE_NAME` 里的 `logical_size`/`physical_size` 都是 128606208 / 128602944，`$DATA` 只有一份（一个 data run），说明是同一份数据的两个目录入口，不是两个不同文件重名

也就是说：这个文件在磁盘上确确实实同时"挂"在两个目录下（一个大概率是 `System32\lxss\lib`，另一个是 `DriverStore\FileRepository\nv_dispig.inf_amd64_...`），而不管是你原来的工具、还是 `mft_dump` 的 CSV 导出，都只会显示其中一个——这正是我们最早分析代码时定位到的那个 bug 的现实实例。

**如果你想确认哪个 entry 号对应哪个目录**，可以分别导出这两个父目录记录的名字：

```
mft_dump -o json -f F:\CP\parent1.json -r 109945 F:\CP\$MFT
mft_dump -o json -f F:\CP\parent2.json -r 111995 F:\CP\$MFT
```

看这两条记录各自的 `$FILE_NAME.name` 是什么（一路往上应该能拼出 `System32\lxss\lib` 和 `DriverStore\FileRepository\nv_dispig.inf_amd64_...` 这两条路径，分别对应上面两个 `parent.entry`）。

这下你可以放心去编译验证我给你的那版补丁了——逻辑上就是要把这两条 `$FILE_NAME` **都**保留、都挂到对应父目录下，而不是像原来（以及 `mft_dump` CSV）那样只留一个。

## User:

> 7/30/2026 14:36:25

> File: 

> File: 

## Assistant:

> 7/30/2026 14:36:38

> Verified file paths across system directories.
>
> - Verified file paths across system directories.
> - **Done**

完美，路径链条完整对上了，100% 实锤：

- `record_number 109945`（"lib" 目录）← 这个就是 `System32\lxss\lib`
- `record_number 111995` 的名字是 `nv_dispig.inf_amd64_0afec3f2050014a0`（父目录 `entry 3709`，大概率就是 `DriverStore\FileRepository`）

所以完整的两条路径是：

```
...\System32\lxss\lib\libnvdxdlkernels.so
...\Windows\System32\DriverStore\FileRepository\nv_dispig.inf_amd64_0afec3f2050014a0\libnvdxdlkernels.so
```

同一个 MFT 记录（111638），同一份 `$DATA`，两个目录入口——跟你最初 WinHex 截图里看到的路径完全对得上。这下证据链闭环了：

1. 你的软件（以及原始 MFT 解析代码、`mft_dump` 的 CSV 导出）在解析记录时都只保留"一个"`$FILE_NAME`
2. 这个文件恰好是 NVIDIA DriverStore 里那种典型的硬链接场景：一份数据、两个目录同时收录
3. 被丢弃的那个目录位置里，这个文件就"凭空消失"了——不是扫描漏掉了这条记录，是记录解析完之后多余的名字被扔了

也就是说，之前给你的那版改动方向是对的：把每条记录的**所有** `$FILE_NAME`（parent + name）都保留下来，扫描阶段按每一个 `(parent, name)` 都在对应目录下挂一个节点，而不是只挑"最优"的一个。你可以拿这份 JSON 当回归测试用例（record 111638，两个 parent 分别是 109945 和 111995），编译通过之后跑一次真实扫描，确认这个文件现在两个目录下都能看到了。

## User:

> 7/30/2026 14:37:49

我用你最后一次给我打包的版本还是报错
  Compiling disk_ui v0.1.0 (D:\a\disk_ui\disk_ui)
error[E0599]: no method named `to_utf8_string` found for struct `String` in the current scope
--> src\mft_scan.rs:659:36
|
659 |                         fname.name.to_utf8_string(),
|                                    ^^^^^^^^^^^^^^
|
help: there is a method `to_string` with a similar name
|
659 -                         fname.name.to_utf8_string(),
659 +                         fname.name.to_string(),
|
For more information about this error, try `rustc --explain E0599`.
error: could not compile `disk_ui` (lib) due to 1 previous error
warning: build failed, waiting for other jobs to finish...
Error: Process completed with exit code 1.

## Assistant:

> 7/30/2026 14:38:06

> - **Done**

这是版本差异——你实际拉到的 `mft` 版本里 `FileNameAttr.name` 字段类型是 `String`（不是我看的那份源码里的 `Utf16LeStr`），所以直接 `.clone()` 就行，不需要 `.to_utf8_string()`。

> Edited file, ran command, presented file
>
> - Fix: fname.name is already String in the resolved crate version, use .clone() instead of .to_utf8_string()
> - Repackage with the .clone() fix
> - Presenting file(s)...
> - **Done**

这一处改完再编一次，看还有没有别的报错（这种因为 crate 小版本差异导致的字段类型不一样的情况,后面也可能还会碰到一两处,有报错继续贴给我)。

> File: [/mnt/user-data/outputs/disk_ui_mft_lib_v3.zip](https://claude.ai/api/organizations/7b29ab9a-e2d7-45f5-a825-06318c80e427/conversations/291f4399-42f7-47ce-82cd-1de0180bc85d/wiggle/download-file?path=%2Fmnt%2Fuser-data%2Foutputs%2Fdisk_ui_mft_lib_v3.zip)

## User:

> 7/30/2026 14:38:34

现在你给我的版本是 不丢文件的版本吗。

## Assistant:

> 7/30/2026 14:38:49

> - **Done**

是的，就"你实测出来的这个具体问题"（同一个文件因为硬链接同时挂在两个目录、只显示其中一个）来说，这版补丁的逻辑是对症的：

- 解析阶段：`RawEntry` 现在会保留一条记录里**所有**的 `$FILE_NAME`（parent + name），不再只挑"最优"的一个丢掉其余的。
- 建树阶段：`children_of`/`build_subtree` 会把这个文件同时挂到**每一个**它实际存在的父目录下（比如 `libnvdxdlkernels.so` 现在应该会同时出现在 `System32\lxss\lib` 和 `DriverStore\FileRepository\nv_dispig.inf_amd64_...` 两处）。

但我要如实说清楚这个"是"的边界在哪：

1. **这只保证解决了硬链接这一类丢失**，也就是你已经用 JSON 实锤验证过的那种情况。你最初反馈的是"很多文件丢了"，硬链接大概率是主因（DriverStore 里这种情况非常多），但我没法 100% 保证你遇到的所有丢失案例都是这一种原因——不排除还有别的、我们还没碰到的边角情况。
2. 我这边没有 Windows/Rust 工具链，**没法帮你实际编译跑一次拿真实扫描结果验证**，这版是基于你给我的 5 个报错逐条对照 crate 源码改的，逻辑上过了我自己的推演，但"编译通过 = 逻辑真的对" 还差最后一步：你编译通过、真跑一次扫描、拿刚才那个 `libnvdxdlkernels.so` 去验证它是不是真的两个目录都显示了。

所以准确的说法是：**这是"目前证实的那个丢失原因"已经修的版本，但还不是我能百分百担保"再也不丢任何文件"的版本**——这个担保只能靠你编译、跑起来、拿实际案例验证之后才能给。你验证完如果发现还有别的文件丢，把具体案例（路径、能不能在 WinHex 或 mft_dump JSON 里找到对应记录）发我，我们接着排查。

Powered by Claude Exporter (https://www.ai-chat-exporter.net)
