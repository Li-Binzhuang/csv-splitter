/// Supported languages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn toggle(&self) -> Self {
        match self {
            Lang::Zh => Lang::En,
            Lang::En => Lang::Zh,
        }
    }

    #[allow(dead_code)]
    pub fn label(&self) -> &str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "EN",
        }
    }
}

/// All translatable strings in the UI.
/// Call with current language to get the localized text.
macro_rules! t {
    ($lang:expr, $key:ident) => {
        $crate::i18n::text($lang, $crate::i18n::Key::$key)
    };
}
pub(crate) use t;

#[derive(Clone, Copy)]
pub enum Key {
    // Tabs
    TabSplit,
    TabPreview,
    TabStats,
    TabDecompress,

    // Split tab
    SplitTitle,
    SplitDesc,
    SectionFile,
    SectionParams,
    InputFile,
    ColumnFilter,
    OutputFile,
    OutputHint,
    SaveLocation,
    Splits,
    BufferMb,
    CompressFormat,
    CompressLevel,
    SortColumn,
    SortColumnHint,
    Descending,
    StartSplit,

    // Preview tab
    PreviewTitle,
    PreviewDesc,
    CsvFile,
    LoadPreview,
    FileSize,
    ApproxRows,
    DetectedDelim,
    ColumnCount,

    // Stats tab
    StatsTitle,
    StatsDesc,
    SampleSize,
    StartAnalysis,
    AnalysisResult,
    SelectColumn,
    UniqueValues,
    EmptyValues,
    Sampled,
    ColValue,
    ColCount,
    ColPercent,

    // Decompress tab
    DecompTitle,
    DecompDesc,
    ArchiveFile,
    OutputDir,
    StartExtract,

    // Widgets / common
    Delimiter,
    CustomDelim,
    CustomChar,
    BrowseFile,
    BrowseDir,
    FileHint,
    DirHint,
    LogTitle,
    Processing,
    Done,
    Failed,
    Finished,
    Error,

    // Compress format filter labels
    FilterCsvArchive,
    FilterCsvTxt,
    FilterCsv,
    FilterArchive,
}

