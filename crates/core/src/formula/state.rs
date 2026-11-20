//! Per-column state for formulas that look at more than one row.
//!
//! The export streams forward, so "the previous row" is the only kind of history a
//! formula can have. Keeping a bounded ring buffer per referenced column costs
//! memory proportional to the lag distance, never to the number of rows — the same
//! rule the rest of the engine follows.

use std::collections::{HashMap, VecDeque};

/// How much history a formula column needs, gathered before the first row is read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowNeeds {
    /// Columns read through `lag(...)`, with the largest distance asked for.
    pub lags: Vec<(String, usize)>,
    /// Columns accumulated by `running_total(...)`.
    pub running: Vec<String>,
    /// Columns whose totals `sum(...)`, `average(...)` and `count(...)` read,
    /// computed once before the first row is emitted.
    pub aggregates: Vec<String>,
    /// `(column, criteria)` for `countif`/`sumif`, evaluated in the same pre-pass.
    pub filtered: Vec<(String, String)>,
}

impl RowNeeds {
    pub fn is_empty(&self) -> bool {
        self.lags.is_empty()
            && self.running.is_empty()
            && self.aggregates.is_empty()
            && self.filtered.is_empty()
    }

    pub fn max_lag(&self) -> usize {
        self.lags.iter().map(|(_, k)| *k).max().unwrap_or(0)
    }

    /// Merge another column's needs into this one.
    pub fn absorb(&mut self, other: &RowNeeds) {
        for (column, k) in &other.lags {
            match self.lags.iter_mut().find(|(c, _)| c == column) {
                Some(slot) => slot.1 = slot.1.max(*k),
                None => self.lags.push((column.clone(), *k)),
            }
        }
        for column in &other.running {
            if !self.running.contains(column) {
                self.running.push(column.clone());
            }
        }
        for column in &other.aggregates {
            if !self.aggregates.contains(column) {
                self.aggregates.push(column.clone());
            }
        }
        for pair in &other.filtered {
            if !self.filtered.contains(pair) {
                self.filtered.push(pair.clone());
            }
        }
    }
}

/// The history itself, advanced one row at a time.
#[derive(Clone, Debug, Default)]
pub struct RowState {
    needs: RowNeeds,
    history: HashMap<String, VecDeque<String>>,
    sums: HashMap<String, f64>,
    /// Whole-column (numeric total, numeric count) for `sum`, `average`, `count`.
    totals: HashMap<String, (f64, u64)>,
    /// `(column, criteria)` -> (sum, count of matching cells) for `countif`/`sumif`.
    filtered: HashMap<(String, String), (f64, u64)>,
    /// Numeric (min, max) per column for `min`/`max`.
    bounds: HashMap<String, (f64, f64)>,
}

impl RowState {
    pub fn new(needs: RowNeeds) -> Self {
        let mut history = HashMap::new();
        for (column, k) in &needs.lags {
            history.insert(column.clone(), VecDeque::with_capacity(*k));
        }
        Self {
            needs,
            history,
            sums: HashMap::new(),
            totals: HashMap::new(),
            filtered: HashMap::new(),
            bounds: HashMap::new(),
        }
    }

    pub fn needs(&self) -> &RowNeeds {
        &self.needs
    }

    /// Value `distance` rows before the current one, if it has been seen.
    pub fn lagged(&self, column: &str, distance: usize) -> Option<&str> {
        let history = self.history.get(column)?;
        if distance == 0 || distance > history.len() {
            return None;
        }
        history.get(history.len() - distance).map(String::as_str)
    }

    /// Install the totals a pre-pass computed for whole-column aggregates.
    pub fn set_totals(&mut self, totals: HashMap<String, (f64, u64)>) {
        self.totals = totals;
    }

    /// Install the numeric bounds a pre-pass computed for whole-column MIN/MAX.
    pub fn set_bounds(&mut self, bounds: HashMap<String, (f64, f64)>) {
        self.bounds = bounds;
    }

    pub fn bounds(&self, column: &str) -> Option<(f64, f64)> {
        self.bounds.get(column).copied()
    }

    /// Matching cells for a `countif`/`sumif` pair: (numeric sum, count of matches).
    pub fn set_filtered(&mut self, filtered: HashMap<(String, String), (f64, u64)>) {
        self.filtered = filtered;
    }

