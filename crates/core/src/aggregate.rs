//! Aggregates shared by `groupby`, `pivot` and (later) chart data.
//!
//! One accumulator per (group, measure). Keeping the arithmetic in one place is
//! not tidiness for its own sake: `sum` uses compensated (Kahan) summation, and
//! if pivot re-implemented it slightly differently the two commands would print
//! different totals for the same file.

use std::fmt::Write as _;

/// One aggregate over one column: `amount:sum` means "sum of amount".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggOp {
    Count,
    Sum,
    Mean,
    Min,
    Max,
}

impl AggOp {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "count" | "n" => Ok(Self::Count),
            "sum" | "total" => Ok(Self::Sum),
            "mean" | "avg" | "average" => Ok(Self::Mean),
            "min" => Ok(Self::Min),
            "max" => Ok(Self::Max),
            other => Err(format!(
                "unknown aggregate '{other}' (use count/sum/mean/min/max)"
            )),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Sum => "sum",
            Self::Mean => "mean",
            Self::Min => "min",
            Self::Max => "max",
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Self::Sum | Self::Mean)
    }
}

/// One aggregate over one column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AggSpec {
    pub column: String,
    pub op: AggOp,
}

impl AggSpec {
    /// Parse `col:op,other:op`.
    pub fn parse_list(raw: &str) -> Result<Vec<Self>, String> {
        let mut out = Vec::new();
        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (column, op) = part
                .split_once(':')
                .ok_or_else(|| format!("aggregate '{part}' must be column:op"))?;
            let column = column.trim();
            if column.is_empty() {
                return Err(format!("aggregate '{part}' has no column"));
            }
            out.push(Self {
                column: column.to_string(),
                op: AggOp::parse(op)?,
            });
        }
        if out.is_empty() {
            return Err("no aggregates given".to_string());
        }
        Ok(out)
    }

    pub fn output_name(&self) -> String {
        format!("{}_{}", self.op.name(), self.column)
    }
}

/// Running statistics for one measure within one group.
#[derive(Clone, Debug)]
pub struct Accumulator {
    /// Values seen (empty cells are skipped, so this is not the row count).
    count: u64,
    sum: f64,
    /// Compensation term for Kahan summation.
    compensation: f64,
    min: f64,
    max: f64,
    non_numeric: u64,
    /// Lexical extremes, used when min/max runs over text.
    text_min: Option<String>,
    text_max: Option<String>,
}

impl Default for Accumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Accumulator {
    pub fn new() -> Self {
        Self {
            count: 0,
            sum: 0.0,
            compensation: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            non_numeric: 0,
            text_min: None,
            text_max: None,
        }
    }

    /// Fold one cell in. Empty cells are ignored, which is what a spreadsheet
    /// does with `AVERAGE` over a blank cell.
    pub fn observe(&mut self, value: &str) {
        if value.is_empty() {
            return;
        }
        self.count += 1;
        match value.trim().parse::<f64>() {
            Ok(n) if n.is_finite() => {
                // Compensated summation keeps the printed total from drifting in
                // the last digits once the column has tens of thousands of rows.
                let adjusted = n - self.compensation;
                let tentative = self.sum + adjusted;
                self.compensation = (tentative - self.sum) - adjusted;
                self.sum = tentative;
                if n < self.min {
                    self.min = n;
                }
                if n > self.max {
                    self.max = n;
                }
            }
            _ => self.non_numeric += 1,
        }
        match &self.text_min {
            None => self.text_min = Some(value.to_string()),
            Some(current) if value < current.as_str() => self.text_min = Some(value.to_string()),
            _ => {}
        }
        match &self.text_max {
            None => self.text_max = Some(value.to_string()),
            Some(current) if value > current.as_str() => self.text_max = Some(value.to_string()),
            _ => {}
        }
    }

    /// How many non-empty values were seen.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Render the final cell.
    ///
    /// A numeric aggregate over a column that turns out to hold text reports
    /// `<mixed>` rather than quietly averaging only the numeric half.
    pub fn render(&self, op: AggOp) -> String {
        if op.is_numeric() && self.non_numeric > 0 {
            return "<mixed>".to_string();
        }
        match op {
            AggOp::Count => self.count.to_string(),
            AggOp::Sum => format_number(self.sum),
            AggOp::Mean => {
                if self.count == 0 {
                    String::new()
                } else {
                    format_number(self.sum / self.count as f64)
                }
            }
            AggOp::Min => {
                if self.count == 0 {
                    String::new()
                } else if self.non_numeric > 0 {
                    self.text_min.clone().unwrap_or_default()
                } else {
                    format_number(self.min)
                }
            }
            AggOp::Max => {
                if self.count == 0 {
                    String::new()
                } else if self.non_numeric > 0 {
                    self.text_max.clone().unwrap_or_default()
                } else {
                    format_number(self.max)
                }
            }
        }
    }
}

/// Format a result so the printed digits mean something.
///
/// f64 has ~15 significant digits, so summing 20 million values leaves error in
/// the last few *bits* — visible as `.200012` on a total of 2e11. The display
/// therefore keeps 6 decimals for ordinary magnitudes and 2 for very large ones,
/// where anything beyond a cent is noise. Trailing zeros are trimmed so a whole
/// number does not print as `60.000000`.
pub fn format_number(v: f64) -> String {
    let decimals = if v.abs() >= 1e9 { 2 } else { 6 };
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let mut rounded = format!("{v:.decimals$}");
    while rounded.ends_with('0') {
        rounded.pop();
    }
    if rounded.ends_with('.') {
        rounded.pop();
    }
    if rounded.is_empty() || rounded == "-0" {
        "0".to_string()
    } else {
        rounded
    }
}

/// Build a value from its parts, so no literal carries more precision than f64.
pub fn render_parts(integer: i64) -> String {
    let mut out = String::new();
    let _ = write!(out, "{integer}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_is_compensated() {
        let mut acc = Accumulator::new();
        for _ in 0..1_000_000 {
            acc.observe("0.1");
        }
        // Naive summation of 0.1 a million times drifts visibly; Kahan does not.
        assert_eq!(acc.render(AggOp::Sum), "100000");
    }

    #[test]
    fn empty_cells_are_skipped_by_count_and_mean() {
        let mut acc = Accumulator::new();
        acc.observe("10");
        acc.observe("");
        acc.observe("20");
        assert_eq!(acc.render(AggOp::Mean), "15");
        assert_eq!(
            acc.render(AggOp::Count),
            "2",
            "count counts values, not rows"
        );
    }

    #[test]
    fn mixed_columns_do_not_silently_average() {
        let mut acc = Accumulator::new();
        acc.observe("10");
        acc.observe("N/A");
        assert_eq!(acc.render(AggOp::Sum), "<mixed>");
        // Text min/max still works.
        assert_eq!(acc.render(AggOp::Min), "10");
    }

    #[test]
    fn big_totals_drop_the_noise_after_the_cent() {
        let mut acc = Accumulator::new();
        acc.observe("215064677256.2");
        // Six decimals would print `215064677256.200012`; two is enough, and the
        // trailing zero is trimmed so the column stays readable.
        assert_eq!(acc.render(AggOp::Sum), "215064677256.2");
    }
}