pub fn text(lang: Lang, key: Key) -> &'static str {
    match (lang, key) {
        // --- Tabs ---
        (Lang::Zh, Key::TabSplit) => "🔪 拆分压缩",
        (Lang::En, Key::TabSplit) => "🔪 Split",
        (Lang::Zh, Key::TabPreview) => "👁 数据预览",
        (Lang::En, Key::TabPreview) => "👁 Preview",
        (Lang::Zh, Key::TabStats) => "📊 列统计",
        (Lang::En, Key::TabStats) => "📊 Stats",
        (Lang::Zh, Key::TabDecompress) => "📦 解压缩",
        (Lang::En, Key::TabDecompress) => "📦 Extract",

        // --- Split ---
        (Lang::Zh, Key::SplitTitle) => "🔪 拆分 & 压缩",
        (Lang::En, Key::SplitTitle) => "🔪 Split & Compress",
        (Lang::Zh, Key::SplitDesc) => "将大型 CSV 文件拆分为多个压缩分片",
        (Lang::En, Key::SplitDesc) => "Split large CSV files into compressed chunks",
        (Lang::Zh, Key::SectionFile) => "📁 文件设置",
        (Lang::En, Key::SectionFile) => "📁 Files",
        (Lang::Zh, Key::SectionParams) => "⚙️ 参数设置",
        (Lang::En, Key::SectionParams) => "⚙️ Parameters",
        (Lang::Zh, Key::InputFile) => "输入文件:",
        (Lang::En, Key::InputFile) => "Input:",
        (Lang::Zh, Key::ColumnFilter) => "列过滤文件:",
        (Lang::En, Key::ColumnFilter) => "Column filter:",
        (Lang::Zh, Key::OutputFile) => "输出文件:",
        (Lang::En, Key::OutputFile) => "Output:",
        (Lang::Zh, Key::OutputHint) => "留空则自动生成...",
        (Lang::En, Key::OutputHint) => "Leave empty for auto...",
        (Lang::Zh, Key::SaveLocation) => "选择保存位置",
        (Lang::En, Key::SaveLocation) => "Choose save location",
        (Lang::Zh, Key::Splits) => "拆分份数:",
        (Lang::En, Key::Splits) => "Splits:",
        (Lang::Zh, Key::BufferMb) => "缓冲区 (MB):",
        (Lang::En, Key::BufferMb) => "Buffer (MB):",
        (Lang::Zh, Key::CompressFormat) => "压缩格式:",
        (Lang::En, Key::CompressFormat) => "Format:",
        (Lang::Zh, Key::CompressLevel) => "压缩级别:",
        (Lang::En, Key::CompressLevel) => "Level:",
        (Lang::Zh, Key::SortColumn) => "排序列名:",
        (Lang::En, Key::SortColumn) => "Sort column:",
        (Lang::Zh, Key::SortColumnHint) => "留空则不排序",
        (Lang::En, Key::SortColumnHint) => "Leave empty to skip",
        (Lang::Zh, Key::Descending) => "降序",
        (Lang::En, Key::Descending) => "Desc",
        (Lang::Zh, Key::StartSplit) => "🚀 开始拆分",
        (Lang::En, Key::StartSplit) => "🚀 Start",

        // --- Preview ---
        (Lang::Zh, Key::PreviewTitle) => "👁 数据预览",
        (Lang::En, Key::PreviewTitle) => "👁 Data Preview",
        (Lang::Zh, Key::PreviewDesc) => "快速预览 CSV 文件的前 100 行数据",
        (Lang::En, Key::PreviewDesc) => "Preview the first 100 rows of a CSV file",
        (Lang::Zh, Key::CsvFile) => "CSV 文件:",
        (Lang::En, Key::CsvFile) => "CSV file:",
        (Lang::Zh, Key::LoadPreview) => "📖 加载预览",
        (Lang::En, Key::LoadPreview) => "📖 Load",
        (Lang::Zh, Key::FileSize) => "大小",
        (Lang::En, Key::FileSize) => "Size",
        (Lang::Zh, Key::ApproxRows) => "行",
        (Lang::En, Key::ApproxRows) => "rows",
        (Lang::Zh, Key::DetectedDelim) => "分隔符",
        (Lang::En, Key::DetectedDelim) => "Delimiter",
        (Lang::Zh, Key::ColumnCount) => "列",
        (Lang::En, Key::ColumnCount) => "cols",

        // --- Stats ---
        (Lang::Zh, Key::StatsTitle) => "📊 列统计",
        (Lang::En, Key::StatsTitle) => "📊 Column Statistics",
        (Lang::Zh, Key::StatsDesc) => "通过采样分析各列的数据分布",
        (Lang::En, Key::StatsDesc) => "Analyze column data distribution via sampling",
        (Lang::Zh, Key::SampleSize) => "采样行数:",
        (Lang::En, Key::SampleSize) => "Sample size:",
        (Lang::Zh, Key::StartAnalysis) => "📊 开始分析",
        (Lang::En, Key::StartAnalysis) => "📊 Analyze",
        (Lang::Zh, Key::AnalysisResult) => "📈 分析结果",
        (Lang::En, Key::AnalysisResult) => "📈 Results",
        (Lang::Zh, Key::SelectColumn) => "选择列:",
        (Lang::En, Key::SelectColumn) => "Column:",
        (Lang::Zh, Key::UniqueValues) => "唯一值",
        (Lang::En, Key::UniqueValues) => "Unique",
        (Lang::Zh, Key::EmptyValues) => "空值",
        (Lang::En, Key::EmptyValues) => "Empty",
        (Lang::Zh, Key::Sampled) => "采样",
        (Lang::En, Key::Sampled) => "Sampled",
        (Lang::Zh, Key::ColValue) => "值",
        (Lang::En, Key::ColValue) => "Value",
        (Lang::Zh, Key::ColCount) => "计数",
        (Lang::En, Key::ColCount) => "Count",
        (Lang::Zh, Key::ColPercent) => "占比",
        (Lang::En, Key::ColPercent) => "Percent",

        // --- Decompress ---
        (Lang::Zh, Key::DecompTitle) => "📦 解压缩",
        (Lang::En, Key::DecompTitle) => "📦 Extract",
        (Lang::Zh, Key::DecompDesc) => "支持: zstd, gzip, bzip2, xz, zip, 7z, tar 及其组合格式",
        (Lang::En, Key::DecompDesc) => "Supports: zstd, gzip, bzip2, xz, zip, 7z, tar and combinations",
        (Lang::Zh, Key::ArchiveFile) => "压缩文件:",
        (Lang::En, Key::ArchiveFile) => "Archive:",
        (Lang::Zh, Key::OutputDir) => "输出目录:",
        (Lang::En, Key::OutputDir) => "Output dir:",
        (Lang::Zh, Key::StartExtract) => "📦 开始解压",
        (Lang::En, Key::StartExtract) => "📦 Extract",

        // --- Widgets / common ---
        (Lang::Zh, Key::Delimiter) => "分隔符:",
        (Lang::En, Key::Delimiter) => "Delimiter:",
        (Lang::Zh, Key::CustomDelim) => "自定义",
        (Lang::En, Key::CustomDelim) => "Custom",
        (Lang::Zh, Key::CustomChar) => "自定义字符:",
        (Lang::En, Key::CustomChar) => "Custom char:",
        (Lang::Zh, Key::BrowseFile) => "浏览文件",
        (Lang::En, Key::BrowseFile) => "Browse file",
        (Lang::Zh, Key::BrowseDir) => "浏览目录",
        (Lang::En, Key::BrowseDir) => "Browse folder",
        (Lang::Zh, Key::FileHint) => "点击右侧按钮选择文件...",
        (Lang::En, Key::FileHint) => "Click browse to select a file...",
        (Lang::Zh, Key::DirHint) => "点击右侧按钮选择目录...",
        (Lang::En, Key::DirHint) => "Click browse to select a folder...",
        (Lang::Zh, Key::LogTitle) => "📋 日志:",
        (Lang::En, Key::LogTitle) => "📋 Log:",
        (Lang::Zh, Key::Processing) => "处理中...",
        (Lang::En, Key::Processing) => "Processing...",
        (Lang::Zh, Key::Done) => "✅ 完成",
        (Lang::En, Key::Done) => "✅ Done",
        (Lang::Zh, Key::Failed) => "❌ 失败",
        (Lang::En, Key::Failed) => "❌ Failed",
        (Lang::Zh, Key::Finished) => "完成",
        (Lang::En, Key::Finished) => "Done",
        (Lang::Zh, Key::Error) => "错误",
        (Lang::En, Key::Error) => "Error",

        // --- Filter labels ---
        (Lang::Zh, Key::FilterCsvArchive) => "CSV / 压缩文件",
        (Lang::En, Key::FilterCsvArchive) => "CSV / Archives",
        (Lang::Zh, Key::FilterCsvTxt) => "CSV/TXT",
        (Lang::En, Key::FilterCsvTxt) => "CSV/TXT",
        (Lang::Zh, Key::FilterCsv) => "CSV",
        (Lang::En, Key::FilterCsv) => "CSV",
        (Lang::Zh, Key::FilterArchive) => "压缩文件",
        (Lang::En, Key::FilterArchive) => "Archives",
    }
}
