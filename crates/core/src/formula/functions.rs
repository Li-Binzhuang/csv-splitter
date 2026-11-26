//! Built-in functions for formula columns.
//!
//! Kept apart from the parser for one practical reason: this list grows every time
//! someone needs `if` or `left`, and mixing "how to parse" with "what 30 functions
//! do" makes neither readable.
//!
//! Conventions that follow a spreadsheet rather than Rust:
//!
//! * `if(cond, a, b)` evaluates **only the branch it needs** — `if(qty=0, 0, total/qty)`
//!   must not divide by zero while computing the branch it will discard.
//! * text comparisons are case-sensitive, like `EXACT`; `=` on two numbers compares
//!   numerically, `=` on text compares literally.
//! * a bad argument returns `#VALUE!` rather than panicking; a wrong number of
//!   arguments is a parse-time error where possible, but reported the same way at
//!   evaluation time.

use super::state::SetOp;
use super::{eval, truthy, Ctx, Expr, Value};
use crate::layout::NumberFormat;

pub(super) fn call(name: &str, args: &[Expr], ctx: &Ctx<'_>) -> Value {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        // History functions: they read the state the exporter maintains, so a formula
        // can see the row before it without the file being loaded.
        // Conditional aggregates: the criteria (">1000", "CDMX", "<>10") were compiled
        // and evaluated in the same pre-pass as the plain totals.
        "countifs" | "sumifs" | "averageifs" | "minifs" | "maxifs" => {
            // A range as the first argument is the row-local form: both the values
            // and the criteria cells live in this same row.
            if matches!(args.first(), Some(Expr::Range { .. })) {
                return multi_range(lower.as_str(), args, ctx);
            }
            let Some(state) = ctx.state else {
                return Value::Error;
            };
            let op = match lower.as_str() {
                "sumifs" => SetOp::Sum,
                "averageifs" => SetOp::Average,
                "minifs" => SetOp::Min,
                "maxifs" => SetOp::Max,
                _ => SetOp::Count,
            };
            let (value, start) = if matches!(op, SetOp::Count) {
                (None, 0usize)
            } else {
                match args.first() {
                    Some(Expr::Column(column)) => (Some(column.clone()), 1usize),
                    _ => return Value::Error,
                }
            };
            let mut pairs = Vec::new();
            let mut i = start;
            while i + 1 < args.len() {
                if let (Expr::Column(column), Expr::Criteria { text, .. }) =
                    (&args[i], &args[i + 1])
                {
                    pairs.push((column.clone(), text.clone()));
                    i += 2;
                } else {
                    return Value::Error;
                }
            }
            let key = super::criteria_key(op, value.as_deref(), &pairs);
            let Some(values) = state.sets(&key) else {
                return Value::Error;
            };
            match op {
                SetOp::Count => Value::Number(values.count as f64),
                SetOp::Sum => Value::Number(values.sum),
                SetOp::Average if values.count > 0 => {
                    Value::Number(values.sum / values.count as f64)
                }
                SetOp::Min if values.count > 0 => Value::Number(values.min),
                SetOp::Max if values.count > 0 => Value::Number(values.max),
                SetOp::Average | SetOp::Min | SetOp::Max => Value::Error,
            }
        }
        "countif" | "sumif" | "averageif" => {
            // A same-row range: count/sum the cells between two columns that match.
            if let (Some(Expr::Range { from, to }), Some(Expr::Criteria { spec, .. })) =
                (args.first(), args.get(1))
            {
                let Some(cells) = range_cells(from, to, ctx) else {
                    return Value::Error;
                };
                let mut count = 0u64;
                let mut sum = 0.0;
                for cell in cells {
                    // An ordering criterion is numeric, like the column aggregates.
                    if matches!(
                        spec.op,
                        crate::filter::Op::Gt
                            | crate::filter::Op::Gte
                            | crate::filter::Op::Lt
                            | crate::filter::Op::Lte
                    ) && cell.trim().parse::<f64>().is_err()
                    {
                        continue;
                    }
                    if crate::filter::matches(cell, spec) {
                        count += 1;
                        if let Ok(value) = cell.trim().parse::<f64>() {
                            if value.is_finite() {
                                sum += value;
                            }
                        }
                    }
                }
                return match lower.as_str() {
                    "countif" => Value::Number(count as f64),
                    "averageif" if count == 0 => Value::Error,
                    "averageif" => Value::Number(sum / count as f64),
                    _ => Value::Number(sum),
                };
            }
            let Some(state) = ctx.state else {
                return Value::Error;
            };
            let (Some(Expr::Column(column)), Some(Expr::Criteria { text, .. })) =
                (args.first(), args.get(1))
            else {
                return Value::Error;
            };
            let Some((sum, count)) = state.filtered(column, text) else {
                return Value::Error;
            };
            match lower.as_str() {
                "countif" => Value::Number(count as f64),
                "averageif" if count == 0 => Value::Error,
                "averageif" => Value::Number(sum / count as f64),
                _ => Value::Number(sum),
            }
        }
        // SUMPRODUCT over two same-row ranges: pairwise multiply and add. Text cells
        // count as 0, like a spreadsheet, but a length mismatch is an error.
        "sumproduct" => {
            if args.len() < 2 {
                return Value::Error;
            }
            let mut columns: Vec<Vec<&str>> = Vec::with_capacity(args.len());
            for arg in args {
                let Expr::Range { from, to } = arg else {
                    return Value::Error;
                };
                let Some(cells) = range_cells(from, to, ctx) else {
                    return Value::Error;
                };
                columns.push(cells);
            }
            let len = columns[0].len();
            if len == 0 || columns.iter().any(|column| column.len() != len) {
                return Value::Error;
            }
            let mut total = 0.0;
            for index in 0..len {
                let mut product = 1.0;
                let mut usable = true;
                for column in &columns {
                    let value = column[index].trim().parse::<f64>().unwrap_or(0.0);
                    if !value.is_finite() {
                        usable = false;
                        break;
                    }
                    product *= value;
                }
                if usable {
                    total += product;
                }
            }
            Value::Number(total)
        }
        // SUMSQ/PRODUCT over one or more same-row ranges. Text and blanks are skipped;
        // no numeric cell at all is #VALUE! (an empty product has no meaning here).
        "sumsq" | "product" => {
            if args.is_empty() {
                return Value::Error;
            }
            let mut squares = 0.0;
            let mut product = 1.0;
            let mut count = 0usize;
            for arg in args {
                let Expr::Range { from, to } = arg else {
                    return Value::Error;
                };
                let Some(cells) = range_cells(from, to, ctx) else {
                    return Value::Error;
                };
                for cell in cells {
                    let Ok(value) = cell.trim().parse::<f64>() else {
                        continue;
                    };
                    if !value.is_finite() {
                        continue;
                    }
                    count += 1;
                    squares += value * value;
                    product *= value;
                }
            }
            if count == 0 {
                return Value::Error;
            }
            Value::Number(if lower == "sumsq" { squares } else { product })
        }
        // COUNTBLANK: empty cells in a column or a same-row range.
        "countblank" => {
            if let Some(Expr::Range { from, to }) = args.first() {
                let Some(cells) = range_cells(from, to, ctx) else {
                    return Value::Error;
                };
                let blank = cells.iter().filter(|c| c.trim().is_empty()).count() as u64;
                return Value::Number(blank as f64);
            }
            let Some(state) = ctx.state else {
                return Value::Error;
            };
            let Some(Expr::Column(column)) = args.first() else {
                return Value::Error;
            };
            let Some(blank) = state.blanks(column) else {
                return Value::Error;
            };
            Value::Number(blank as f64)
        }
        // SUBTOTAL(function_num, range): 1–11 and 101–111. CSV has no hidden
        // rows, so the 100-series maps to the same calculation.
        "subtotal" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(code) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let Some(Expr::Range { from, to }) = args.get(1) else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            if cells.iter().all(|cell| cell.trim().is_empty()) {
                return Value::Error;
            }
            if !code.is_finite() {
                return Value::Error;
            }
            let code = code.trunc() as i32;
            let code = if (101..=111).contains(&code) {
                code - 100
            } else if (1..=11).contains(&code) {
                code
            } else {
                return Value::Error;
            };
            subtotal_value(code, &cells)
        }
        // AGGREGATE(function_num, options, range[, k]): SUBTOTAL's 1–11 plus
        // LARGE/SMALL/PERCENTILE/QUARTILE families (14–19). CSV has no hidden rows
        // or error cells, so options 0/4/6 are equivalent here.
        "aggregate" => {
            if args.len() < 3 || args.len() > 4 {
                return Value::Error;
            }
            let Some(code) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let Some(options) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            let Some(Expr::Range { from, to }) = args.get(2) else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            if cells.iter().all(|cell| cell.trim().is_empty()) {
                return Value::Error;
            }
            if !options.is_finite() || ![0.0, 4.0, 6.0].contains(&options) {
                return Value::Error;
            }
            if !code.is_finite() {
                return Value::Error;
            }
            let code = code.trunc() as i32;
            if !((1..=11).contains(&code) || (14..=19).contains(&code)) {
                return Value::Error;
            }
            let k = args.get(3).and_then(|arg| eval(arg, ctx).number());
            if (14..=19).contains(&code) && k.is_none() {
                return Value::Error;
            }
            aggregate_value(code, k, &cells)
        }
        // Sample standard deviation / variance: Excel's STDEV.S and VAR.S. The pre-pass
        // kept the sum of squares, so this is O(1) per row.
        "stdev" | "var" | "stdevp" | "varp" | "stdev.s" | "var.s" | "stdev.p" | "var.p" => {
            let (sum, squares, count) = if let Some(Expr::Range { from, to }) = args.first() {
                let Some(values) = range_values(from, to, ctx) else {
                    return Value::Error;
                };
                (
                    values.iter().sum::<f64>(),
                    values.iter().map(|v| v * v).sum::<f64>(),
                    values.len() as u64,
                )
            } else {
                let Some(state) = ctx.state else {
                    return Value::Error;
                };
                let Some(Expr::Column(column)) = args.first() else {
                    return Value::Error;
                };
                let Some((sum, count)) = state.totals(column) else {
                    return Value::Error;
                };
                let Some(squares) = state.squares(column) else {
                    return Value::Error;
                };
                (sum, squares, count)
            };
            let variance = if lower.ends_with('p') {
                population_variance(sum, squares, count)
            } else {
                sample_variance(sum, squares, count)
            };
            let Some(variance) = variance else {
                return Value::Error;
            };
            Value::Number(if lower.starts_with("stdev") {
                variance.sqrt()
            } else {
                variance
            })
        }
        // MEDIAN over a same-row range; a whole-column median would need every value
        // kept, which is not affordable at 50M rows, so it stays a documented #VALUE!.
        "median" => {
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            if values.is_empty() {
                return Value::Error;
            }
            let mut values = values;
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let len = values.len();
            Value::Number(if len % 2 == 1 {
                values[len / 2]
            } else {
                (values[len / 2 - 1] + values[len / 2]) / 2.0
            })
        }
        // LARGE/SMALL over a same-row range: k is 1-based, truncated toward zero.
        "large" | "small" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let Some(k) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if !k.is_finite() || k < 1.0 {
                return Value::Error;
            }
            let k = k.floor() as usize;
            if k == 0 || k > values.len() {
                return Value::Error;
            }
            let mut values = values;
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Value::Number(if lower == "large" {
                values[values.len() - k]
            } else {
                values[k - 1]
            })
        }
        // QUARTILE.EXC: same quart grid, exclusive percentile boundary.
        "quartile.exc" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let Some(quart) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if !quart.is_finite() || !(0.0..=4.0).contains(&quart) {
                return Value::Error;
            }
            match percentile_exc_of(&values, quart / 4.0) {
                Some(value) => Value::Number(value),
                None => Value::Error,
            }
        }
        // QUARTILE.INC over a same-row range: quart 0..4 maps to p = quart/4.
        "quartile" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let Some(quart) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if !quart.is_finite() || !(0.0..=4.0).contains(&quart) {
                return Value::Error;
            }
            match percentile_of(&values, quart / 4.0) {
                Some(value) => Value::Number(value),
                None => Value::Error,
            }
        }
        // TRIMMEAN: drop the same percentage from both tails, then average.
        "trimmean" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let Some(percent) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if values.is_empty() || !percent.is_finite() || !(0.0..=1.0).contains(&percent) {
                return Value::Error;
            }
            let mut values = values;
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let trim =
                ((values.len() as f64 * percent / 2.0).floor() as usize).min(values.len() / 2);
            let kept = &values[trim..values.len() - trim];
            Value::Number(kept.iter().sum::<f64>() / kept.len() as f64)
        }
        // RANK.AVG: tied values share the average of their occupied ranks.
        "rank.avg" => {
            if args.len() < 2 || args.len() > 3 {
                return Value::Error;
            }
            let Some(number) = args.first().and_then(|arg| eval(arg, ctx).number()) else {
                return Value::Error;
            };
            let Some(Expr::Range { from, to }) = args.get(1) else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let ascending = args
                .get(2)
                .and_then(|arg| eval(arg, ctx).number())
                .map(|order| order != 0.0)
                .unwrap_or(false);
            let (ahead, equal) = if ascending {
                (
                    values.iter().filter(|value| **value < number).count(),
                    values.iter().filter(|value| **value == number).count(),
                )
            } else {
                (
                    values.iter().filter(|value| **value > number).count(),
                    values.iter().filter(|value| **value == number).count(),
                )
            };
            Value::Number(ahead as f64 + (equal as f64 + 1.0) / 2.0)
        }
        // RANK(number, range[, order]): 1 is the largest by default; ties share a rank.
        "rank" | "rank.eq" => {
            if args.len() < 2 || args.len() > 3 {
                return Value::Error;
            }
            let Some(number) = args.first().and_then(|arg| eval(arg, ctx).number()) else {
                return Value::Error;
            };
            let Some(Expr::Range { from, to }) = args.get(1) else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let ascending = args
                .get(2)
                .and_then(|arg| eval(arg, ctx).number())
                .map(|order| order != 0.0)
                .unwrap_or(false);
            let ahead = if ascending {
                values.iter().filter(|value| **value < number).count()
            } else {
                values.iter().filter(|value| **value > number).count()
            };
            Value::Number((ahead + 1) as f64)
        }
        // PERCENTILE.INC over a same-row range: p is 0..1 and the result interpolates
        // between the two neighbouring values.
        "percentile" | "percentile.inc" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let Some(p) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if values.is_empty() || !p.is_finite() || !(0.0..=1.0).contains(&p) {
                return Value::Error;
            }
            let mut values = values;
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if values.len() == 1 {
                return Value::Number(values[0]);
            }
            let rank = p * (values.len() - 1) as f64;
            let low = rank.floor() as usize;
            let high = (low + 1).min(values.len() - 1);
            let fraction = rank - low as f64;
            Value::Number(values[low] + fraction * (values[high] - values[low]))
        }
        // PERCENTILE.EXC: p is exclusive, rank = p*(n+1)-1.
        "percentile.exc" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let Some(p) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if values.is_empty() || !p.is_finite() || p <= 0.0 || p >= 1.0 {
                return Value::Error;
            }
            let mut values = values;
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let rank = p * (values.len() + 1) as f64 - 1.0;
            if rank < 0.0 || rank > (values.len() - 1) as f64 {
                return Value::Error;
            }
            let low = rank.floor() as usize;
            let high = (low + 1).min(values.len() - 1);
            let fraction = rank - low as f64;
            Value::Number(values[low] + fraction * (values[high] - values[low]))
        }
        // PERCENTRANK.EXC: (position+1)/(n+1) with interpolation.
        "percentrank.exc" => {
            if args.len() < 2 || args.len() > 3 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let Some(number) = args.get(1).and_then(|arg| eval(arg, ctx).number()) else {
                return Value::Error;
            };
            if values.is_empty() {
                return Value::Error;
            }
            let mut values = values;
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if number < values[0] || number > *values.last().unwrap() {
                return Value::Text("#N/A".to_string());
            }
            let position = match values.iter().position(|value| *value == number) {
                Some(index) => index as f64,
                None => {
                    let upper = values.iter().position(|value| *value > number).unwrap_or(0);
                    let low = values[upper - 1];
                    let high = values[upper];
                    (upper - 1) as f64 + (number - low) / (high - low)
                }
            };
            Value::Number((position + 1.0) / (values.len() as f64 + 1.0))
        }
        // PERCENTRANK.INC: position of a value in 0..1, interpolating between neighbours.
        "percentrank" => {
            if args.len() < 2 || args.len() > 3 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let Some(number) = args.get(1).and_then(|arg| eval(arg, ctx).number()) else {
                return Value::Error;
            };
            if values.is_empty() {
                return Value::Error;
            }
            let mut values = values;
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if number < values[0] || number > *values.last().unwrap() {
                return Value::Text("#N/A".to_string());
            }
            let position = match values.iter().position(|value| *value == number) {
                Some(index) => index as f64,
                None => {
                    let upper = values.iter().position(|value| *value > number).unwrap_or(0);
                    let low = values[upper - 1];
                    let high = values[upper];
                    (upper - 1) as f64 + (number - low) / (high - low)
                }
            };
            if values.len() == 1 {
                return Value::Number(0.0);
            }
            let significance = args
                .get(2)
                .and_then(|arg| eval(arg, ctx).number())
                .unwrap_or(6.0);
            if !significance.is_finite() || !(1.0..=15.0).contains(&significance) {
                return Value::Error;
            }
            let factor = 10f64.powi(significance.trunc() as i32);
            Value::Number((position / (values.len() - 1) as f64 * factor).trunc() / factor)
        }
        // GEOMEAN/HARMEAN over a same-row range; non-positive inputs are #VALUE!.
        "geomean" | "harmmean" => {
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            if values.is_empty() || values.iter().any(|value| *value <= 0.0) {
                return Value::Error;
            }
            let value = if lower == "geomean" {
                (values.iter().map(|value| value.ln()).sum::<f64>() / values.len() as f64).exp()
            } else {
                values.len() as f64 / values.iter().map(|value| 1.0 / value).sum::<f64>()
            };
            Value::Number(value)
        }
        // MODE.SNGL over a same-row range: the most frequent numeric value; ties pick the
        // smallest so the answer is deterministic, and no repeated value is #VALUE!.
        "mode" => {
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            if values.is_empty() {
                return Value::Error;
            }
            let mut counts: Vec<(f64, usize)> = Vec::new();
            for value in values {
                match counts.iter_mut().find(|(seen, _)| *seen == value) {
                    Some((_, count)) => *count += 1,
                    None => counts.push((value, 1)),
                }
            }
            let most = counts.iter().map(|(_, count)| *count).max().unwrap_or(0);
            if most < 2 {
                return Value::Error;
            }
            let mut winners = counts
                .iter()
                .filter(|(_, count)| *count == most)
                .map(|(value, _)| *value);
            let mut best = winners.next().unwrap_or(f64::NAN);
            for candidate in winners {
                if candidate < best {
                    best = candidate;
                }
            }
            Value::Number(best)
        }
        // Whole-column aggregates: the pre-pass installed the totals before the loop
        // started, so every row reads the same number in O(1). The argument is a column
        // name taken from the syntax tree, exactly like `lag`.
        "sum" | "average" | "avg" | "count" => {
            // `sum(B2:C2)` reads this row's cells between two columns; it needs no
            // pre-pass, so it works even where the state is absent (window reads).
            if let Some(Expr::Range { from, to }) = args.first() {
                let Some(values) = range_values(from, to, ctx) else {
                    return Value::Error;
                };
                return match lower.as_str() {
                    "sum" => Value::Number(values.iter().sum()),
                    "count" => Value::Number(values.len() as f64),
                    _ if values.is_empty() => Value::Error,
                    _ => Value::Number(values.iter().sum::<f64>() / values.len() as f64),
                };
            }
            let Some(state) = ctx.state else {
                return Value::Error;
            };
            let Some(Expr::Column(column)) = args.first() else {
                return Value::Error;
            };
            let Some((sum, count)) = state.totals(column) else {
                return Value::Error;
            };
            match lower.as_str() {
                "sum" => Value::Number(sum),
                "count" => Value::Number(count as f64),
                // Average of an empty column is #DIV/0! in a spreadsheet; #VALUE! is the
                // only error this engine has, and a wrong answer would be worse.
                _ if count > 0 => Value::Number(sum / count as f64),
                _ => Value::Error,
            }
        }
        "lag" => {
            let Some(state) = ctx.state else {
                return Value::Error;
            };
            // The argument is a column *name*, not its value: `lag(amount)` means the
            // amount of the previous row, so the name comes from the syntax tree.
            let column = match args.first() {
                Some(Expr::Column(name)) => name.clone(),
                _ => return Value::Error,
            };
            let distance = match args.get(1) {
                None => 1,
                Some(expr) => match eval(expr, ctx).number() {
                    Some(n) if n >= 1.0 => n as usize,
                    _ => return Value::Error,
                },
            };
            match state.lagged(&column, distance) {
                Some(value) => match value.trim().parse::<f64>() {
                    Ok(n) if n.is_finite() => Value::Number(n),
                    _ if value.trim().is_empty() => Value::Number(0.0),
                    _ => Value::Text(value.to_string()),
                },
                // Nothing that far back: the first rows of a file genuinely have no
                // previous value, and saying so is better than inventing a zero.
                None => Value::Error,
            }
        }
        "running_total" | "runningtotal" | "cumulative" | "cumsum" => {
            let Some(state) = ctx.state else {
                return Value::Error;
            };
            let column = match args.first() {
                Some(Expr::Column(name)) => name.clone(),
                _ => return Value::Error,
            };
            Value::Number(state.sum(&column))
        }
        "if" => {
            if args.len() < 2 {
                return Value::Error;
            }
            // Lazy: only the chosen branch is evaluated.
            if truthy(&eval(&args[0], ctx)) {
                eval(&args[1], ctx)
            } else if args.len() >= 3 {
                eval(&args[2], ctx)
            } else {
                Value::Bool(false)
            }
        }
        // IFS(cond1, value1, cond2, value2, ...): first true pair wins; no match is #N/A.
        "ifs" => {
            if args.len() < 2 || !args.len().is_multiple_of(2) {
                return Value::Error;
            }
            for pair in args.chunks(2) {
                if truthy(&eval(&pair[0], ctx)) {
                    return eval(&pair[1], ctx);
                }
            }
            Value::Text("#N/A".to_string())
        }
        // SWITCH(expr, case1, value1, ..., [default]): first text-equal case wins.
        "switch" => {
            if args.len() < 3 {
                return Value::Error;
            }
            let subject = eval(&args[0], ctx).text();
            let pairs_end = if (args.len() - 1) % 2 == 1 {
                args.len() - 1
            } else {
                args.len()
            };
            let mut index = 1;
            while index + 1 < pairs_end + 1 && index + 1 < args.len() {
                if subject == eval(&args[index], ctx).text() {
                    return eval(&args[index + 1], ctx);
                }
                index += 2;
            }
            if pairs_end < args.len() {
                eval(&args[pairs_end], ctx)
            } else {
                Value::Text("#N/A".to_string())
            }
        }
        "choose" => {
            let Some(index) = args.first().and_then(|arg| eval(arg, ctx).number()) else {
                return Value::Error;
            };
            let index = index.trunc();
            if index < 1.0 || index as usize >= args.len() {
                return Value::Error;
            }
            eval(&args[index as usize], ctx)
        }
        // NA() is the deliberate #N/A constant, so IFNA/IFERROR can be tested and
        // composed without inventing a lookup miss.
        "na" => Value::Text("#N/A".to_string()),
        "and" => {
            if args.is_empty() {
                return Value::Error;
            }
            for arg in args {
                if !truthy(&eval(arg, ctx)) {
                    return Value::Bool(false);
                }
            }
            Value::Bool(true)
        }
        "or" => {
            if args.is_empty() {
                return Value::Error;
            }
            for arg in args {
                if truthy(&eval(arg, ctx)) {
                    return Value::Bool(true);
                }
            }
            Value::Bool(false)
        }
        "xor" => {
            if args.is_empty() {
                return Value::Error;
            }
            let mut true_count = 0usize;
            for arg in args {
                if truthy(&eval(arg, ctx)) {
                    true_count += 1;
                }
            }
            Value::Bool(true_count % 2 == 1)
        }
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "not" => match args.first() {
            Some(arg) => Value::Bool(!truthy(&eval(arg, ctx))),
            None => Value::Error,
        },
        "isblank" | "isempty" => match args.first() {
            Some(arg) => match eval(arg, ctx) {
                Value::Empty => Value::Bool(true),
                Value::Text(t) => Value::Bool(t.trim().is_empty()),
                _ => Value::Bool(false),
            },
            None => Value::Error,
        },
        "isnumber" => match args.first() {
            Some(arg) => Value::Bool(matches!(eval(arg, ctx), Value::Number(_))),
            None => Value::Error,
        },
        "iserror" => match args.first() {
            Some(arg) => {
                let value = eval(arg, ctx);
                // ref/vlookup signal a miss with the text "#N/A", so iserror owns both
                // spellings; otherwise a lookup miss is invisible to IFERROR-style tests.
                Value::Bool(
                    matches!(value, Value::Error)
                        || matches!(&value, Value::Text(text) if text == "#N/A"),
                )
            }
            None => Value::Error,
        },
        // ISNA is the narrow test: ref/vlookup spell a miss as the text "#N/A", so
        // that spelling is the only thing it accepts (expression errors are ISERROR's).
        "isna" => match args.first() {
            Some(arg) => {
                let value = eval(arg, ctx);
                Value::Bool(matches!(&value, Value::Text(text) if text == "#N/A"))
            }
            None => Value::Error,
        },
        "istext" => match args.first() {
            Some(arg) => Value::Bool(matches!(eval(arg, ctx), Value::Text(_))),
            None => Value::Error,
        },
        "iferror" => match args.first() {
            Some(arg) => {
                let value = eval(arg, ctx);
                // Excel's IFERROR also catches the "#N/A" text used by ref/lookup
                // misses, not just #VALUE!.
                let is_error = matches!(value, Value::Error)
                    || matches!(&value, Value::Text(text) if text == "#N/A");
                if is_error {
                    match args.get(1) {
                        Some(fallback) => eval(fallback, ctx),
                        None => Value::Bool(false),
                    }
                } else {
                    value
                }
            }
            None => Value::Error,
        },
        // IFNA is the narrow fallback: only #N/A, not #VALUE!.
        "ifna" => match (args.first(), args.get(1)) {
            (Some(arg), Some(fallback)) => match eval(arg, ctx) {
                Value::Text(text) if text == "#N/A" => eval(fallback, ctx),
                value => value,
            },
            _ => Value::Error,
        },

        // ---- numeric -------------------------------------------------------
        "round" => {
            let Some(x) = args.first().and_then(|a| eval(a, ctx).number()) else {
                return Value::Error;
            };
            let digits = args
                .get(1)
                .and_then(|a| eval(a, ctx).number())
                .unwrap_or(0.0) as i32;
            let factor = 10f64.powi(digits);
            Value::Number((x * factor).round() / factor)
        }
        "abs" => match args.first().map(|a| eval(a, ctx)) {
            Some(Value::Number(n)) => Value::Number(n.abs()),
            _ => Value::Error,
        },
        "floor" => match args.first().map(|a| eval(a, ctx).number()) {
            Some(Some(n)) => Value::Number(n.floor()),
            _ => Value::Error,
        },
        "ceil" | "ceiling" => match args.first().map(|a| eval(a, ctx).number()) {
            Some(Some(n)) => Value::Number(n.ceil()),
            _ => Value::Error,
        },
        "int" | "trunc" => match args.first().map(|a| eval(a, ctx).number()) {
            Some(Some(n)) => Value::Number(n.trunc()),
            _ => Value::Error,
        },
        "sqrt" => match args.first().map(|a| eval(a, ctx).number()) {
            Some(Some(n)) if n >= 0.0 => Value::Number(n.sqrt()),
            _ => Value::Error,
        },
        "mod" => {
            let Some(x) = args.first().and_then(|a| eval(a, ctx).number()) else {
                return Value::Error;
            };
            let Some(y) = args.get(1).and_then(|a| eval(a, ctx).number()) else {
                return Value::Error;
            };
            if y == 0.0 {
                Value::Error
            } else {
                Value::Number(x % y)
            }
        }
        // GCD/LCM/FACT/COMBIN/PERMUT: non-negative integer domain.
        "gcd" | "lcm" => {
            if args.is_empty() {
                return Value::Error;
            }
            let mut result: u64 = if lower == "gcd" { 0 } else { 1 };
            for arg in args {
                let Some(number) = eval(arg, ctx).number() else {
                    return Value::Error;
                };
                let Some(number) = nonnegative_integer(number) else {
                    return Value::Error;
                };
                if lower == "gcd" {
                    result = gcd_u64(result, number);
                } else if result == 0 || number == 0 {
                    result = 0;
                } else {
                    let Some(product) = result.checked_mul(number) else {
                        return Value::Error;
                    };
                    result = product / gcd_u64(result, number);
                }
            }
            Value::Number(result as f64)
        }
        "fact" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let Some(number) = nonnegative_integer(number) else {
                return Value::Error;
            };
            if number > 170 {
                return Value::Error;
            }
            let mut result = 1.0f64;
            for value in 1..=number {
                result *= value as f64;
            }
            Value::Number(result)
        }
        "combin" | "permut" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(n), Some(k)) = (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            let (Some(n), Some(k)) = (nonnegative_integer(n), nonnegative_integer(k)) else {
                return Value::Error;
            };
            if k > n {
                return Value::Error;
            }
            let mut result = 1.0f64;
            if lower == "combin" {
                let k = k.min(n - k);
                for step in 1..=k {
                    result *= (n - k + step) as f64 / step as f64;
                }
            } else {
                for step in 0..k {
                    result *= (n - step) as f64;
                }
            }
            Value::Number(result.round())
        }
        // BITAND/BITOR/BITXOR: 48-bit non-negative integers.
        "bitand" | "bitor" | "bitxor" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(left), Some(right)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            let (Some(left), Some(right)) = (bit_integer(left), bit_integer(right)) else {
                return Value::Error;
            };
            Value::Number(match lower.as_str() {
                "bitand" => (left & right) as f64,
                "bitor" => (left | right) as f64,
                _ => (left ^ right) as f64,
            })
        }
        "min" | "max" => {
            // `min(amount)` means the whole column, exactly like `sum(amount)`; a bare
            // column argument would otherwise read this row's cell and call it a day.
            if let Some(Expr::Column(column)) = args.first() {
                if args.len() == 1 {
                    let Some(state) = ctx.state else {
                        return Value::Error;
                    };
                    let Some((min, max)) = state.bounds(column) else {
                        return Value::Error;
                    };
                    return Value::Number(if lower == "min" { min } else { max });
                }
            }
            if let Some(Expr::Range { from, to }) = args.first() {
                let Some(values) = range_values(from, to, ctx) else {
                    return Value::Error;
                };
                let Some(picked) = (if lower == "min" {
                    values.iter().copied().reduce(f64::min)
                } else {
                    values.iter().copied().reduce(f64::max)
                }) else {
                    return Value::Error;
                };
                return Value::Number(picked);
            }
            let mut nums = Vec::new();
            for arg in args {
                match eval(arg, ctx).number() {
                    Some(n) => nums.push(n),
                    None => return Value::Error,
                }
            }
            if nums.is_empty() {
                return Value::Error;
            }
            let picked = if lower == "min" {
                nums.iter().cloned().fold(f64::INFINITY, f64::min)
            } else {
                nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            };
            Value::Number(picked)
        }

        // ---- dates (ISO prefixes, the only shape CSV dates reliably have) ---
        "year" | "month" | "day" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => match date_part(&v.text(), &lower) {
                Some(n) => Value::Number(n as f64),
                None => Value::Error,
            },
            None => Value::Error,
        },
        // WEEKNUM with the default Sunday start (Excel's return_type 1).
        "weeknum" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => match parse_date(&v.text()) {
                Some((year, month, day)) => {
                    let day_of_year =
                        days_from_civil(year, month, day) - days_from_civil(year, 1, 1) + 1;
                    let return_type = args
                        .get(1)
                        .and_then(|arg| eval(arg, ctx).number())
                        .unwrap_or(1.0);
                    if return_type as u32 == 21 {
                        return Value::Number(iso_week_number(year, month, day) as f64);
                    }
                    // return_type 1 starts on Sunday, 2 starts on Monday.
                    let offset = match return_type as u32 {
                        1 => weekday(year, 1, 1) - 1,
                        2 => (weekday(year, 1, 1) + 5) % 7,
                        _ => return Value::Error,
                    };
                    Value::Number((((day_of_year + offset as i64 - 1) / 7) + 1) as f64)
                }
                None => Value::Error,
            },
            None => Value::Error,
        },
        "datestring" | "datekey" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => {
                let text = v.text();
                match date_part(&text, "year") {
                    Some(_) => Value::Text(text.chars().take(10).collect()),
                    None => Value::Error,
                }
            }
            None => Value::Error,
        },
        "days360" => match (args.first(), args.get(1)) {
            (Some(start), Some(end)) => {
                let (Some(start), Some(end)) = (
                    parse_date(&eval(start, ctx).text()),
                    parse_date(&eval(end, ctx).text()),
                ) else {
                    return Value::Error;
                };
                if end < start {
                    return Value::Error;
                }
                let european = args
                    .get(2)
                    .and_then(|arg| eval(arg, ctx).number())
                    .map(|method| method != 0.0)
                    .unwrap_or(false);
                Value::Number(if european {
                    days_360_european(start, end)
                } else {
                    days_360(start, end)
                })
            }
            _ => Value::Error,
        },
        "datevalue" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => match parse_date(&v.text()) {
                Some((year, month, day)) => {
                    let mut serial =
                        days_from_civil(year, month, day) - days_from_civil(1899, 12, 30);
                    // Excel's 1900 system has a fake 1900-02-29; dates before
                    // 1900-03-01 sit one serial lower than the civil count.
                    if (year, month, day) < (1900, 3, 1) {
                        serial -= 1;
                    }
                    if (year, month, day) == (1900, 2, 29) {
                        serial = 60;
                    }
                    Value::Number(serial as f64)
                }
                None => Value::Error,
            },
            None => Value::Error,
        },
        "hour" | "minute" | "second" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => match time_part(&v.text(), &lower) {
                Some(n) => Value::Number(n as f64),
                None => Value::Error,
            },
            None => Value::Error,
        },
        "timevalue" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => match time_fraction(&v.text()) {
                Some(fraction) => Value::Number(fraction),
                None => Value::Error,
            },
            None => Value::Error,
        },
        // Volatile cells: the clock is read once per evaluation. The CSV export
        // never persists them, so a rerun is expected to differ from yesterday.
        "today" => Value::Text(now_parts().0),
        "now" => Value::Text(format!("{} {}", now_parts().0, now_parts().1)),
        "time" => match (
            args.first().and_then(|a| eval(a, ctx).number()),
            args.get(1).and_then(|a| eval(a, ctx).number()),
            args.get(2).and_then(|a| eval(a, ctx).number()),
        ) {
            (Some(hour), Some(minute), Some(second)) => {
                if hour < 0.0
                    || minute < 0.0
                    || second < 0.0
                    || hour > 23.0
                    || minute > 59.0
                    || second > 59.0
                {
                    return Value::Error;
                }
                Value::Number(
                    (hour.trunc() * 3_600.0 + minute.trunc() * 60.0 + second.trunc()) / 86_400.0,
                )
            }
            _ => Value::Error,
        },
        "isoweeknum" => match args.first().map(|a| eval(a, ctx)) {
            Some(value) => match parse_date(&value.text()) {
                Some((year, month, day)) => Value::Number(iso_week_number(year, month, day) as f64),
                None => Value::Error,
            },
            None => Value::Error,
        },
        "yearfrac" => match (args.first(), args.get(1)) {
            (Some(start), Some(end)) => {
                let (Some(start), Some(end)) = (
                    parse_date(&eval(start, ctx).text()),
                    parse_date(&eval(end, ctx).text()),
                ) else {
                    return Value::Error;
                };
                let basis = args
                    .get(2)
                    .and_then(|a| eval(a, ctx).number())
                    .unwrap_or(0.0);
                match year_fraction(start, end, basis) {
                    Some(value) => Value::Number(value),
                    None => Value::Error,
                }
            }
            _ => Value::Error,
        },
        "datedif" => match (args.first(), args.get(1), args.get(2)) {
            (Some(start), Some(end), Some(unit)) => {
                let (Some(start), Some(end)) = (
                    parse_date(&eval(start, ctx).text()),
                    parse_date(&eval(end, ctx).text()),
                ) else {
                    return Value::Error;
                };
                let unit = eval(unit, ctx).text().to_ascii_uppercase();
                match date_difference(start, end, &unit) {
                    Some(value) => Value::Number(value as f64),
                    None => Value::Error,
                }
            }
            _ => Value::Error,
        },

        // ---- text ----------------------------------------------------------
        "len" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => Value::Number(v.text().chars().count() as f64),
            None => Value::Error,
        },
        "upper" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => Value::Text(v.text().to_uppercase()),
            None => Value::Error,
        },
        "lower" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => Value::Text(v.text().to_lowercase()),
            None => Value::Error,
        },
        "trim" => match args.first().map(|a| eval(a, ctx)) {
            Some(v) => Value::Text(v.text().trim().to_string()),
            None => Value::Error,
        },
        // TEXT(value, format): reuse the same NumberFormat the viewer uses.
        "text" => match (args.first(), args.get(1)) {
            (Some(value), Some(format)) => {
                let spec = eval(format, ctx).text();
                match NumberFormat::parse(&spec) {
                    Ok(format) => Value::Text(format.render(&eval(value, ctx).text())),
                    Err(_) => Value::Error,
                }
            }
            _ => Value::Error,
        },
        // FIXED/DOLLAR are text formatting, not number formatting; they return
        // strings a downstream concatenation can use.
        "fixed" | "dollar" => {
            if args.is_empty() || args.len() > if lower == "dollar" { 2 } else { 3 } {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let decimals = args
                .get(1)
                .and_then(|arg| eval(arg, ctx).number())
                .unwrap_or(2.0);
            if !number.is_finite() || !decimals.is_finite() {
                return Value::Error;
            }
            let decimals = decimals.trunc() as i32;
            if !(-15..=15).contains(&decimals) {
                return Value::Error;
            }
            let no_commas = args
                .get(2)
                .map(|arg| truthy(&eval(arg, ctx)))
                .unwrap_or(false);
            let body = fixed_body(
                if lower == "dollar" {
                    number.abs()
                } else {
                    number
                },
                decimals,
                no_commas,
            );
            if lower == "dollar" {
                let currency = format!("${body}");
                return Value::Text(if number < 0.0 {
                    format!("({currency})")
                } else {
                    currency
                });
            }
            Value::Text(body)
        }
        "numbervalue" => {
            if args.is_empty() || args.len() > 3 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let decimal = args
                .get(1)
                .map(|arg| eval(arg, ctx).text())
                .unwrap_or_else(|| ".".to_string());
            let group = args
                .get(2)
                .map(|arg| eval(arg, ctx).text())
                .unwrap_or_else(|| ",".to_string());
            let Some(decimal) = single_separator(&decimal) else {
                return Value::Error;
            };
            let Some(group) = single_separator(&group) else {
                return Value::Error;
            };
            if decimal == group {
                return Value::Error;
            }
            let mut text = text.trim().to_string();
            let mut negative = false;
            if text.starts_with('(') && text.ends_with(')') {
                negative = true;
                text = text[1..text.len() - 1].to_string();
            }
            let mut percent = false;
            if text.ends_with('%') {
                percent = true;
                text.pop();
            }
            if text.matches(decimal).count() > 1 {
                return Value::Error;
            }
            let normalized = text.replace(group, "").replace(decimal, ".");
            if normalized.is_empty() {
                return Value::Number(0.0);
            }
            match normalized.parse::<f64>() {
                Ok(value) if value.is_finite() => Value::Number(
                    value * if negative { -1.0 } else { 1.0 } * if percent { 0.01 } else { 1.0 },
                ),
                _ => Value::Error,
            }
        }
        "textbefore" => text_around(args, ctx, true),
        "textafter" => text_around(args, ctx, false),
        "left" => text_slice(args, ctx, |s, n| s.chars().take(n).collect()),
        "right" => text_slice(args, ctx, |s, n| {
            let total = s.chars().count();
            s.chars().skip(total.saturating_sub(n)).collect()
        }),
        "mid" => {
            let Some(text) = args.first().map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            let start = args
                .get(1)
                .and_then(|a| eval(a, ctx).number())
                .unwrap_or(1.0)
                .max(1.0) as usize;
            let length = args
                .get(2)
                .and_then(|a| eval(a, ctx).number())
                .map(|n| n.max(0.0) as usize);
            let taken: String = text
                .chars()
                .skip(start.saturating_sub(1))
                .take(length.unwrap_or(usize::MAX))
                .collect();
            Value::Text(taken)
        }
        // Excel REPLACE(text, start_num, num_chars, new_text): a positional edit,
        // distinct from SUBSTITUTE's occurrence-based search.
        "replace" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let Some(text) = args.first().map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            let Some(start) = args.get(1).and_then(|a| eval(a, ctx).number()) else {
                return Value::Error;
            };
            let Some(count) = args.get(2).and_then(|a| eval(a, ctx).number()) else {
                return Value::Error;
            };
            let Some(new_text) = args.get(3).map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            if !start.is_finite() || start < 1.0 || !count.is_finite() || count < 0.0 {
                return Value::Error;
            }
            let chars: Vec<char> = text.chars().collect();
            let start = start.trunc() as usize;
            let count = count.trunc() as usize;
            if start > chars.len() + 1 {
                return Value::Text(text);
            }
            let prefix: String = chars[..start - 1].iter().collect();
            let suffix: String = chars[(start - 1 + count).min(chars.len())..]
                .iter()
                .collect();
            Value::Text(format!("{prefix}{new_text}{suffix}"))
        }
        // Excel SUBSTITUTE(text, old_text, new_text[, instance_num]): replace all
        // occurrences, or only the 1-based N-th one.
        "substitute" => {
            if args.len() < 3 || args.len() > 4 {
                return Value::Error;
            }
            let Some(text) = args.first().map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            let Some(old_text) = args.get(1).map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            let Some(new_text) = args.get(2).map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            if old_text.is_empty() {
                return Value::Text(text);
            }
            let instance = match args.get(3).and_then(|a| eval(a, ctx).number()) {
                Some(number) if number.is_finite() && number >= 1.0 => {
                    Some(number.trunc() as usize)
                }
                Some(_) => return Value::Error,
                None => None,
            };
            let Some(instance) = instance else {
                return Value::Text(text.replace(&old_text, &new_text));
            };
            let hits: Vec<usize> = text
                .match_indices(&old_text)
                .map(|(index, _)| index)
                .collect();
            let Some(index) = hits.get(instance - 1).copied() else {
                return Value::Text(text);
            };
            Value::Text(format!(
                "{}{}{}",
                &text[..index],
                new_text,
                &text[index + old_text.len()..]
            ))
        }
        "concat" => {
            let mut out = String::new();
            for arg in args {
                out.push_str(&eval(arg, ctx).text());
            }
            Value::Text(out)
        }
        // TEXTJOIN(separator, ignore_empty, value...) joins the rest with the separator.
        "textjoin" => {
            let Some(separator) = args.first().map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            let Some(ignore_empty) = args.get(1).map(|a| eval(a, ctx).number()) else {
                return Value::Error;
            };
            let Some(ignore_empty) = ignore_empty else {
                return Value::Error;
            };
            if args.len() < 3 {
                return Value::Error;
            }
            let mut parts = Vec::new();
            for arg in args.iter().skip(2) {
                let text = eval(arg, ctx).text();
                if ignore_empty != 0.0 && text.is_empty() {
                    continue;
                }
                parts.push(text);
            }
            Value::Text(parts.join(&separator))
        }
        // CHAR(65) = "A"; CODE("A") = 65.
        "char" => match args.first().map(|a| eval(a, ctx).number()) {
            Some(Some(code)) if (0.0..=0x10FFFF as f64).contains(&code) => {
                match char::from_u32(code as u32) {
                    Some(ch) => Value::Text(ch.to_string()),
                    None => Value::Error,
                }
            }
            _ => Value::Error,
        },
        "code" => match args.first().map(|a| eval(a, ctx).text()) {
            Some(text) => match text.chars().next() {
                Some(ch) => Value::Number(ch as u32 as f64),
                None => Value::Error,
            },
            None => Value::Error,
        },
        // UNICHAR/UNICODE cover the non-ASCII code points CHAR/CODE cannot.
        "unichar" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(code) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !code.is_finite() || code < 1.0 || code > 0x10FFFF as f64 {
                return Value::Error;
            }
            match char::from_u32(code.trunc() as u32) {
                Some(ch) => Value::Text(ch.to_string()),
                None => Value::Error,
            }
        }
        "unicode" => {
            if args.len() != 1 {
                return Value::Error;
            }
            match eval(&args[0], ctx).text().chars().next() {
                Some(ch) => Value::Number(ch as u32 as f64),
                None => Value::Error,
            }
        }

        // Alias for `mid`, which is the name people reach for first.
        "substr" => call("mid", args, ctx),
        // ---- more of the spreadsheet's everyday functions --------------------
        "concatenate" => call("concat", args, ctx),
        "sign" => match args.first().map(|a| eval(a, ctx).number()) {
            Some(Some(n)) => Value::Number(if n > 0.0 {
                1.0
            } else if n < 0.0 {
                -1.0
            } else {
                0.0
            }),
            _ => Value::Error,
        },
        "power" | "pow" => match (
            args.first().map(|a| eval(a, ctx).number()),
            args.get(1).map(|a| eval(a, ctx).number()),
        ) {
            (Some(Some(base)), Some(Some(exp))) => Value::Number(base.powf(exp)),
            _ => Value::Error,
        },
        "exp" => match args.first().map(|a| eval(a, ctx).number()) {
            Some(Some(n)) => Value::Number(n.exp()),
            _ => Value::Error,
        },
        "ln" => match args.first().map(|a| eval(a, ctx).number()) {
            Some(Some(n)) if n > 0.0 => Value::Number(n.ln()),
            _ => Value::Error,
        },
        // Excel's LOG(number, [base]) defaults to base 10; LN is the natural log.
        "log" => match args.first().map(|a| eval(a, ctx).number()) {
            Some(Some(n)) if n > 0.0 => {
                let base = args.get(1).and_then(|a| eval(a, ctx).number());
                match base {
                    None => Value::Number(n.log10()),
                    Some(b) if b > 0.0 && b != 1.0 => Value::Number(n.log(b)),
                    _ => Value::Error,
                }
            }
            _ => Value::Error,
        },
        // `roundup`/`rounddown` round away from/towards zero, like Excel, and honour digits.
        "roundup" | "rounddown" => round_directed(&lower, args, ctx),
        "exact" => match (
            args.first().map(|a| eval(a, ctx).text()),
            args.get(1).map(|a| eval(a, ctx).text()),
        ) {
            (Some(a), Some(b)) => Value::Bool(a == b),
            _ => Value::Error,
        },
        "find" => find_in(args, ctx, true),
        "search" => find_in(args, ctx, false),
        "proper" => match args.first().map(|a| eval(a, ctx).text()) {
            Some(text) => Value::Text(
                text.split(' ')
                    .map(|word| {
                        let mut chars = word.chars();
                        match chars.next() {
                            Some(first) => {
                                format!("{}{}", first.to_uppercase(), chars.as_str().to_lowercase())
                            }
                            None => String::new(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            None => Value::Error,
        },
        "rept" => match (
            args.first().map(|a| eval(a, ctx).text()),
            args.get(1).map(|a| eval(a, ctx).number()),
        ) {
            (Some(text), Some(Some(times))) if (0.0..=10_000.0).contains(&times) => {
                Value::Text(text.repeat(times as usize))
            }
            _ => Value::Error,
        },
        "value" => match args.first().map(|a| eval(a, ctx).text()) {
            Some(text) => match text.trim().parse::<f64>() {
                Ok(n) if n.is_finite() => Value::Number(n),
                _ => Value::Error,
            },
            None => Value::Error,
        },
        // DATE(year, month, day) renders the ISO day a CSV date column can hold, so it
        // composes with year()/month()/day() rather than inventing a serial number.
        "date" => match (
            args.first().map(|a| eval(a, ctx).number()),
            args.get(1).map(|a| eval(a, ctx).number()),
            args.get(2).map(|a| eval(a, ctx).number()),
        ) {
            (Some(Some(y)), Some(Some(m)), Some(Some(d)))
                if (1.0..=9999.0).contains(&y) && (1.0..=12.0).contains(&m) && d >= 1.0 =>
            {
                Value::Text(format!("{:04}-{:02}-{:02}", y as u32, m as u32, d as u32))
            }
            _ => Value::Error,
        },
        // EDATE/EOMONTH keep the project's ISO-text date representation, so a result
        // can feed the next formula without a serial-number round trip.
        "edate" | "eomonth" => {
            let (Some(date), Some(months)) = (
                args.first().map(|a| eval(a, ctx).text()),
                args.get(1).map(|a| eval(a, ctx).number()),
            ) else {
                return Value::Error;
            };
            let Some(months) = months else {
                return Value::Error;
            };
            let Some((year, month, day)) = parse_date(&date) else {
                return Value::Error;
            };
            if !(-120000.0..=120000.0).contains(&months) {
                return Value::Error;
            }
            let shifted = add_months(year, month, months as i32);
            let last = days_in_month(shifted.0, shifted.1);
            let day_out = if lower == "eomonth" {
                last
            } else {
                day.min(last)
            };
            Value::Text(format!("{:04}-{:02}-{:02}", shifted.0, shifted.1, day_out))
        }
        // WEEKDAY follows the spreadsheet default: Sunday = 1 … Saturday = 7.
        "weekday" => match args.first().map(|a| eval(a, ctx).text()) {
            Some(date) => match parse_date(&date) {
                Some((year, month, day)) => {
                    let base = weekday(year, month, day);
                    let return_type = args
                        .get(1)
                        .and_then(|arg| eval(arg, ctx).number())
                        .unwrap_or(1.0);
                    let value = match return_type as u32 {
                        1 => base,
                        2 => ((base + 5) % 7) + 1,
                        3 => (base + 5) % 7,
                        _ => return Value::Error,
                    };
                    Value::Number(value as f64)
                }
                None => Value::Error,
            },
            None => Value::Error,
        },
        // DAYS(end, start) is end - start, like Excel's DAYS().
        "days" => match (
            args.first().map(|a| eval(a, ctx).text()),
            args.get(1).map(|a| eval(a, ctx).text()),
        ) {
            (Some(end), Some(start)) => match (parse_date(&end), parse_date(&start)) {
                (Some((ey, em, ed)), Some((sy, sm, sd))) => Value::Number(
                    (days_from_civil(ey, em, ed) - days_from_civil(sy, sm, sd)) as f64,
                ),
                _ => Value::Error,
            },
            _ => Value::Error,
        },

        // NETWORKDAYS(start, end) without a holiday list: weekdays only, both ends
        // included. Sunday=1..Saturday=7, so business days are 2..=6.
        "networkdays" => match (
            args.first().map(|a| eval(a, ctx).text()),
            args.get(1).map(|a| eval(a, ctx).text()),
        ) {
            (Some(start), Some(end)) => match (parse_date(&start), parse_date(&end)) {
                (Some((sy, sm, sd)), Some((ey, em, ed))) => {
                    let start_day = days_from_civil(sy, sm, sd);
                    let end_day = days_from_civil(ey, em, ed);
                    if end_day < start_day {
                        return Value::Error;
                    }
                    let total = end_day - start_day + 1;
                    let full_weeks = total / 7 * 5;
                    let remainder = total % 7;
                    let start_weekday = weekday(sy, sm, sd);
                    let mut extra = 0i64;
                    for step in 0..remainder {
                        let day = (start_weekday - 1 + step as i32) % 7 + 1;
                        if (2..=6).contains(&day) {
                            extra += 1;
                        }
                    }
                    let holidays = match holiday_days(&args[2..], ctx) {
                        Ok(holidays) => holidays,
                        Err(_) => return Value::Error,
                    };
                    let mut count = full_weeks + extra;
                    let mut unique = std::collections::HashSet::new();
                    for holiday in holidays {
                        if holiday >= start_day
                            && holiday <= end_day
                            && (2..=6).contains(&(((holiday + 4).rem_euclid(7) + 1) as i32))
                            && unique.insert(holiday)
                        {
                            count -= 1;
                        }
                    }
                    Value::Number(count as f64)
                }
                _ => Value::Error,
            },
            _ => Value::Error,
        },
        // WORKDAY(start, days): move by business days, skipping weekends (no holiday
        // list in the two-argument form). Negative days move backwards.
        "workday" => match (
            args.first().map(|a| eval(a, ctx).text()),
            args.get(1).and_then(|a| eval(a, ctx).number()),
        ) {
            (Some(start), Some(days)) if days.is_finite() => match parse_date(&start) {
                Some((year, month, day)) => {
                    let delta = days.trunc() as i64;
                    let mut civil = days_from_civil(year, month, day);
                    let step = delta.signum();
                    let mut left = delta.abs();
                    let holidays = match holiday_days(&args[2..], ctx) {
                        Ok(holidays) => holidays,
                        Err(_) => return Value::Error,
                    };
                    while left > 0 {
                        civil += step;
                        let weekday = (civil + 4).rem_euclid(7) + 1;
                        if (2..=6).contains(&weekday) && !holidays.contains(&civil) {
                            left -= 1;
                        }
                    }
                    let (y, m, d) = civil_from_days(civil);
                    Value::Text(format!("{y:04}-{m:02}-{d:02}"))
                }
                None => Value::Error,
            },
            _ => Value::Error,
        },
        // ---- row position ---------------------------------------------------
        "row" => Value::Number((ctx.row + 1) as f64),

        // HLOOKUP/LOOKUP on a single-row table. HLOOKUP's row_index must be 1;
        // LOOKUP is the vector form with an optional equal-width result range.
        "hlookup" => {
            if args.len() < 3 || args.len() > 4 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.get(1) else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let Some(row_index) = args.get(2).and_then(|arg| eval(arg, ctx).number()) else {
                return Value::Error;
            };
            if !row_index.is_finite() || row_index.trunc() != 1.0 {
                return Value::Error;
            }
            let exact = args
                .get(3)
                .map(|arg| !truthy(&eval(arg, ctx)))
                .unwrap_or(false);
            let lookup = eval(&args[0], ctx);
            let position = if exact {
                exact_position(&lookup, &cells)
            } else {
                approximate_position(&lookup, &cells, 1.0)
            };
            position
                .and_then(|index| cells.get(index).copied())
                .map(cell_as_value)
                .unwrap_or(Value::Text("#N/A".to_string()))
        }
        "lookup" => {
            if args.len() < 2 || args.len() > 3 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.get(1) else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let result_cells = match args.get(2) {
                Some(Expr::Range { from, to }) => {
                    let Some(cells) = range_cells(from, to, ctx) else {
                        return Value::Error;
                    };
                    Some(cells)
                }
                Some(_) => return Value::Error,
                None => None,
            };
            if let Some(result) = &result_cells {
                if result.len() != cells.len() {
                    return Value::Error;
                }
            }
            let lookup = eval(&args[0], ctx);
            let position = exact_position(&lookup, &cells)
                .or_else(|| approximate_position(&lookup, &cells, 1.0));
            position
                .and_then(|index| {
                    result_cells
                        .as_ref()
                        .and_then(|result| result.get(index).copied())
                        .or_else(|| cells.get(index).copied())
                })
                .map(cell_as_value)
                .unwrap_or(Value::Text("#N/A".to_string()))
        }
        // ---- row-local lookup ------------------------------------------------
        // INDEX(range, n) and INDEX(range, 1, n): pick the n-th cell of a
        // same-row range. A second row is not materialised, so row_num must be 1.
        "index" => {
            if args.len() != 2 && args.len() != 3 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let position = if args.len() == 3 {
                let Some(row) = eval(&args[1], ctx).number() else {
                    return Value::Error;
                };
                if !row.is_finite() || row.trunc() != 1.0 {
                    return Value::Error;
                }
                let Some(column) = eval(&args[2], ctx).number() else {
                    return Value::Error;
                };
                column
            } else {
                let Some(position) = eval(&args[1], ctx).number() else {
                    return Value::Error;
                };
                position
            };
            if !position.is_finite() || position < 1.0 {
                return Value::Error;
            }
            let position = position.trunc() as usize;
            let Some(cell) = cells.get(position - 1) else {
                return Value::Error;
            };
            cell_as_value(cell)
        }
        // MATCH(value, range[, match_type]) / XMATCH(value, range[, match_mode])
        // on a same-row range. 0 is exact; 1/-1 are the usual sorted neighbours.
        "match" | "xmatch" => {
            if args.len() < 2 || args.len() > 4 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.get(1) else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let lookup = eval(&args[0], ctx);
            let mode = args
                .get(2)
                .and_then(|arg| eval(arg, ctx).number())
                .unwrap_or(if lower == "xmatch" { 0.0 } else { 1.0 });
            if !mode.is_finite() || ![-1.0, 0.0, 1.0].contains(&mode) {
                return Value::Error;
            }
            if lower == "xmatch" {
                if let Some(search_mode) = args.get(3).and_then(|arg| eval(arg, ctx).number()) {
                    if !search_mode.is_finite() || search_mode.trunc() != 1.0 {
                        return Value::Error;
                    }
                }
            }
            if mode == 0.0 {
                return match exact_position(&lookup, &cells) {
                    Some(position) => Value::Number((position + 1) as f64),
                    None => Value::Text("#N/A".to_string()),
                };
            }
            // XMATCH's +1 means "exact or larger" while MATCH's +1 means
            // "largest <= lookup"; flip the direction for the approximate path.
            if lower == "xmatch" {
                if let Some(position) = exact_position(&lookup, &cells) {
                    return Value::Number((position + 1) as f64);
                }
            }
            let direction = if lower == "xmatch" { -mode } else { mode };
            match approximate_position(&lookup, &cells, direction) {
                Some(position) => Value::Number((position + 1) as f64),
                None => Value::Text("#N/A".to_string()),
            }
        }
        // ---- cross-sheet reference -----------------------------------------
        // ref("customers.csv", "cust_no", "name"): this row's cust_no looked up
        // in that sheet. The referenced sheet is already in memory; the current
        // sheet stays streamed.
        // XLOOKUP's if_not_found: a miss returns the caller's fallback instead of
        // the "#N/A" text that `iferror` cannot catch.
        "xlookup" => {
            if args.len() < 3 || args.len() > 4 {
                return Value::Error;
            }
            let (Expr::Text(sheet), Expr::Text(key_column), Expr::Text(value_column)) =
                (&args[0], &args[1], &args[2])
            else {
                return Value::Error;
            };
            let Some(reference) = ctx.refs.get(sheet, key_column, value_column) else {
                return Value::Error;
            };
            let key = match ctx.cells.get(key_column) {
                Some(k) => k.trim().to_string(),
                None => return Value::Error,
            };
            match reference.get(&key) {
                Some(value) => Value::Text(value.clone()),
                None => match args.get(3) {
                    Some(fallback) => eval(fallback, ctx),
                    None => Value::Text("#N/A".to_string()),
                },
            }
        }
        "ref" | "vlookup" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Expr::Text(sheet), Expr::Text(key_column), Expr::Text(value_column)) =
                (&args[0], &args[1], &args[2])
            else {
                // Dynamic sheet names would make preloading impossible, so they
                // are refused rather than silently returning nothing.
                return Value::Error;
            };
            let Some(reference) = ctx.refs.get(sheet, key_column, value_column) else {
                return Value::Error;
            };
            let key = match ctx.cells.get(key_column) {
                Some(k) => k.trim().to_string(),
                None => return Value::Error,
            };
            match reference.get(&key) {
                Some(value) => Value::Text(value.clone()),
                None => Value::Text("#N/A".to_string()),
            }
        }
        _ => Value::Error,
    }
}

/// Row-local SUMIFS/COUNTIFS family: `sumifs(A2:C2, D2:F2, ">0", G2:I2, "<>x")`
/// sums the positions whose criteria cells all match. Widths must agree.
fn multi_range(name: &str, args: &[Expr], ctx: &Ctx<'_>) -> Value {
    let op = match name {
        "sumifs" => SetOp::Sum,
        "averageifs" => SetOp::Average,
        "minifs" => SetOp::Min,
        "maxifs" => SetOp::Max,
        _ => SetOp::Count,
    };
    let (measure, start) = if matches!(op, SetOp::Count) {
        (None, 0usize)
    } else {
        let Some(Expr::Range { from, to }) = args.first() else {
            return Value::Error;
        };
        let Some(cells) = range_cells(from, to, ctx) else {
            return Value::Error;
        };
        (Some(cells), 1usize)
    };
    let mut pairs: Vec<(Vec<&str>, &crate::filter::FilterSpec)> = Vec::new();
    let mut index = start;
    while index + 1 < args.len() {
        let Expr::Range { from, to } = &args[index] else {
            return Value::Error;
        };
        let Expr::Criteria { spec, .. } = &args[index + 1] else {
            return Value::Error;
        };
        let Some(cells) = range_cells(from, to, ctx) else {
            return Value::Error;
        };
        pairs.push((cells, spec));
        index += 2;
    }
    if pairs.is_empty() {
        return Value::Error;
    }
    let width = measure
        .as_ref()
        .map(|cells| cells.len())
        .unwrap_or(pairs[0].0.len());
    if width == 0
        || pairs.iter().any(|(cells, _)| cells.len() != width)
        || measure.as_ref().is_some_and(|cells| cells.len() != width)
    {
        return Value::Error;
    }
    let mut matched = 0usize;
    let mut numeric = 0usize;
    let mut sum = 0.0;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for position in 0..width {
        if !pairs.iter().all(|(cells, spec)| {
            crate::filter::matches(cells.get(position).copied().unwrap_or(""), spec)
        }) {
            continue;
        }
        matched += 1;
        let Some(measure) = measure.as_ref() else {
            continue;
        };
        let Some(value) = measure.get(position) else {
            continue;
        };
        let Ok(value) = value.trim().parse::<f64>() else {
            continue;
        };
        if !value.is_finite() {
            continue;
        }
        numeric += 1;
        sum += value;
        min = min.min(value);
        max = max.max(value);
    }
    match op {
        SetOp::Count => Value::Number(matched as f64),
        SetOp::Sum => Value::Number(sum),
        SetOp::Average => {
            if numeric == 0 {
                Value::Error
            } else {
                Value::Number(sum / numeric as f64)
            }
        }
        SetOp::Min | SetOp::Max => {
            if numeric == 0 {
                Value::Error
            } else if matches!(op, SetOp::Min) {
                Value::Number(min)
            } else {
                Value::Number(max)
            }
        }
    }
}

fn text_slice(args: &[Expr], ctx: &Ctx<'_>, take: impl Fn(&str, usize) -> String) -> Value {
    let Some(text) = args.first().map(|a| eval(a, ctx).text()) else {
        return Value::Error;
    };
    let count = args
        .get(1)
        .and_then(|a| eval(a, ctx).number())
        .unwrap_or(1.0)
        .max(0.0) as usize;
    Value::Text(take(&text, count))
}

fn time_fraction(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if let Some((hour, minute, second)) = parse_clock(trimmed) {
        return Some((hour * 3_600 + minute * 60 + second) as f64 / 86_400.0);
    }
    let fraction = trimmed.parse::<f64>().ok()?;
    (0.0..1.0).contains(&fraction).then_some(fraction)
}

fn now_parts() -> (String, String) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let clock = seconds.rem_euclid(86_400);
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!(
            "{:02}:{:02}:{:02}",
            clock / 3_600,
            (clock / 60) % 60,
            clock % 60
        ),
    )
}

