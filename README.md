# DiskForge WMS

由 WMS 开发的 NTFS 磁盘空间分析器，使用 Rust + egui 构建。

## 功能

- **启动即选盘**：打开程序先弹出分区/目录选择界面，支持多选分区、也支持手动添加自定义目录；
  在点"开始扫描"之前不查询任何分区的容量数据——所有数据都是扫描之后才产生的
- **MFT 直读扫描**（管理员模式）：直接读取 NTFS `$MFT`，秒级扫描整盘
- **常规遍历扫描**（非管理员）：批量枚举目录项，自动 fallback 到逐条读取
- **双大小显示**：Logical Size（逻辑大小）+ Physical Size（物理大小，含压缩/稀疏/硬链接去重）
- **顺序批量扫描**：一次选多个分区/目录会排队依次扫描，扫完一个自动接着扫下一个
- **视图 > 显示全部信息**：开启时显示全部列和 NTFS 元数据文件；关闭时只留关键列、隐藏元数据文件
- **隐藏/系统文件提示**：带 Hidden/System 属性的文件会淡化显示并标注"(隐藏)"，不会被过滤掉
- **CSV 导出**：每个已扫描的分区/目录各自导出一份，UTF-8 BOM 编码
- **诊断日志**：双写 stderr + `diskforge_log.txt`（超过 5MB 自动重新开始，不会无限增长）

## 实现要点

### MFT 直读（管理员模式）
1. 打开卷设备 `\\.\X:`（`FILE_READ_DATA | FILE_READ_ATTRIBUTES`，`FILE_FLAG_NO_BUFFERING`）
2. `FSCTL_GET_NTFS_VOLUME_DATA` 拿卷信息（含真实簇大小）
3. 打开 `\\.\X:\$MFT::$DATA` + `FSCTL_GET_RETRIEVAL_POINTERS` 拿 MFT 簇映射
4. 按 run 顺序读 MFT，每条记录做 USA fixup 后解析属性（所有裸字节解析都带越界检查，
   不会因为损坏/畸形记录而崩溃）
5. 用两个哈希表聚合：`base_file_records`（record→属性）+ `parent_to_children`（parent→子项列表）
6. 从根目录（record 5）迭代式建树（显式栈，非原生递归，目录树多深都不会栈溢出）

### 大小计算
- Logical Size = `$DATA.FileSize`，只在 `LowestVcn==0` 的 extent 有效
- Physical Size = `$DATA.AllocatedLength`，压缩/稀疏文件用 `Compressed`
- 硬链接去重：Physical 只在第一个实例计入，Logical 不去重
- fallback 路径的簇对齐用 `GetDiskFreeSpaceW` 查到的真实簇大小，不是固定按 4096 算

### 常规遍历（非管理员 fallback）
- 批量枚举（`GetFileInformationByHandleEx` + `FileIdBothDirectoryInfo`），每个目录只开一次 handle
- 共享工作队列 + 固定数量 worker 线程处理并发遍历（无原生递归，栈深度和目录深度无关）
- 批量枚举失败时逐条 `std::fs::read_dir` fallback，权限失败不中断

## 编译

```powershell
cargo build --release
# target\release\disk_ui.exe
```

## 使用

1. 启动后先在弹出的界面里选择要扫描的分区（可多选）和/或添加自定义目录
2. 点"开始扫描"，多个目标会排队依次扫描
3. 非管理员运行只能常规遍历；菜单栏"工具 > 以管理员身份重启"可以自动提权切换到 MFT 直读模式
4. 日志在 `diskforge_log.txt`，CSV 导出在程序所在目录（`diskforge_export_<序号>_<名称>.csv`）
