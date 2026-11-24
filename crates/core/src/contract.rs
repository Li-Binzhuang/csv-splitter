//! Machine-readable operation contract shared by the CLI, HTTP and MCP adapters.
//!
//! This is the first stage of the operations layer: adapters can discover the same
//! names, effects and stable error codes instead of re-describing them. Execution is
//! still in the existing core functions; typed requests move here incrementally.

use crate::format_json;

/// What an operation does to files or state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Reads files only.
    Read,
    /// Computes a change but writes nothing until applied.
    Preview,
    /// Writes an output, journal, layout or other durable state.
    Apply,
    /// Starts work that continues after the request returns.
    Job,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Preview => "preview",
            Self::Apply => "apply",
            Self::Job => "job",
        }
    }
}

/// Stable error codes. Adapters may add HTTP status mapping, never rename these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidRequest,
    NotFound,
    PathEscape,
    UnknownTool,
    UnknownJob,
    CliError,
    JobStartError,
    Conflict,
    Unsupported,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::NotFound => "not_found",
            Self::PathEscape => "path_escape",
            Self::UnknownTool => "unknown_tool",
            Self::UnknownJob => "unknown_job",
            Self::CliError => "cli_error",
            Self::JobStartError => "job_start_error",
            Self::Conflict => "conflict",
            Self::Unsupported => "unsupported",
            Self::Internal => "internal",
        }
    }
}

/// One operation as adapters publish it.
pub struct OperationSpec {
    pub name: &'static str,
    pub effect: Effect,
    pub summary: &'static str,
    /// Short form of the accepted input, not a substitute for a JSON Schema.
    pub input: &'static str,
    pub output: &'static str,
}

pub const OPERATIONS: &[OperationSpec] = &[
    OperationSpec {
        name: "schema",
        effect: Effect::Read,
        summary: "Inspect columns, types and row count.",
        input: "path",
        output: "columns, rowCount",
    },
    OperationSpec {
        name: "view",
        effect: Effect::Read,
        summary: "Read a page of the edited sheet.",
        input: "path, start, count, where/order optional",
        output: "rows, total, appliedEdits",
    },
    OperationSpec {
        name: "filter",
        effect: Effect::Read,
        summary: "Find rows matching a predicate.",
        input: "path, where, limit optional",
        output: "matched, rows/out",
    },
    OperationSpec {
        name: "search",
        effect: Effect::Read,
        summary: "Find text in one or more columns.",
        input: "path, text, columns optional",
        output: "matches, snippets",
    },
    OperationSpec {
        name: "quality",
        effect: Effect::Read,
        summary: "Profile columns and report quality findings.",
        input: "path, key/sample optional",
        output: "report",
    },
    OperationSpec {
        name: "pivot",
        effect: Effect::Read,
        summary: "Build a rows × columns × measures matrix.",
        input: "path, rows, values, agg",
        output: "matrix/out",
    },
    OperationSpec {
        name: "chart",
        effect: Effect::Read,
        summary: "Return chart categories and series.",
        input: "path, x, y, type optional",
        output: "chart JSON",
    },
    OperationSpec {
        name: "edit_preview",
        effect: Effect::Preview,
        summary: "Validate edits and show affected rows without writing.",
        input: "path, action, values",
        output: "affected, preview",
    },
    OperationSpec {
        name: "edit_apply",
        effect: Effect::Apply,
        summary: "Append a validated edit transaction to the journal.",
        input: "path, action, values, expect optional",
        output: "journal row, applied",
    },
    OperationSpec {
        name: "convert",
        effect: Effect::Apply,
        summary: "Write a clean copy in another format.",
        input: "path, format, out",
        output: "out, rows",
    },
    OperationSpec {
        name: "pipeline_run",
        effect: Effect::Job,
        summary: "Run stale steps of a declarative pipeline.",
        input: "plan, root/force/contentCheck optional",
        output: "job id, lineage",
    },
    OperationSpec {
        name: "pipeline_watch",
        effect: Effect::Job,
        summary: "Poll a pipeline and rerun stale steps.",
        input: "plan, interval/backoff/maxFailures optional",
        output: "state file, rounds",
    },
    OperationSpec {
        name: "job_status",
        effect: Effect::Read,
        summary: "Read or wait on a background job.",
        input: "id, wait optional",
        output: "status, progress, result",
    },
    OperationSpec {
        name: "cache_stats",
        effect: Effect::Read,
        summary: "Report materialized-cache hits and size.",
        input: "none",
        output: "hits, misses, entries, bytes",
    },
    OperationSpec {
        name: "gate",
        effect: Effect::Read,
        summary: "Enforce row/empty/uniqueness thresholds and fail closed.",
        input: "path or directory, minRows/maxEmpty/unique optional",
        output: "passed, rows, empty, uniqueDuplicates, report optional",
    },
    OperationSpec {
        name: "report",
        effect: Effect::Read,
        summary: "Render a deterministic CSV-spec Markdown or HTML report.",
        input: "path, spec optional, format/title optional",
        output: "markdown/html body",
    },
    OperationSpec {
        name: "concat",
        effect: Effect::Apply,
        summary: "Concatenate same-shaped files or a directory's CSV files.",
        input: "paths or directory, out, byName/engine optional",
        output: "out, rows, columns",
    },
];

pub const ERROR_CODES: &[ErrorCode] = &[
    ErrorCode::InvalidRequest,
    ErrorCode::NotFound,
    ErrorCode::PathEscape,
    ErrorCode::UnknownTool,
    ErrorCode::UnknownJob,
    ErrorCode::CliError,
    ErrorCode::JobStartError,
    ErrorCode::Conflict,
    ErrorCode::Unsupported,
    ErrorCode::Internal,
];

/// Full catalog as one JSON object. Hand-built to keep core free of serde.
pub fn operations_json() -> String {
    let ops = OPERATIONS
        .iter()
        .map(|op| {
            format!(
                r#"{{"name":{},"effect":{},"summary":{},"input":{},"output":{}}}"#,
                format_json(op.name),
                format_json(op.effect.as_str()),
                format_json(op.summary),
                format_json(op.input),
                format_json(op.output)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let errors = ERROR_CODES
        .iter()
        .map(|code| format_json(code.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"version":{},"operations":[{}],"errorCodes":[{}]}}"#,
        format_json(env!("CARGO_PKG_VERSION")),
        ops,
        errors
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_unique_names_and_effects() {
        let mut names = std::collections::HashSet::new();
        for op in OPERATIONS {
            assert!(names.insert(op.name), "duplicate operation {}", op.name);
            assert!(!op.summary.is_empty());
            assert!(matches!(
                op.effect,
                Effect::Read | Effect::Preview | Effect::Apply | Effect::Job
            ));
        }
        assert_eq!(names.len(), OPERATIONS.len());
    }

    #[test]
    fn json_contains_every_operation_and_error_code() {
        let json = operations_json();
        for op in OPERATIONS {
            assert!(
                json.contains(&format!("{:?}", op.name).replace('"', "")),
                "missing {}",
                op.name
            );
        }
        for code in ERROR_CODES {
            assert!(json.contains(code.as_str()), "missing {}", code.as_str());
        }
        assert!(json.contains("\"version\""));
    }

    #[test]
    fn effects_render_lowercase() {
        assert_eq!(Effect::Read.as_str(), "read");
        assert_eq!(Effect::Preview.as_str(), "preview");
        assert_eq!(Effect::Apply.as_str(), "apply");
        assert_eq!(Effect::Job.as_str(), "job");
    }
}
