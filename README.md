# CSV Splitter Pro

> Cross-platform CSV splitter, compressor, previewer & column statistics tool with bilingual (Chinese/English) UI.
>
> 跨平台 CSV 文件拆分、压缩、预览与列统计工具，支持中英文界面。

[![CI](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/ci.yml/badge.svg)](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/ci.yml)
[![Release](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/release.yml/badge.svg)](https://github.com/Li-Binzhuang/csv-splitter/actions/workflows/release.yml)
[![GitHub Release](https://img.shields.io/github/v/release/Li-Binzhuang/csv-splitter?include_prereleases&sort=semver)](https://github.com/Li-Binzhuang/csv-splitter/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

---

## Download / 下载

从 [Releases](https://github.com/Li-Binzhuang/csv-splitter/releases/latest) 页面下载对应平台的预编译包：

| Platform | Artifact | Arch |
|----------|----------|------|
| 🐧 Linux | `csv-splitter-linux-x86_64.tar.gz` | x86_64 |
| 🍎 macOS | `csv-splitter-macos-arm64.tar.gz` | Apple Silicon |
| 🍎 macOS | `csv-splitter-macos-x86_64.tar.gz` | Intel |
| 🪟 Windows | `csv-splitter-windows-x86_64.zip` | x86_64 |

> 每个包附带 `.sha256` 校验文件，可用于验证下载完整性。

## Features / 功能

| Feature | Description | 说明 |
|---------|-------------|------|
| 🔪 Split & Compress | Split large CSV into compressed chunks (Zstd/Gzip/Bzip2/XZ/Zip) | 将大型 CSV 拆分为多个压缩分片 |
| 🔄 Toggle Pipeline | Independently enable/disable split and compress steps | 可独立开关拆分与压缩步骤 |
| 📋 Column Filter | Select columns from CSV headers or via filter file | 通过表头选择或过滤文件筛选列 |
| 🔃 Sort | External merge sort by column with numeric-aware ordering | 按指定列外部归并排序，支持数值智能排序 |
| 👁 Data Preview | Preview CSV rows with auto-detected delimiter, row/col range & freeze | 预览 CSV 数据，支持行列范围与冻结 |
| 📊 Column Stats | Reservoir sampling analysis (unique/empty/top frequency) | 蓄水池采样分析列数据分布 |
| 📦 Decompress | Supports Zstd, Gzip, Bzip2, XZ, Zip, 7z, Tar and combos | 支持 12 种压缩格式及其组合 |
| 🌐 i18n | Chinese (default) / English, one-click switch | 默认中文，一键切换英文 |
| 📁 Drag & Drop | Drop files onto cards, shows file metadata | 拖放文件到卡片区域，显示文件元数据 |
| 🎞 Flow Visualization | Animated pipeline: Source → Split → Compress → Result | 动画流程图，动态显示活跃步骤 |
| 🧩 Merge | Re-assemble chunks back into one CSV (inverse of split) | 把分片重新合并回单个 CSV（拆分的逆操作） |
| ✅ Verify | Split → merge round-trip check that proves a split is lossless | 拆分后合并做字节级校验，证明拆分无损 |
| 💻 CLI | Full headless interface for cron / CI / servers, no GUI needed | 完整命令行，适合 cron / CI / 服务器 |

## Screenshots / 截图

### 🔪 拆分压缩 / Split & Compress

![Split & Compress](docs/split.png)

### 👁 数据预览 / Data Preview

![Data Preview](docs/preview.png)

### 📊 列统计 / Column Statistics

![Column Statistics](docs/stats.png)

### 📦 解压缩 / Extract

![Extract](docs/extract.png)

## CLI / 命令行

桌面版适合手动处理，服务器和脚本需要的是无界面入口。`crates/cli` 与 GUI 共用同一套核心管线，
行为不会漂移。

```bash
# 安装到 ~/.cargo/bin
cargo install --path crates/cli

# 拆分（默认 zstd、10 片）
csv-splitter split big.csv -n 8 -f zst -o chunks.tar

# 只要几列，并在拆分前按列排序
csv-splitter split big.csv --columns id,phone,score --sort score --desc -n 4 -f zip

# 合并回单文件（支持 tar / tar.gz / tar.zst / tar.bz2 / tar.xz / zip）
csv-splitter merge chunks.tar -o restored.csv

# 证明这次拆分是无损的：split → merge → 逐字节比较
csv-splitter verify big.csv -n 6

# 看一眼数据与列分布
csv-splitter preview big.csv --rows 5
csv-splitter stats big.csv --sample 50000 --top 5

# 解压任意支持的归档
csv-splitter extract chunks.tar -o ./out
```

所有子命令都支持 `-d/--delim auto|comma|tab|semicolon|pipe|<char>`、`-q/--quiet`、`-h/--help`。

### 设计上的两个保证 / Two guarantees

1. **无损**：`split` 按*逻辑记录*切分（引号内换行的字段算一条记录），`merge` 去掉重复表头后按序拼接，
   `verify` 用字节比较兜底。`splits > 记录数` 时不会产生空分片。
2. **内存可控**：`--buf-mb` 是**总预算**，拆成 N 片不会变成 N × 缓冲。旧版每片固定 64 MB，
   10 片时峰值接近 700 MB。

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

## Build from Source / 从源码构建

Requires **Rust 1.75+**.

```bash
# Clone
git clone https://github.com/Li-Binzhuang/csv-splitter.git
cd csv-splitter

# Dev build
cargo build

# Release build (with LTO)
cargo build --release

# Run the GUI
cargo run --release -p csv-splitter-gui

# Run the CLI
cargo run --release -p csv-splitter-cli -- --help
```

### Platform Dependencies / 平台依赖

| Platform | Dependencies |
|----------|-------------|
| macOS | None |
| Windows | None |
| Linux | CJK font + GTK3 dev libs |

Linux setup:

```bash
# Ubuntu / Debian
sudo apt install fonts-noto-cjk libgtk-3-dev libxdo-dev

# Arch
sudo pacman -S noto-fonts-cjk gtk3

# Fedora
sudo dnf install google-noto-sans-cjk-fonts gtk3-devel
```

## Project Structure / 项目结构

```
csv-splitter/
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
│   ├── cli/                   # Headless CLI (split / merge / verify / stats)
│   │   └── src/main.rs
│   └── gui/                   # GUI application (egui / eframe)
│       └── src/
│           ├── main.rs        # Entry point
│           ├── app.rs         # App framework + drag-drop handling
│           ├── state.rs       # Per-tab state definitions
│           ├── theme.rs       # Dark theme + CJK font loading
│           ├── i18n.rs        # Chinese / English i18n
│           ├── widgets/       # Shared UI components
│           └── tabs/          # Feature pages (split, preview, stats, decompress)
```

## CI/CD

| Trigger | Workflow | Description |
|---------|----------|-------------|
| Push to `main` / PR | CI | Check, Clippy (-D warnings), Tests |
| Tag `v*` push | Release | Build all platforms → GitHub Release |
| Manual dispatch | Release | Optional tag input, build with/without release |

To create a release:

```bash
git tag v0.1.0
git push origin v0.1.0
```

Release artifacts are automatically uploaded with SHA256 checksums. Tags containing `-alpha`, `-beta`, `-rc` are marked as pre-release.

## License

[MIT](LICENSE)