fn iso_week_number(year: i32, month: u32, day: u32) -> u32 {
    let date = days_from_civil(year, month, day);
    // ISO weekday: Monday = 1 … Sunday = 7.
    let weekday = (date + 3).rem_euclid(7) + 1;
    let thursday = date + (4 - weekday);
    let (iso_year, _, _) = civil_from_days(thursday);
    let jan1 = days_from_civil(iso_year, 1, 1);
    (1 + (thursday - jan1).div_euclid(7)) as u32
}

fn year_fraction(start: (i32, u32, u32), end: (i32, u32, u32), basis: f64) -> Option<f64> {
    if end < start {
        return None;
    }
    let actual =
        (days_from_civil(end.0, end.1, end.2) - days_from_civil(start.0, start.1, start.2)) as f64;
    match basis as u32 {
        // 0 = US 30/360, the Excel default.
        0 => Some(days_360(start, end) / 360.0),
        // 1 = actual days divided by an average year; documented as approximate.
        1 => Some(actual / 365.25),
        2 => Some(actual / 360.0),
        3 => Some(actual / 365.0),
        // 4 = European 30/360.
        4 => Some(days_360_european(start, end) / 360.0),
        _ => None,
    }
}

fn days_360_european(start: (i32, u32, u32), end: (i32, u32, u32)) -> f64 {
    let start_day = if start.2 == 31 { 30 } else { start.2 } as i64;
    let end_day = if end.2 == 31 { 30 } else { end.2 } as i64;
    ((end.0 - start.0) as i64 * 360 + (end.1 as i64 - start.1 as i64) * 30 + (end_day - start_day))
        as f64
}

