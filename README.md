# CSV Splitter Pro

跨平台 CSV 文件拆分、压缩、预览与统计分析工具，支持中英文界面。

Cross-platform CSV file splitter, compressor, previewer and column statistics tool with Chinese/English UI.

## 功能 / Features

- **拆分 & 压缩** — 将大型 CSV 拆分为多个分片并压缩（Zstd / Gzip / Bzip2 / XZ / Zip），支持不拆分直接压缩
- **列筛选** — 通过列过滤文件或直接从 CSV 表头选择需要的列（支持搜索）
- **排序** — 按指定列进行外部归并排序，支持数值智能排序
- **数据预览** — 快速预览 CSV 前 100 行，自动检测分隔符
- **列统计** — 通过蓄水池采样分析各列数据分布（唯一值、空值、Top 频率）
- **解压缩** — 支持 Zstd、Gzip、Bzip2、XZ、Zip、7z、Tar 及其组合格式
- **中英文 i18n** — 默认中文，一键切换英文
- **拖放支持** — 直接拖放文件到卡片区域，显示文件元数据
- **流程可视化** — 源文件 → 拆分 → 压缩 → 结果 的动画流程图
- **自适应布局** — 窗口宽度自适应，紧凑/宽屏两种布局

## 截图 / Screenshots

```
┌─────────────────────────────────────────────────┐
│ 🔪 拆分压缩 │ 👁 数据预览 │ 📊 列统计 │ 📦 解压缩 │ 🌐 EN │
├─────────────────────────────────────────────────┤
│  [源文件] → [拆分] → [压缩] → [结果]           │
│  [列过滤文件]                                    │
│  [列筛选: ☑ col1 ☑ col2 ☐ col3 ...]            │
│  ⚙️ 参数: 份数 / 缓冲 / 压缩格式 / 排序列      │
│  🚀 开始处理                                     │
└─────────────────────────────────────────────────┘
```

## 构建 / Build

需要 Rust 1.75+。

```bash
# 开发构建
cargo build

# Release 构建（启用 LTO 优化）
cargo build --release

# 运行
cargo run --release -p csv-splitter-gui
```

### 平台依赖

| 平台 | 额外依赖 |
|------|---------|
| macOS | 无（系统自带中文字体） |
| Windows | 无（使用微软雅黑） |
| Linux | 需要安装 CJK 字体包，如 `noto-fonts-cjk`，以及 GTK3 开发库用于文件对话框 |

Linux 安装依赖示例：
```bash
# Ubuntu/Debian
sudo apt install fonts-noto-cjk libgtk-3-dev

# Arch
sudo pacman -S noto-fonts-cjk gtk3

# Fedora
sudo dnf install google-noto-sans-cjk-fonts gtk3-devel
```

## 项目结构 / Project Structure

```
├── Cargo.toml              # Workspace 根配置
├── crates/
│   ├── core/               # 核心库（无 GUI 依赖）
│   │   └── src/
│   │       ├── types.rs    # 类型定义（SplitConfig, ProgressEvent 等）
│   │       ├── csv_reader.rs  # CSV 解析、表头读取、分隔符检测
│   │       ├── splitter.rs # 拆分 + 列过滤 + 压缩管线
│   │       ├── compress.rs # 压缩器工厂（Zstd/Gzip/Bzip2/XZ）
│   │       ├── decompress.rs # 解压缩（支持 12 种格式组合）
│   │       ├── preview.rs  # 数据预览 + 列统计（蓄水池采样）
│   │       └── sort.rs     # 外部归并排序
│   └── gui/                # GUI 应用（egui/eframe）
│       └── src/
│           ├── main.rs     # 入口
│           ├── app.rs      # 主应用框架 + 拖放处理
│           ├── state.rs    # 各 Tab 状态定义
│           ├── theme.rs    # 暗色主题 + CJK 字体加载
│           ├── widgets.rs  # 共享组件（文件卡片、分隔符选择等）
│           ├── i18n.rs     # 中英文国际化
│           └── tabs/       # 各功能页面
│               ├── split.rs     # 拆分压缩页
│               ├── flow.rs      # 流程可视化 + 列选择器
│               ├── preview.rs   # 数据预览页
│               ├── stats.rs     # 列统计页
│               └── decompress.rs # 解压缩页
```

## 支持的压缩格式

| 格式 | 压缩 | 解压 |
|------|:----:|:----:|
| Zstandard (.zst) | ✅ | ✅ |
| Gzip (.gz) | ✅ | ✅ |
| Bzip2 (.bz2) | ✅ | ✅ |
| XZ/LZMA (.xz) | ✅ | ✅ |
| Zip (.zip) | ✅ | ✅ |
| 7-Zip (.7z) | — | ✅ |
| Tar (.tar) | ✅ | ✅ |
| Tar+Gzip (.tar.gz/.tgz) | — | ✅ |
| Tar+Zstd (.tar.zst) | — | ✅ |
| Tar+Bzip2 (.tar.bz2) | — | ✅ |
| Tar+XZ (.tar.xz) | — | ✅ |

## License

MIT
