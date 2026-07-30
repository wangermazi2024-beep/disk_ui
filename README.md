# DiskLens

NTFS 磁盘空间分析器，使用 Rust + egui 构建。参考 [WinDirStat](https://github.com/windirstat/windirstat) 的 MFT 直读算法。

## 功能

- **MFT 直读扫描**（管理员模式）：直接读取 NTFS `$MFT`，秒级扫描整盘（参考 WinDirStat 的 `FinderNtfs`）
- **常规遍历扫描**（非管理员）：递归遍历目录树，自动 fallback
- **双大小显示**：Logical Size（逻辑大小）+ Physical Size（物理大小，含压缩/稀疏去重）
- **全量列**：名称、父占比、总占比、逻辑大小、修改时间、物理大小、创建时间、访问时间、项目/文件/文件夹数、属性、重解析点、保留、所有者
- **CSV 导出**：和列表列一致，UTF-8 BOM 编码
- **诊断日志**：双写 stderr + `disklens_log.txt`

## 实现

### MFT 直读（管理员模式）
1. 打开卷设备 `\\.\C:`（`FILE_READ_DATA | FILE_READ_ATTRIBUTES`，`FILE_FLAG_NO_BUFFERING`）
2. `FSCTL_GET_NTFS_VOLUME_DATA` 拿卷信息
3. 打开 `\\.\C:\$MFT::$DATA`（`FILE_READ_ATTRIBUTES`）+ `FSCTL_GET_RETRIEVAL_POINTERS` 拿 MFT 簇映射
4. 按 run 顺序读 MFT，每条记录做 USA fixup 后解析属性
5. 用两个哈希表聚合：`base_file_records`（record→属性）+ `parent_to_children`（parent→子项列表）
6. 从根目录（record 5）递归建树

### 大小计算
- Logical Size = `$DATA.FileSize`（attr+0x30），只在 `LowestVcn==0` 的 extent 有效
- Physical Size = `$DATA.AllocatedLength`（attr+0x28），压缩/稀疏文件用 `Compressed`（attr+0x40）
- 硬链接去重：Physical 只在第一个实例计入（和 WizTree 一致），Logical 不去重

### 常规遍历（非管理员 fallback）
- 递归 `std::fs::read_dir`，权限失败不中断
- 压缩/稀疏文件用 `GetCompressedFileSizeW` 获取物理大小
- 普通文件用簇对齐估算物理大小

## 编译

```powershell
cargo build --release
# target\release\disk_ui.exe
```

## 使用

1. 以管理员身份运行 `disk_ui.exe`（点"⚡ 以管理员运行"按钮自动提权）
2. 默认显示 C 盘，点"扫描"开始
3. 日志在 `disklens_log.txt`，CSV 导出在 `disklens_export.csv`