fn days_360(start: (i32, u32, u32), end: (i32, u32, u32)) -> f64 {
    let (mut start_day, mut end_day) = (start.2 as i64, end.2 as i64);
    if start_day == 31 {
        start_day = 30;
    }
    if end_day == 31 && start_day >= 30 {
        end_day = 30;
    }
    ((end.0 - start.0) as i64 * 360 + (end.1 as i64 - start.1 as i64) * 30 + (end_day - start_day))
        as f64
}

fn date_difference(start: (i32, u32, u32), end: (i32, u32, u32), unit: &str) -> Option<i64> {
    if end < start {
        return None;
    }
    match unit {
        "D" => {
            Some(days_from_civil(end.0, end.1, end.2) - days_from_civil(start.0, start.1, start.2))
        }
        "Y" => {
            let mut years = end.0 - start.0;
            if (end.1, end.2) < (start.1, start.2) {
                years -= 1;
            }
            Some(years as i64)
        }
        "M" => {
            let mut months = (end.0 - start.0) as i64 * 12 + end.1 as i64 - start.1 as i64;
            if end.2 < start.2 {
                months -= 1;
            }
            Some(months)
        }
        // Months ignoring years.
        "YM" => {
            let mut months = end.1 as i64 - start.1 as i64;
            if end.2 < start.2 {
                months -= 1;
            }
            Some(months.rem_euclid(12))
        }
        // Days ignoring months and years.
        "MD" => {
            let (mut year, mut month) = (end.0, end.1);
            let mut anchor_day = start.2;
            if end.2 < start.2 {
                if month == 1 {
                    year -= 1;
                    month = 12;
                } else {
                    month -= 1;
                }
                anchor_day = anchor_day.min(days_in_month(year, month));
            }
            Some(days_from_civil(end.0, end.1, end.2) - days_from_civil(year, month, anchor_day))
        }
        // Days ignoring years.
        "YD" => {
            let mut year = end.0;
            if (end.1, end.2) < (start.1, start.2) {
                year -= 1;
            }
            let anchor_day = start.2.min(days_in_month(year, start.1));
            Some(days_from_civil(end.0, end.1, end.2) - days_from_civil(year, start.1, anchor_day))
        }
        _ => None,
    }
}

