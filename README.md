# CSV Splitter Pro

> Cross-platform CSV splitter, compressor, previewer & column statistics tool with bilingual (Chinese/English) UI.
>
> 跨平台 CSV 文件拆分、压缩、预览与列统计工具，支持中英文界面。

[![CI](https://github.com/user/csv-splitter-pro/actions/workflows/ci.yml/badge.svg)](https://github.com/user/csv-splitter-pro/actions/workflows/ci.yml)
[![Release](https://github.com/user/csv-splitter-pro/actions/workflows/release.yml/badge.svg)](https://github.com/user/csv-splitter-pro/actions/workflows/release.yml)
![Version](https://img.shields.io/badge/version-0.1-blue)

---

## Features / 功能

| Feature | Description | 说明 |
|---------|-------------|------|
| Split & Compress | Split large CSV into compressed chunks (Zstd/Gzip/Bzip2/XZ/Zip) | 将大型 CSV 拆分为多个压缩分片 |
| Toggle Pipeline | Independently enable/disable split and compress steps | 可独立开关拆分与压缩步骤 |
| Column Filter | Select columns from CSV headers or via filter file | 通过表头选择或过滤文件筛选列 |
| Sort | External merge sort by column with numeric-aware ordering | 按指定列外部归并排序，支持数值智能排序 |
| Data Preview | Preview first 100 rows with auto-detected delimiter | 快速预览前 100 行，自动检测分隔符 |
| Column Stats | Reservoir sampling analysis (unique/empty/top frequency) | 蓄水池采样分析列数据分布 |
| Decompress | Supports Zstd, Gzip, Bzip2, XZ, Zip, 7z, Tar and combos | 支持 12 种压缩格式及其组合 |
| i18n | Chinese (default) / English, one-click switch | 默认中文，一键切换英文 |
| Drag & Drop | Drop files onto cards, shows file metadata | 拖放文件到卡片区域，显示文件元数据 |
| Flow Visualization | Animated pipeline: Source → Split → Compress → Result | 动画流程图，动态显示活跃步骤 |
| Adaptive Layout | Responsive layout with 960px max content width | 自适应布局，最大内容宽度 960px |

## Screenshots / 截图

```
┌──────────────────────────────────────────────────────┐
│ 🔪 Split  │ 👁 Preview  │ 📊 Stats  │ 📦 Extract │ 🌐 │
├──────────────────────────────────────────────────────┤
│  ✅ Split   ✅ Compress                              │
│  [Source] → [Split] → [Compress] → [Result]         │
│  [Column filter file]                                │
│  [Columns: ☑ col1 ☑ col2 ☐ col3 ...]               │
│  ⚙️ Params: splits / buffer / format / sort          │
│  🚀 Start                                            │
└──────────────────────────────────────────────────────┘
```

## Supported Formats / 支持的压缩格式

| Format | Compress | Decompress |
|--------|:--------:|:----------:|
| Zstandard (.zst) | ✅ | ✅ |
| Gzip (.gz) | ✅ | ✅ |
| Bzip2 (.bz2) | ✅ | ✅ |
| XZ/LZMA (.xz) | ✅ | ✅ |
| Zip (.zip) | ✅ | ✅ |
| 7-Zip (.7z) | — | ✅ |
| Tar (.tar) | ✅ | ✅ |
| Tar+Gzip (.tar.gz) | — | ✅ |
| Tar+Zstd (.tar.zst) | — | ✅ |
| Tar+Bzip2 (.tar.bz2) | — | ✅ |
| Tar+XZ (.tar.xz) | — | ✅ |

## Build / 构建

Requires Rust 1.75+.

```bash
# Dev build
cargo build

# Release build (with LTO)
cargo build --release

# Run
cargo run --release -p csv-splitter-gui
```

### Platform Dependencies / 平台依赖

| Platform | Dependencies |
|----------|-------------|
| macOS | None (system CJK fonts) |
| Windows | None (Microsoft YaHei) |
| Linux | CJK font package + GTK3 dev libs |

Linux setup:

```bash
# Ubuntu / Debian
sudo apt install fonts-noto-cjk libgtk-3-dev

# Arch
sudo pacman -S noto-fonts-cjk gtk3

# Fedora
sudo dnf install google-noto-sans-cjk-fonts gtk3-devel
```

## Project Structure / 项目结构

```
├── Cargo.toml                 # Workspace root
├── crates/
│   ├── core/                  # Core library (no GUI dependency)
│   │   └── src/
│   │       ├── types.rs       # SplitConfig, ProgressEvent, etc.
│   │       ├── csv_reader.rs  # CSV parsing, header reading, delimiter detection
│   │       ├── splitter.rs    # Split + column filter + compress pipeline
│   │       ├── compress.rs    # Compressor factory (Zstd/Gzip/Bzip2/XZ)
│   │       ├── decompress.rs  # Decompression (12 format combos)
│   │       ├── preview.rs     # Data preview + column stats (reservoir sampling)
│   │       └── sort.rs        # External merge sort
│   └── gui/                   # GUI application (egui / eframe)
│       └── src/
│           ├── main.rs        # Entry point
│           ├── app.rs         # App framework + drag-drop handling
│           ├── state.rs       # Per-tab state definitions
│           ├── theme.rs       # Dark theme + CJK font loading
│           ├── i18n.rs        # Chinese / English i18n
│           ├── widgets/       # Shared UI components
│           │   ├── mod.rs     # Colors, format_bytes, delimiter helper
│           │   ├── dialog.rs  # Async file dialog (thread + mpsc)
│           │   ├── file_card.rs # File input card + dir path card
│           │   └── common.rs  # section_frame, log_panel, arrows, etc.
│           └── tabs/          # Feature pages
│               ├── split.rs       # Split & compress page
│               ├── split_job.rs   # Split job execution + polling
│               ├── flow.rs        # Pipeline visualization + column selector
│               ├── flow_cards.rs  # Flow step card widgets
│               ├── preview.rs     # Data preview page
│               ├── stats.rs       # Column statistics page
│               └── decompress.rs  # Decompression page
```

## CI/CD

Builds are triggered automatically on every push to `main` and on pull requests.

Release builds for all platforms are triggered on git tag push (`v*`) or manual dispatch, producing downloadable artifacts:

| Platform | Artifact |
|----------|----------|
| Linux x86_64 | `csv-splitter-linux-x86_64.tar.gz` |
| macOS ARM64 | `csv-splitter-macos-arm64.tar.gz` |
| macOS x86_64 | `csv-splitter-macos-x86_64.tar.gz` |
| Windows x86_64 | `csv-splitter-windows-x86_64.zip` |

To create a release:

```bash
git tag v0.1.0
git push origin v0.1.0
```

## License

MIT