    pub fn filtered(&self, column: &str, criteria: &str) -> Option<(f64, u64)> {
        self.filtered
            .get(&(column.to_string(), criteria.to_string()))
            .copied()
    }

    /// Numeric total and numeric count for a column, or `None` when the pre-pass
    /// did not run (the view path can render `lag`, but not a whole-column total).
    pub fn totals(&self, column: &str) -> Option<(f64, u64)> {
        self.totals.get(column).copied()
    }

    /// Accumulated total for a column, including the current row.
    pub fn sum(&self, column: &str) -> f64 {
        self.sums.get(column).copied().unwrap_or(0.0)
    }

    /// Fold this row's values into the sums, before the formulas are evaluated.
    pub fn observe_sums(&mut self, row: &HashMap<String, String>) {
        for column in &self.needs.running {
            let value = row.get(column).map(String::as_str).unwrap_or("");
            let number = value.trim().parse::<f64>().ok().filter(|v| v.is_finite());
            let entry = self.sums.entry(column.clone()).or_insert(0.0);
            *entry += number.unwrap_or(0.0);
        }
    }

    /// Remember this row, for the rows that follow.
    ///
    /// Call this *after* the formulas for the row have been evaluated: the history
    /// must contain rows before the current one, or `lag(col, 1)` would return the
    /// row it is being evaluated on.
    pub fn push_row(&mut self, row: &HashMap<String, String>) {
        let max = self.needs.max_lag();
        if max == 0 {
            return;
        }
        for (column, _) in &self.needs.lags {
            let value = row.get(column).cloned().unwrap_or_default();
            let buffer = self.history.entry(column.clone()).or_default();
            if buffer.len() == max {
                buffer.pop_front();
            }
            buffer.push_back(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn lag_returns_the_row_before_and_says_so_when_it_cannot() {
        let mut state = RowState::new(RowNeeds {
            lags: vec![("amount".to_string(), 2)],
            running: Vec::new(),
            aggregates: Vec::new(),
            filtered: Vec::new(),
        });
        // Evaluating the first row: nothing has been seen.
        assert_eq!(state.lagged("amount", 1), None);
        state.push_row(&row(&[("amount", "10")]));
        // Evaluating the second row: one row of history, but not two.
        assert_eq!(state.lagged("amount", 1), Some("10"));
        assert_eq!(state.lagged("amount", 2), None, "no row before the first");
        state.push_row(&row(&[("amount", "20")]));
        // Evaluating the third row.
        assert_eq!(state.lagged("amount", 1), Some("20"));
        assert_eq!(state.lagged("amount", 2), Some("10"));
        state.push_row(&row(&[("amount", "30")]));
        // The window is bounded: two rows back is as far as it goes.
        assert_eq!(state.lagged("amount", 1), Some("30"));
        assert_eq!(state.lagged("amount", 2), Some("20"));
        assert_eq!(state.lagged("amount", 3), None);
    }

    #[test]
    fn running_totals_follow_the_rows() {
        let mut state = RowState::new(RowNeeds {
            lags: Vec::new(),
            running: vec!["amount".to_string()],
            aggregates: Vec::new(),
            filtered: Vec::new(),
        });
        state.observe_sums(&row(&[("amount", "10")]));
        assert_eq!(state.sum("amount"), 10.0);
        state.observe_sums(&row(&[("amount", "20")]));
        assert_eq!(state.sum("amount"), 30.0);
        // A non-numeric value contributes nothing rather than poisoning the total.
        state.observe_sums(&row(&[("amount", "N/A")]));
        assert_eq!(state.sum("amount"), 30.0);
    }

    #[test]
    fn needs_merge_across_columns() {
        let mut a = RowNeeds {
            lags: vec![("amount".to_string(), 1)],
            running: vec!["qty".to_string()],
            aggregates: Vec::new(),
            filtered: Vec::new(),
        };
        let b = RowNeeds {
            lags: vec![("amount".to_string(), 3)],
            running: vec!["qty".to_string(), "amount".to_string()],
            aggregates: vec!["amount".to_string()],
            filtered: Vec::new(),
        };
        a.absorb(&b);
        assert_eq!(a.lags.len(), 1);
        assert_eq!(a.lags[0].1, 3, "the larger window wins");
        assert_eq!(a.running.len(), 2);
        assert_eq!(a.aggregates, vec!["amount".to_string()]);
    }
}