fn percentile_exc_of(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() || !(0.0..1.0).contains(&p) || p <= 0.0 {
        return None;
    }
    let mut values = values.to_vec();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = p * (values.len() + 1) as f64 - 1.0;
    if rank < 0.0 || rank > (values.len() - 1) as f64 {
        return None;
    }
    let low = rank.floor() as usize;
    let high = (low + 1).min(values.len() - 1);
    let fraction = rank - low as f64;
    Some(values[low] + fraction * (values[high] - values[low]))
}

fn percentile_of(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() || !(0.0..=1.0).contains(&p) {
        return None;
    }
    let mut values = values.to_vec();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if values.len() == 1 {
        return Some(values[0]);
    }
    let rank = p * (values.len() - 1) as f64;
    let low = rank.floor() as usize;
    let high = (low + 1).min(values.len() - 1);
    let fraction = rank - low as f64;
    Some(values[low] + fraction * (values[high] - values[low]))
}

fn text_around(args: &[Expr], ctx: &Ctx<'_>, before: bool) -> Value {
    let Some(text) = args.first().map(|a| eval(a, ctx).text()) else {
        return Value::Error;
    };
    let Some(delimiter) = args.get(1).map(|a| eval(a, ctx).text()) else {
        return Value::Error;
    };
    if delimiter.is_empty() {
        return Value::Error;
    }
    let instance = args
        .get(2)
        .and_then(|a| eval(a, ctx).number())
        .unwrap_or(1.0);
    if instance < 1.0 || !instance.is_finite() {
        return Value::Error;
    }
    let mut cursor = 0usize;
    let mut where_delimiter = None;
    for _ in 0..instance as usize {
        match text[cursor..].find(&delimiter) {
            Some(index) => {
                where_delimiter = Some(cursor + index);
                cursor += index + delimiter.len();
            }
            None => return Value::Text("#N/A".to_string()),
        }
    }
    match where_delimiter {
        Some(index) if before => Value::Text(text[..index].to_string()),
        Some(index) => Value::Text(text[index + delimiter.len()..].to_string()),
        None => Value::Text("#N/A".to_string()),
    }
}

fn time_part(text: &str, part: &str) -> Option<u32> {
    let trimmed = text.trim();
    let time_text = if let Some(index) = trimmed.find('T') {
        &trimmed[index + 1..]
    } else if let Some(index) = trimmed.find(' ') {
        &trimmed[index + 1..]
    } else {
        trimmed
    };
    if let Some((hour, minute, second)) = parse_clock(time_text) {
        return match part {
            "hour" => Some(hour),
            "minute" => Some(minute),
            _ => Some(second),
        };
    }
    let fraction = trimmed.parse::<f64>().ok()?;
    if !(0.0..1.0).contains(&fraction) {
        return None;
    }
    let total = (fraction * 86_400.0).round() as u32;
    match part {
        "hour" => Some(total / 3_600),
        "minute" => Some((total / 60) % 60),
        _ => Some(total % 60),
    }
}

fn parse_clock(text: &str) -> Option<(u32, u32, u32)> {
    let mut pieces = text.split(':');
    let hour: u32 = pieces.next()?.parse().ok()?;
    let minute: u32 = pieces.next()?.parse().ok()?;
    let second: u32 = pieces.next().unwrap_or("0").parse().ok()?;
    if pieces.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

fn holiday_days(args: &[Expr], ctx: &Ctx<'_>) -> Result<Vec<i64>, Value> {
    let mut days = Vec::new();
    for arg in args {
        match arg {
            Expr::Range { from, to } => {
                let Some(cells) = range_cells(from, to, ctx) else {
                    return Err(Value::Error);
                };
                for cell in cells {
                    let trimmed = cell.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match parse_date(trimmed) {
                        Some((year, month, day)) => days.push(days_from_civil(year, month, day)),
                        None => return Err(Value::Error),
                    }
                }
            }
            _ => {
                let text = eval(arg, ctx).text();
                match parse_date(&text) {
                    Some((year, month, day)) => days.push(days_from_civil(year, month, day)),
                    None => return Err(Value::Error),
                }
            }
        }
    }
    Ok(days)
}

/// Pull `year`, `month` or `day` out of a leading ISO date.
fn date_part(text: &str, part: &str) -> Option<u32> {
    let trimmed = text.trim();
    let mut pieces = trimmed.split(['-', '/', '.']);
    let year = pieces.next()?.parse::<i32>().ok()?;
    if !(1000..=9999).contains(&year) {
        return None;
    }
    match part {
        "year" => Some(year as u32),
        "month" => pieces
            .next()?
            .parse::<u32>()
            .ok()
            .filter(|m| (1..=12).contains(m)),
        "day" => {
            let _ = pieces.next()?;
            pieces
                .next()?
                .parse::<u32>()
                .ok()
                .filter(|d| (1..=31).contains(d))
        }
        _ => None,
    }
}

/// `roundup(1.44, 1)` → 1.5, `rounddown(1.46, 1)` → 1.4. Negative numbers round away
/// from / towards zero (Excel semantics), `digits` may be negative to round tens.
fn round_directed(direction: &str, args: &[Expr], ctx: &Ctx<'_>) -> Value {
    let Some(Some(number)) = args.first().map(|a| eval(a, ctx).number()) else {
        return Value::Error;
    };
    let digits = args
        .get(1)
        .and_then(|a| eval(a, ctx).number())
        .unwrap_or(0.0);
    if !number.is_finite() || digits.abs() > 12.0 {
        return Value::Error;
    }
    let factor = 10f64.powi(digits as i32);
    let scaled = number * factor;
    let rounded = if direction == "roundup" {
        if number >= 0.0 {
            scaled.ceil()
        } else {
            scaled.floor()
        }
    } else if number >= 0.0 {
        scaled.floor()
    } else {
        scaled.ceil()
    };
    Value::Number(rounded / factor)
}

fn cell_as_value(raw: &str) -> Value {
    match raw.trim().parse::<f64>() {
        Ok(number) if number.is_finite() => Value::Number(number),
        _ if raw.trim().is_empty() => Value::Empty,
        _ => Value::Text(raw.to_string()),
    }
}

fn cells_equal(raw: &str, lookup: &Value) -> bool {
    let cell = raw.trim();
    match lookup {
        Value::Number(number) => cell
            .parse::<f64>()
            .map(|value| value == *number)
            .unwrap_or(false),
        Value::Text(text) => {
            cell.eq_ignore_ascii_case(text.trim())
                || cell
                    .parse::<f64>()
                    .ok()
                    .zip(text.trim().parse::<f64>().ok())
                    .map(|(a, b)| a == b)
                    .unwrap_or(false)
        }
        _ => false,
    }
}

fn exact_position(lookup: &Value, cells: &[&str]) -> Option<usize> {
    cells.iter().position(|cell| cells_equal(cell, lookup))
}

fn approximate_position(lookup: &Value, cells: &[&str], mode: f64) -> Option<usize> {
    let target = match lookup {
        Value::Number(number) => *number,
        Value::Text(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    let mut candidates: Vec<(usize, f64)> = cells
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| cell.trim().parse::<f64>().ok().map(|value| (index, value)))
        .collect();
    candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    if mode > 0.0 {
        let mut best = None;
        for (index, value) in candidates {
            if value <= target {
                best = Some(index);
            }
        }
        best
    } else {
        for (index, value) in candidates {
            if value >= target {
                return Some(index);
            }
        }
        None
    }
}

fn single_separator(text: &str) -> Option<char> {
    if text.chars().count() == 1 {
        text.chars().next()
    } else {
        None
    }
}

fn fixed_body(number: f64, decimals: i32, no_commas: bool) -> String {
    let negative = number < 0.0;
    let mut text = if decimals >= 0 {
        format!("{:.*}", decimals as usize, number.abs())
    } else {
        let factor = 10f64.powi(-decimals);
        format!("{:.0}", (number.abs() / factor).round() * factor)
    };
    if !no_commas {
        let integer_end = text.find('.').unwrap_or(text.len());
        let (integer, fraction) = text.split_at(integer_end);
        text = format!("{}{}", group_digits(integer), fraction);
    }
    if negative {
        format!("-{text}")
    } else {
        text
    }
}

fn group_digits(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

/// 1-based position of `needle` in `text`; 0 when absent, like Excel's FIND/SEARCH.
fn find_in(args: &[Expr], ctx: &Ctx<'_>, case_sensitive: bool) -> Value {
    let needle = args.first().map(|a| eval(a, ctx).text());
    let haystack = args.get(1).map(|a| eval(a, ctx).text());
    let start = args
        .get(2)
        .and_then(|a| eval(a, ctx).number())
        .unwrap_or(1.0)
        .max(1.0) as usize;
    let (Some(needle), Some(haystack)) = (needle, haystack) else {
        return Value::Error;
    };
    if needle.is_empty() {
        return Value::Error;
    }
    let haystack_slice: String = haystack.chars().skip(start.saturating_sub(1)).collect();
    let (needle_cmp, haystack_cmp) = if case_sensitive {
        (needle.clone(), haystack_slice.clone())
    } else {
        (needle.to_lowercase(), haystack_slice.to_lowercase())
    };
    match haystack_cmp.find(&needle_cmp) {
        Some(byte_index) => {
            let char_index = haystack_cmp[..byte_index].chars().count();
            Value::Number((char_index + start) as f64)
        }
        None => Value::Number(0.0),
    }
}

fn subtotal_value(code: i32, cells: &[&str]) -> Value {
    let values: Vec<f64> = cells
        .iter()
        .filter_map(|cell| cell.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .collect();
    let count = values.len() as u64;
    let sum: f64 = values.iter().sum();
    let squares: f64 = values.iter().map(|value| value * value).sum();
    match code {
        1 => {
            if count == 0 {
                Value::Error
            } else {
                Value::Number(sum / count as f64)
            }
        }
        2 => Value::Number(count as f64),
        3 => Value::Number(cells.iter().filter(|cell| !cell.trim().is_empty()).count() as f64),
        4 => values
            .iter()
            .copied()
            .reduce(f64::max)
            .map_or(Value::Error, Value::Number),
        5 => values
            .iter()
            .copied()
            .reduce(f64::min)
            .map_or(Value::Error, Value::Number),
        6 => {
            if count == 0 {
                Value::Error
            } else {
                Value::Number(values.iter().product())
            }
        }
        7 => sample_variance(sum, squares, count)
            .map(|variance| Value::Number(variance.sqrt()))
            .unwrap_or(Value::Error),
        8 => population_variance(sum, squares, count)
            .map(|variance| Value::Number(variance.sqrt()))
            .unwrap_or(Value::Error),
        9 => Value::Number(sum),
        10 => sample_variance(sum, squares, count)
            .map(Value::Number)
            .unwrap_or(Value::Error),
        11 => population_variance(sum, squares, count)
            .map(Value::Number)
            .unwrap_or(Value::Error),
        _ => Value::Error,
    }
}

fn aggregate_value(code: i32, k: Option<f64>, cells: &[&str]) -> Value {
    if (1..=11).contains(&code) {
        return subtotal_value(code, cells);
    }
    let Some(k) = k else {
        return Value::Error;
    };
    if !k.is_finite() {
        return Value::Error;
    }
    let mut values: Vec<f64> = cells
        .iter()
        .filter_map(|cell| cell.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .collect();
    if values.is_empty() {
        return Value::Error;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    match code {
        14 => {
            if k < 1.0 || k.trunc() > values.len() as f64 {
                return Value::Error;
            }
            let index = values.len() - k.trunc() as usize;
            Value::Number(values[index])
        }
        15 => {
            if k < 1.0 || k.trunc() > values.len() as f64 {
                return Value::Error;
            }
            Value::Number(values[k.trunc() as usize - 1])
        }
        16 => percentile_of(&values, k).map_or(Value::Error, Value::Number),
        17 => {
            if !(0.0..=4.0).contains(&k) {
                return Value::Error;
            }
            percentile_of(&values, k / 4.0).map_or(Value::Error, Value::Number)
        }
        18 => percentile_exc_of(&values, k).map_or(Value::Error, Value::Number),
        19 => {
            if !(0.0..=4.0).contains(&k) {
                return Value::Error;
            }
            percentile_exc_of(&values, k / 4.0).map_or(Value::Error, Value::Number)
        }
        _ => Value::Error,
    }
}

/// Numeric cells in `from..=to` of the current row, or `None` when the range is not
/// resolvable while streaming (another row, unknown column letters, reversed order).
fn range_values(from: &(String, u64), to: &(String, u64), ctx: &Ctx<'_>) -> Option<Vec<f64>> {
    if from.1 != super::FIRST_DATA_ROW as u64 || to.1 != super::FIRST_DATA_ROW as u64 {
        return None;
    }
    let start_name = super::column_from_letters(&from.0, ctx.columns)?;
    let end_name = super::column_from_letters(&to.0, ctx.columns)?;
    let start = ctx.columns.iter().position(|c| c == &start_name)?;
    let end = ctx.columns.iter().position(|c| c == &end_name)?;
    if start > end {
        return None;
    }
    let mut values = Vec::new();
    for name in &ctx.columns[start..=end] {
        let raw = ctx.cells.get(name)?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = trimmed.parse::<f64>() {
            if value.is_finite() {
                values.push(value);
            }
        }
    }
    Some(values)
}

/// The cells between two columns of the current row, for `countif(B2:C2, …)`.
fn range_cells<'a>(
    from: &(String, u64),
    to: &(String, u64),
    ctx: &Ctx<'a>,
) -> Option<Vec<&'a str>> {
    if from.1 != super::FIRST_DATA_ROW as u64 || to.1 != super::FIRST_DATA_ROW as u64 {
        return None;
    }
    let start_name = super::column_from_letters(&from.0, ctx.columns)?;
    let end_name = super::column_from_letters(&to.0, ctx.columns)?;
    let start = ctx.columns.iter().position(|c| c == &start_name)?;
    let end = ctx.columns.iter().position(|c| c == &end_name)?;
    if start > end {
        return None;
    }
    let mut cells = Vec::new();
    for name in &ctx.columns[start..=end] {
        cells.push(ctx.cells.get(name)?.as_str());
    }
    Some(cells)
}

/// Sample variance from (sum, sum of squares, n); `None` when n < 2.
fn bit_integer(value: f64) -> Option<u64> {
    if value.is_finite() && value >= 0.0 && value.trunc() == value && value <= 281_474_976_710_655.0
    {
        Some(value as u64)
    } else {
        None
    }
}

fn nonnegative_integer(value: f64) -> Option<u64> {
    if value.is_finite() && value >= 0.0 && value.trunc() == value && value <= u64::MAX as f64 {
        Some(value as u64)
    } else {
        None
    }
}

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

fn sample_variance(sum: f64, squares: f64, count: u64) -> Option<f64> {
    if count < 2 {
        return None;
    }
    let n = count as f64;
    let variance = (squares - sum * sum / n) / (n - 1.0);
    // Floating-point cancellation can make an exact variance look slightly negative.
    Some(variance.max(0.0))
}

/// Parse the ISO-ish date shapes the CSV world actually contains (2026-11-23,
/// 2026/11/23, 2026.11.23) into (year, month, day).
fn parse_date(text: &str) -> Option<(i32, u32, u32)> {
    let trimmed = text.trim();
    let mut pieces = trimmed.split(['-', '/', '.']);
    let year: i32 = pieces.next()?.parse().ok()?;
    let month: u32 = pieces.next()?.parse().ok()?;
    let rest = pieces.next()?;
    if pieces.next().is_some() {
        return None;
    }
    let day: u32 = rest.get(..2)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !(1..=9999).contains(&year) {
        return None;
    }
    Some((year, month, day))
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 30,
    }
}

/// Shift a (year, month) by `delta` months, normalising the month into 1..=12.
fn add_months(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let index = year * 12 + month as i32 - 1 + delta;
    (index.div_euclid(12), (index.rem_euclid(12) + 1) as u32)
}

/// Days since 1970-01-01 (Howard Hinnant's civil-from-days inverse).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let m = month as i64;
    let d = day as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Spreadsheet weekday: Sunday = 1 … Saturday = 7.
/// Inverse of `days_from_civil` (Howard Hinnant's civil_from_days).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (if m <= 2 { y + 1 } else { y } as i32, m as u32, d as u32)
}

fn weekday(year: i32, month: u32, day: u32) -> i32 {
    let days = days_from_civil(year, month, day);
    (days + 4).rem_euclid(7) as i32 + 1
}

/// Population variance from (sum, sum of squares, n); `None` when n == 0.
fn population_variance(sum: f64, squares: f64, count: u64) -> Option<f64> {
    if count == 0 {
        return None;
    }
    let n = count as f64;
    Some(((squares - sum * sum / n) / n).max(0.0))
}
