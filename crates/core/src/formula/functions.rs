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
        "quartile" | "quartile.inc" => {
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
        // TEXTSPLIT: scalar engine exposes only the first split segment.
        "textsplit" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(text), Some(delimiter)) = (
                match eval(&args[0], ctx) {
                    Value::Text(text) => Some(text),
                    _ => None,
                },
                match eval(&args[1], ctx) {
                    Value::Text(delimiter) => Some(delimiter),
                    _ => None,
                },
            ) else {
                return Value::Error;
            };
            if delimiter.is_empty() {
                return Value::Error;
            }
            match text.split(&delimiter).next() {
                Some(first) => Value::Text(first.to_string()),
                None => Value::Error,
            }
        }
        // ARRAYTOTEXT: render a same-row range as text (concise or strict).
        "arraytotext" => {
            if args.is_empty() || args.len() > 2 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            if from.1 != super::FIRST_DATA_ROW as u64 || to.1 != super::FIRST_DATA_ROW as u64 {
                return Value::Error;
            }
            let (Some(start_name), Some(end_name)) = (
                super::column_from_letters(&from.0, ctx.columns),
                super::column_from_letters(&to.0, ctx.columns),
            ) else {
                return Value::Error;
            };
            let (Some(start), Some(end)) = (
                ctx.columns.iter().position(|c| c == &start_name),
                ctx.columns.iter().position(|c| c == &end_name),
            ) else {
                return Value::Error;
            };
            if start > end {
                return Value::Error;
            }
            let format = args
                .get(1)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !format.is_finite() || !(0.0..=1.0).contains(&format) || format.trunc() != format {
                return Value::Error;
            }
            let mut pieces = Vec::new();
            let mut non_empty = false;
            for name in &ctx.columns[start..=end] {
                let Some(raw) = ctx.cells.get(name) else {
                    return Value::Error;
                };
                let trimmed = raw.trim().to_string();
                if !trimmed.is_empty() {
                    non_empty = true;
                }
                if format == 1.0 {
                    if trimmed.parse::<f64>().is_ok() {
                        pieces.push(trimmed);
                    } else {
                        pieces.push(format!("\"{}\"", trimmed.replace('"', "\"\"")));
                    }
                } else {
                    pieces.push(trimmed);
                }
            }
            if !non_empty {
                return Value::Error;
            }
            let result = if format == 1.0 {
                format!("{{{}}}", pieces.join(","))
            } else {
                pieces.join(", ")
            };
            Value::Text(result)
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
        "percentrank" | "percentrank.inc" => {
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
        "geomean" | "harmean" | "harmmean" => {
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
        "mode" | "mode.sngl" | "mode.mult" => {
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
        // ERROR.TYPE maps each error code to Excel's 1–7 numbering. Internal
        // `Value::Error` has no further subtype, so it reports #VALUE! (3).
        "error.type" => {
            if args.len() != 1 {
                return Value::Error;
            }
            match eval(&args[0], ctx) {
                Value::Error => Value::Number(3.0),
                Value::Text(text) => match text.trim().to_ascii_uppercase().as_str() {
                    "#NULL!" => Value::Number(1.0),
                    "#DIV/0!" => Value::Number(2.0),
                    "#VALUE!" => Value::Number(3.0),
                    "#REF!" => Value::Number(4.0),
                    "#NAME?" => Value::Number(5.0),
                    "#NUM!" => Value::Number(6.0),
                    "#N/A" => Value::Number(7.0),
                    _ => Value::Text("#N/A".to_string()),
                },
                _ => Value::Text("#N/A".to_string()),
            }
        }
        // TYPE: 1 number, 2 text, 4 logical, 16 error; empty cells read as 0.
        "type" => {
            if args.len() != 1 {
                return Value::Error;
            }
            Value::Number(match eval(&args[0], ctx) {
                Value::Number(_) | Value::Empty => 1.0,
                Value::Text(_) => 2.0,
                Value::Bool(_) => 4.0,
                Value::Error => 16.0,
            })
        }
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
        // ISERR ignores the "#N/A" miss spelling; only expression errors count.
        "iserr" => match args.first() {
            Some(arg) => Value::Bool(matches!(eval(arg, ctx), Value::Error)),
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
        // ISREF tests the unevaluated expression shape: only references are TRUE.
        "isref" => match args.first() {
            Some(Expr::CellRef { .. }) | Some(Expr::Range { .. }) => Value::Bool(true),
            Some(_) => Value::Bool(false),
            None => Value::Error,
        },
        // ISLOGICAL/ISNONTEXT test the coerced value class.
        "islogical" | "isnontext" => match args.first() {
            Some(arg) => {
                let value = eval(arg, ctx);
                Value::Bool(if lower == "islogical" {
                    matches!(value, Value::Bool(_))
                } else {
                    !matches!(&value, Value::Text(text) if text != "#N/A")
                })
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
        // MROUND and the PRECISE rounding pair. MROUND keeps the sign relationship;
        // PRECISE ignores the sign of the significance.
        "mround" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(number), Some(multiple)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !number.is_finite() || !multiple.is_finite() || multiple == 0.0 {
                return Value::Error;
            }
            if number != 0.0 && (number < 0.0) != (multiple < 0.0) {
                return Value::Error;
            }
            Value::Number((number / multiple).round() * multiple)
        }
        "ceiling.precise" | "floor.precise" => {
            if args.is_empty() || args.len() > 2 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let significance = args
                .get(1)
                .and_then(|arg| eval(arg, ctx).number())
                .unwrap_or(1.0);
            if !number.is_finite() || !significance.is_finite() {
                return Value::Error;
            }
            if significance == 0.0 {
                return Value::Number(0.0);
            }
            let significance = significance.abs();
            let ratio = number / significance;
            let rounded = if lower == "ceiling.precise" {
                ratio.ceil()
            } else {
                ratio.floor()
            };
            Value::Number(rounded * significance)
        }
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
        // PMT: loan payment with optional future value and payment timing.
        "pmt" => {
            if args.len() < 3 || args.len() > 5 {
                return Value::Error;
            }
            let (Some(rate), Some(nper), Some(pv)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            let fv = args
                .get(3)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            let payment_type = args
                .get(4)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !rate.is_finite()
                || !nper.is_finite()
                || !pv.is_finite()
                || !fv.is_finite()
                || !payment_type.is_finite()
                || nper == 0.0
                || rate <= -1.0
                || ![0.0, 1.0].contains(&payment_type)
            {
                return Value::Error;
            }
            let payment = if rate == 0.0 {
                -(pv + fv) / nper
            } else {
                let growth = (1.0 + rate).powf(nper);
                -(fv + pv * growth) * rate / ((1.0 + rate * payment_type) * (growth - 1.0))
            };
            if payment.is_finite() {
                Value::Number(payment)
            } else {
                Value::Error
            }
        }
        // FV: future value from a present value, periodic payment and rate.
        "fv" => {
            if args.len() < 3 || args.len() > 5 {
                return Value::Error;
            }
            let (Some(rate), Some(nper), Some(pmt)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            let pv = args
                .get(3)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            let payment_type = args
                .get(4)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !rate.is_finite()
                || !nper.is_finite()
                || !pmt.is_finite()
                || !pv.is_finite()
                || !payment_type.is_finite()
                || rate <= -1.0
                || ![0.0, 1.0].contains(&payment_type)
            {
                return Value::Error;
            }
            let value = if rate == 0.0 {
                -(pv + pmt * nper)
            } else {
                let growth = (1.0 + rate).powf(nper);
                -(pv * growth + pmt * (1.0 + rate * payment_type) * (growth - 1.0) / rate)
            };
            if value.is_finite() {
                Value::Number(value)
            } else {
                Value::Error
            }
        }
        // PV: present value of a future value and periodic payments.
        "pv" => {
            if args.len() < 3 || args.len() > 5 {
                return Value::Error;
            }
            let (Some(rate), Some(nper), Some(pmt)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            let fv = args
                .get(3)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            let payment_type = args
                .get(4)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !rate.is_finite()
                || !nper.is_finite()
                || !pmt.is_finite()
                || !fv.is_finite()
                || !payment_type.is_finite()
                || rate <= -1.0
                || ![0.0, 1.0].contains(&payment_type)
            {
                return Value::Error;
            }
            let value = if rate == 0.0 {
                -(fv + pmt * nper)
            } else {
                let growth = (1.0 + rate).powf(nper);
                -(fv + pmt * (1.0 + rate * payment_type) * (growth - 1.0) / rate) / growth
            };
            if value.is_finite() {
                Value::Number(value)
            } else {
                Value::Error
            }
        }
        // NPER: number of periods from rate, payment and present value.
        "nper" => {
            if args.len() < 3 || args.len() > 5 {
                return Value::Error;
            }
            let (Some(rate), Some(pmt), Some(pv)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            let fv = args
                .get(3)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            let payment_type = args
                .get(4)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !rate.is_finite()
                || !pmt.is_finite()
                || !pv.is_finite()
                || !fv.is_finite()
                || !payment_type.is_finite()
                || rate <= -1.0
                || ![0.0, 1.0].contains(&payment_type)
            {
                return Value::Error;
            }
            let periods = if rate == 0.0 {
                if pmt == 0.0 {
                    return Value::Error;
                }
                -(pv + fv) / pmt
            } else {
                let payment_factor = pmt * (1.0 + rate * payment_type) / rate;
                let numerator = payment_factor - fv;
                let denominator = pv + payment_factor;
                if denominator == 0.0 || numerator / denominator <= 0.0 {
                    return Value::Error;
                }
                (numerator / denominator).ln() / (1.0 + rate).ln()
            };
            if periods.is_finite() {
                Value::Number(periods)
            } else {
                Value::Error
            }
        }
        // NPV(rate, value...): cash flows discounted from period 1 onward.
        "npv" => {
            if args.len() < 2 {
                return Value::Error;
            }
            let Some(rate) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !rate.is_finite() || rate <= -1.0 {
                return Value::Error;
            }
            let mut values = Vec::new();
            for arg in &args[1..] {
                match arg {
                    Expr::Range { from, to } => {
                        let Some(range) = range_values(from, to, ctx) else {
                            return Value::Error;
                        };
                        if range.is_empty() {
                            return Value::Error;
                        }
                        values.extend(range);
                    }
                    _ => {
                        let Some(value) = eval(arg, ctx).number() else {
                            return Value::Error;
                        };
                        values.push(value);
                    }
                }
            }
            if values.is_empty() {
                return Value::Error;
            }
            let mut growth = 1.0 + rate;
            let mut total = 0.0;
            for value in values {
                total += value / growth;
                growth *= 1.0 + rate;
            }
            if total.is_finite() {
                Value::Number(total)
            } else {
                Value::Error
            }
        }
        // RATE: solve the annuity equation by Newton iteration.
        "rate" => {
            if args.len() < 3 || args.len() > 6 {
                return Value::Error;
            }
            let (Some(nper), Some(pmt), Some(pv)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            let fv = args
                .get(3)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            let payment_type = args
                .get(4)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            let guess = args
                .get(5)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.1))
                .unwrap_or(f64::NAN);
            if !nper.is_finite()
                || nper <= 0.0
                || !pmt.is_finite()
                || !pv.is_finite()
                || !fv.is_finite()
                || !payment_type.is_finite()
                || !guess.is_finite()
                || guess <= -1.0
                || ![0.0, 1.0].contains(&payment_type)
            {
                return Value::Error;
            }
            let f = |rate: f64| {
                if rate == 0.0 {
                    fv + pv + pmt * nper
                } else {
                    let growth = (1.0 + rate).powf(nper);
                    fv + pv * growth + pmt * (1.0 + rate * payment_type) * (growth - 1.0) / rate
                }
            };
            if f(0.0).abs() < 1e-9 {
                return Value::Number(0.0);
            }
            let mut rate = if guess.abs() < 1e-6 { 0.1 } else { guess };
            for _ in 0..100 {
                let value = f(rate);
                let growth = (1.0 + rate).powf(nper);
                let growth_derivative = nper * (1.0 + rate).powf(nper - 1.0);
                let ratio = (growth - 1.0) / rate;
                let ratio_derivative = (growth_derivative * rate - (growth - 1.0)) / (rate * rate);
                let derivative = pv * growth_derivative
                    + pmt * (payment_type * ratio + (1.0 + rate * payment_type) * ratio_derivative);
                if !derivative.is_finite() || derivative.abs() < 1e-12 {
                    break;
                }
                let next = rate - value / derivative;
                if !next.is_finite() || next <= -1.0 {
                    break;
                }
                if (next - rate).abs() < 1e-10 {
                    rate = next;
                    break;
                }
                rate = next;
            }
            if rate.is_finite() && rate > -1.0 && f(rate).abs() < 1e-7 {
                Value::Number(rate)
            } else {
                Value::Error
            }
        }
        // IRR: values are period-0 cash flows; solve NPV(rate)=0.
        "irr" => {
            if args.is_empty() || args.len() > 2 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            if values.len() < 2
                || !values.iter().any(|value| *value > 0.0)
                || !values.iter().any(|value| *value < 0.0)
            {
                return Value::Error;
            }
            let guess = args
                .get(1)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.1))
                .unwrap_or(f64::NAN);
            if !guess.is_finite() || guess <= -1.0 {
                return Value::Error;
            }
            let npv = |rate: f64| {
                values
                    .iter()
                    .enumerate()
                    .map(|(period, value)| value / (1.0 + rate).powi(period as i32))
                    .sum::<f64>()
            };
            if npv(guess).abs() < 1e-10 {
                return Value::Number(guess);
            }
            let mut rate = guess;
            for _ in 0..100 {
                let value = npv(rate);
                let derivative = values
                    .iter()
                    .enumerate()
                    .map(|(period, cash)| {
                        if period == 0 {
                            0.0
                        } else {
                            -(period as f64) * cash / (1.0 + rate).powi(period as i32 + 1)
                        }
                    })
                    .sum::<f64>();
                if !derivative.is_finite() || derivative.abs() < 1e-12 {
                    break;
                }
                let next = rate - value / derivative;
                if !next.is_finite() || next <= -1.0 {
                    break;
                }
                if (next - rate).abs() < 1e-10 {
                    rate = next;
                    break;
                }
                rate = next;
            }
            if rate.is_finite() && rate > -1.0 && npv(rate).abs() < 1e-7 {
                Value::Number(rate)
            } else {
                Value::Error
            }
        }
        // EFFECT/NOMINAL: annual rate conversions with periodic compounding.
        "effect" | "nominal" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(rate), Some(periods)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !rate.is_finite()
                || !periods.is_finite()
                || rate < 0.0
                || periods < 1.0
                || periods.trunc() != periods
            {
                return Value::Error;
            }
            let periods = periods as u32;
            let value = if lower == "effect" {
                (1.0 + rate / periods as f64).powi(periods as i32) - 1.0
            } else {
                periods as f64 * ((1.0 + rate).powf(1.0 / periods as f64) - 1.0)
            };
            if value.is_finite() {
                Value::Number(value)
            } else {
                Value::Error
            }
        }
        // SLN/SYD: straight-line and sum-of-years depreciation.
        "sln" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(cost), Some(salvage), Some(life)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !cost.is_finite() || !salvage.is_finite() || !life.is_finite() || life <= 0.0 {
                return Value::Error;
            }
            Value::Number((cost - salvage) / life)
        }
        "syd" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(cost), Some(salvage), Some(life), Some(period)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !cost.is_finite()
                || !salvage.is_finite()
                || !life.is_finite()
                || !period.is_finite()
                || life <= 0.0
                || period < 1.0
                || period > life
                || life.trunc() != life
                || period.trunc() != period
            {
                return Value::Error;
            }
            let factor = life - period + 1.0;
            let sum = life * (life + 1.0) / 2.0;
            Value::Number((cost - salvage) * factor / sum)
        }
        // DB/DDB: declining-balance depreciation.
        "db" => {
            if args.len() < 4 || args.len() > 5 {
                return Value::Error;
            }
            let (Some(cost), Some(salvage), Some(life), Some(period)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            let month = args
                .get(4)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(12.0))
                .unwrap_or(f64::NAN);
            if !cost.is_finite()
                || !salvage.is_finite()
                || !life.is_finite()
                || !period.is_finite()
                || !month.is_finite()
                || cost <= 0.0
                || salvage < 0.0
                || life <= 0.0
                || period < 1.0
                || period > life
                || life.trunc() != life
                || period.trunc() != period
                || month < 1.0
                || month > 12.0
                || month.trunc() != month
            {
                return Value::Error;
            }
            let rate = ((1.0 - (salvage / cost).powf(1.0 / life)) * 1000.0).round() / 1000.0;
            let mut total = 0.0;
            let mut depreciation = 0.0;
            for step in 1..=period as i32 {
                depreciation = if step == 1 {
                    cost * rate * month / 12.0
                } else if step == life as i32 {
                    (cost - total) * rate * (12.0 - month) / 12.0
                } else {
                    (cost - total) * rate
                };
                total += depreciation;
            }
            Value::Number(depreciation)
        }
        "ddb" => {
            if args.len() < 4 || args.len() > 5 {
                return Value::Error;
            }
            let (Some(cost), Some(salvage), Some(life), Some(period)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            let factor = args
                .get(4)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(2.0))
                .unwrap_or(f64::NAN);
            if !cost.is_finite()
                || !salvage.is_finite()
                || !life.is_finite()
                || !period.is_finite()
                || !factor.is_finite()
                || cost <= 0.0
                || salvage < 0.0
                || life <= 0.0
                || period < 1.0
                || period > life
                || period.trunc() != period
                || factor <= 0.0
            {
                return Value::Error;
            }
            let rate = factor / life;
            let mut total = 0.0;
            let mut depreciation = 0.0;
            for _ in 1..=period as i32 {
                depreciation = ((cost - total) * rate).min((cost - salvage - total).max(0.0));
                total += depreciation;
            }
            Value::Number(depreciation)
        }
        // XNPV: discount cash flows by actual days from the first date (365-day basis).
        "xnvp" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let Some(rate) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !rate.is_finite() || rate <= -1.0 {
                return Value::Error;
            }
            let (
                Some(Expr::Range { from, to }),
                Some(Expr::Range {
                    from: date_from,
                    to: date_to,
                }),
            ) = (args.get(1), args.get(2))
            else {
                return Value::Error;
            };
            let Some(values) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let Some(dates) = range_cells(date_from, date_to, ctx) else {
                return Value::Error;
            };
            if values.is_empty() || values.len() != dates.len() {
                return Value::Error;
            }
            let mut cash = Vec::with_capacity(values.len());
            let mut days = Vec::with_capacity(dates.len());
            for (value, date) in values.iter().zip(dates.iter()) {
                let Some(amount) = value.trim().parse::<f64>().ok().filter(|n| n.is_finite())
                else {
                    return Value::Error;
                };
                let Some(parsed) = parse_date(date) else {
                    return Value::Error;
                };
                cash.push(amount);
                days.push(days_from_civil(parsed.0, parsed.1, parsed.2));
            }
            let base = days[0];
            let mut total = 0.0;
            for (amount, day) in cash.iter().zip(days.iter()) {
                total += amount / (1.0 + rate).powf((day - base) as f64 / 365.0);
            }
            if total.is_finite() {
                Value::Number(total)
            } else {
                Value::Error
            }
        }
        // RRI: equivalent periodic rate for a present-to-future value growth.
        "rri" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(nper), Some(pv), Some(fv)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !nper.is_finite()
                || !pv.is_finite()
                || !fv.is_finite()
                || nper <= 0.0
                || pv == 0.0
                || fv / pv <= 0.0
            {
                return Value::Error;
            }
            Value::Number((fv / pv).powf(1.0 / nper) - 1.0)
        }
        // IPMT/PPMT: split each payment into interest and principal.
        "ipmt" | "ppmt" => {
            if args.len() < 4 || args.len() > 6 {
                return Value::Error;
            }
            let (Some(rate), Some(per), Some(nper)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            let Some(pv) = eval(&args[3], ctx).number() else {
                return Value::Error;
            };
            let fv = args
                .get(4)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            let payment_type = args
                .get(5)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !rate.is_finite()
                || !per.is_finite()
                || !nper.is_finite()
                || !pv.is_finite()
                || !fv.is_finite()
                || !payment_type.is_finite()
                || rate <= -1.0
                || nper <= 0.0
                || per < 1.0
                || per > nper
                || per.trunc() != per
                || ![0.0, 1.0].contains(&payment_type)
            {
                return Value::Error;
            }
            let payment = if rate == 0.0 {
                -(pv + fv) / nper
            } else {
                let growth = (1.0 + rate).powf(nper);
                -(fv + pv * growth) * rate / ((1.0 + rate * payment_type) * (growth - 1.0))
            };
            let interest = if rate == 0.0 || (payment_type == 1.0 && per == 1.0) {
                0.0
            } else if payment_type == 1.0 {
                let months = per - 2.0;
                let balance = pv * (1.0 + rate).powf(months)
                    + payment * ((1.0 + rate).powf(months + 1.0) - 1.0) / rate;
                -balance * rate
            } else {
                let months = per - 1.0;
                let balance = pv * (1.0 + rate).powf(months)
                    + payment * ((1.0 + rate).powf(months) - 1.0) / rate;
                -balance * rate
            };
            if !interest.is_finite() || !payment.is_finite() {
                return Value::Error;
            }
            Value::Number(if lower == "ipmt" {
                interest
            } else {
                payment - interest
            })
        }
        // XIRR: date-based internal rate of return, solved by Newton iteration.
        "xirr" => {
            if args.len() < 2 || args.len() > 3 {
                return Value::Error;
            }
            let (
                Some(Expr::Range { from, to }),
                Some(Expr::Range {
                    from: date_from,
                    to: date_to,
                }),
            ) = (args.first(), args.get(1))
            else {
                return Value::Error;
            };
            let Some(values) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let Some(dates) = range_cells(date_from, date_to, ctx) else {
                return Value::Error;
            };
            if values.len() < 2 || values.len() != dates.len() {
                return Value::Error;
            }
            let mut cash = Vec::with_capacity(values.len());
            let mut days = Vec::with_capacity(dates.len());
            for (value, date) in values.iter().zip(dates.iter()) {
                let Some(amount) = value.trim().parse::<f64>().ok().filter(|n| n.is_finite())
                else {
                    return Value::Error;
                };
                let Some(parsed) = parse_date(date) else {
                    return Value::Error;
                };
                cash.push(amount);
                days.push(days_from_civil(parsed.0, parsed.1, parsed.2));
            }
            if !cash.iter().any(|value| *value > 0.0) || !cash.iter().any(|value| *value < 0.0) {
                return Value::Error;
            }
            let guess = args
                .get(2)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.1))
                .unwrap_or(f64::NAN);
            if !guess.is_finite() || guess <= -1.0 {
                return Value::Error;
            }
            let base = days[0];
            let npv = |rate: f64| {
                cash.iter()
                    .zip(days.iter())
                    .map(|(amount, day)| amount / (1.0 + rate).powf((day - base) as f64 / 365.0))
                    .sum::<f64>()
            };
            let derivative = |rate: f64| {
                cash.iter()
                    .zip(days.iter())
                    .map(|(amount, day)| {
                        let years = (day - base) as f64 / 365.0;
                        if years == 0.0 {
                            0.0
                        } else {
                            -years * amount / (1.0 + rate).powf(years + 1.0)
                        }
                    })
                    .sum::<f64>()
            };
            if npv(guess).abs() < 1e-10 {
                return Value::Number(guess);
            }
            let mut rate = guess;
            for _ in 0..100 {
                let value = npv(rate);
                let slope = derivative(rate);
                if !slope.is_finite() || slope.abs() < 1e-12 {
                    break;
                }
                let next = rate - value / slope;
                if !next.is_finite() || next <= -1.0 {
                    break;
                }
                if (next - rate).abs() < 1e-10 {
                    rate = next;
                    break;
                }
                rate = next;
            }
            if rate.is_finite() && rate > -1.0 && npv(rate).abs() < 1e-7 {
                Value::Number(rate)
            } else {
                Value::Error
            }
        }
        // FVSCHEDULE: compound a principal through a schedule of rates.
        "fvschedule" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(principal) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let Some(Expr::Range { from, to }) = args.get(1) else {
                return Value::Error;
            };
            let Some(rates) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            if rates.is_empty() || !principal.is_finite() {
                return Value::Error;
            }
            let mut value = principal;
            for rate in rates {
                let Some(rate) = rate.trim().parse::<f64>().ok().filter(|n| n.is_finite()) else {
                    return Value::Error;
                };
                if rate <= -1.0 {
                    return Value::Error;
                }
                value *= 1.0 + rate;
                if !value.is_finite() {
                    return Value::Error;
                }
            }
            Value::Number(value)
        }
        // CUMIPMT/CUMPRINC: sum interest or principal over a period range.
        "cumipmt" | "cumprinc" => {
            if args.len() != 6 {
                return Value::Error;
            }
            let (Some(rate), Some(nper), Some(pv), Some(start), Some(end), Some(payment_type)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
                eval(&args[4], ctx).number(),
                eval(&args[5], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !rate.is_finite()
                || !nper.is_finite()
                || !pv.is_finite()
                || !start.is_finite()
                || !end.is_finite()
                || !payment_type.is_finite()
                || rate <= -1.0
                || nper <= 0.0
                || start < 1.0
                || end < start
                || end > nper
                || start.trunc() != start
                || end.trunc() != end
                || ![0.0, 1.0].contains(&payment_type)
            {
                return Value::Error;
            }
            let mut interest_total = 0.0;
            let mut principal_total = 0.0;
            for period in start as i32..=end as i32 {
                let Some((interest, principal)) =
                    payment_parts(rate, period as f64, nper, pv, 0.0, payment_type)
                else {
                    return Value::Error;
                };
                interest_total += interest;
                principal_total += principal;
            }
            Value::Number(if lower == "cumipmt" {
                interest_total
            } else {
                principal_total
            })
        }
        // VDB: variable declining-balance depreciation over a period interval.
        "vdb" => {
            if args.len() < 5 || args.len() > 7 {
                return Value::Error;
            }
            let (Some(cost), Some(salvage), Some(life), Some(start), Some(end)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
                eval(&args[4], ctx).number(),
            ) else {
                return Value::Error;
            };
            let factor = args
                .get(5)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(2.0))
                .unwrap_or(f64::NAN);
            let no_switch = args
                .get(6)
                .map(|arg| truthy(&eval(arg, ctx)))
                .unwrap_or(false);
            if !cost.is_finite()
                || !salvage.is_finite()
                || !life.is_finite()
                || !start.is_finite()
                || !end.is_finite()
                || !factor.is_finite()
                || cost <= 0.0
                || salvage < 0.0
                || life <= 0.0
                || life.trunc() != life
                || start < 0.0
                || end <= start
                || end > life
                || start.trunc() != start
                || end.trunc() != end
                || factor <= 0.0
            {
                return Value::Error;
            }
            let rate = factor / life;
            let mut accumulated = 0.0;
            let mut total = 0.0;
            for period in 1..=end as i32 {
                let remaining = (cost - salvage - accumulated).max(0.0);
                let declining = ((cost - accumulated) * rate).min(remaining);
                let straight = remaining / (life - (period - 1) as f64);
                let depreciation = if !no_switch && straight > declining {
                    straight
                } else {
                    declining
                };
                if period as f64 > start {
                    total += depreciation;
                }
                accumulated += depreciation;
            }
            Value::Number(total)
        }
        // PDURATION: periods needed to grow pv to fv at a fixed rate.
        "pduration" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(rate), Some(pv), Some(fv)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !rate.is_finite()
                || !pv.is_finite()
                || !fv.is_finite()
                || rate <= -1.0
                || pv <= 0.0
                || fv <= 0.0
            {
                return Value::Error;
            }
            if rate == 0.0 {
                if pv == fv {
                    return Value::Number(0.0);
                }
                return Value::Error;
            }
            let periods = (fv / pv).ln() / (1.0 + rate).ln();
            if periods.is_finite() {
                Value::Number(periods)
            } else {
                Value::Error
            }
        }
        // SKEW/KURT: sample skewness and excess kurtosis over a same-row range.
        "skew" | "kurt" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let result = if lower == "skew" {
                sample_skewness(&values)
            } else {
                sample_kurtosis(&values)
            };
            result.map_or(Value::Error, Value::Number)
        }
        // STANDARDIZE: z-score for x with the given mean and standard deviation.
        "standardize" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(value), Some(mean), Some(deviation)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !value.is_finite() || !mean.is_finite() || !deviation.is_finite() || deviation <= 0.0
            {
                return Value::Error;
            }
            Value::Number((value - mean) / deviation)
        }
        // SLOPE/INTERCEPT: ordinary least-squares line over paired ranges.
        "slope" | "intercept" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (
                Some(Expr::Range {
                    from: y_from,
                    to: y_to,
                }),
                Some(Expr::Range {
                    from: x_from,
                    to: x_to,
                }),
            ) = (args.first(), args.get(1))
            else {
                return Value::Error;
            };
            let Some(known_y) = range_values(y_from, y_to, ctx) else {
                return Value::Error;
            };
            let Some(known_x) = range_values(x_from, x_to, ctx) else {
                return Value::Error;
            };
            let Some((slope, intercept)) = linear_regression(&known_y, &known_x) else {
                return Value::Error;
            };
            Value::Number(if lower == "slope" { slope } else { intercept })
        }
        // PEARSON/RSQ: Pearson correlation and its square over paired ranges.
        "pearson" | "rsq" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (
                Some(Expr::Range {
                    from: y_from,
                    to: y_to,
                }),
                Some(Expr::Range {
                    from: x_from,
                    to: x_to,
                }),
            ) = (args.first(), args.get(1))
            else {
                return Value::Error;
            };
            let Some(known_y) = range_values(y_from, y_to, ctx) else {
                return Value::Error;
            };
            let Some(known_x) = range_values(x_from, x_to, ctx) else {
                return Value::Error;
            };
            let Some(correlation) = pearson_correlation(&known_y, &known_x) else {
                return Value::Error;
            };
            Value::Number(if lower == "pearson" {
                correlation
            } else {
                correlation * correlation
            })
        }
        // DEVSQ: sum of squared deviations from the mean.
        "devsq" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            if values.is_empty() {
                return Value::Error;
            }
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let result = values
                .iter()
                .map(|value| (value - mean).powi(2))
                .sum::<f64>();
            Value::Number(result)
        }
        // COVAR/COVARIANCE.P/COVARIANCE.S: covariance over paired ranges.
        "covar" | "covariance.p" | "covariance.s" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (
                Some(Expr::Range {
                    from: y_from,
                    to: y_to,
                }),
                Some(Expr::Range {
                    from: x_from,
                    to: x_to,
                }),
            ) = (args.first(), args.get(1))
            else {
                return Value::Error;
            };
            let Some(known_y) = range_values(y_from, y_to, ctx) else {
                return Value::Error;
            };
            let Some(known_x) = range_values(x_from, x_to, ctx) else {
                return Value::Error;
            };
            if known_y.len() != known_x.len() || known_y.is_empty() {
                return Value::Error;
            }
            let count = known_y.len() as f64;
            let mean_y = known_y.iter().sum::<f64>() / count;
            let mean_x = known_x.iter().sum::<f64>() / count;
            let sum = known_x
                .iter()
                .zip(known_y.iter())
                .map(|(x, y)| (x - mean_x) * (y - mean_y))
                .sum::<f64>();
            let divisor = if lower == "covariance.s" {
                if known_y.len() < 2 {
                    return Value::Error;
                }
                count - 1.0
            } else {
                count
            };
            Value::Number(sum / divisor)
        }
        // FORECAST/FORECAST.LINEAR: predict y for a given x from paired ranges.
        "forecast" | "forecast.linear" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let Some(x_value) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let (
                Some(Expr::Range {
                    from: y_from,
                    to: y_to,
                }),
                Some(Expr::Range {
                    from: x_from,
                    to: x_to,
                }),
            ) = (args.get(1), args.get(2))
            else {
                return Value::Error;
            };
            let Some(known_y) = range_values(y_from, y_to, ctx) else {
                return Value::Error;
            };
            let Some(known_x) = range_values(x_from, x_to, ctx) else {
                return Value::Error;
            };
            let Some((slope, intercept)) = linear_regression(&known_y, &known_x) else {
                return Value::Error;
            };
            if !x_value.is_finite() {
                return Value::Error;
            }
            Value::Number(intercept + slope * x_value)
        }
        // AVEDEV: mean absolute deviation from the mean.
        "avedev" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            if values.is_empty() {
                return Value::Error;
            }
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            Value::Number(
                values.iter().map(|value| (value - mean).abs()).sum::<f64>() / values.len() as f64,
            )
        }
        // ISEVEN/ISODD: integer parity checks.
        "iseven" | "isodd" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() || number.trunc() != number {
                return Value::Error;
            }
            let even = (number.trunc() as i64) % 2 == 0;
            Value::Bool(if lower == "iseven" { even } else { !even })
        }
        // CEILING.MATH/FLOOR.MATH: significance-aware rounding with negative modes.
        "ceiling.math" | "floor.math" => {
            if args.is_empty() || args.len() > 3 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let significance = args
                .get(1)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(1.0))
                .unwrap_or(f64::NAN);
            let mode = args
                .get(2)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !number.is_finite() || !significance.is_finite() || !mode.is_finite() {
                return Value::Error;
            }
            if significance == 0.0 {
                return Value::Number(0.0);
            }
            let significance = significance.abs();
            let ratio = number / significance;
            let rounded = if lower == "ceiling.math" {
                if number >= 0.0 || mode == 0.0 {
                    ratio.ceil()
                } else {
                    ratio.floor()
                }
            } else if number >= 0.0 || mode == 0.0 {
                ratio.floor()
            } else {
                ratio.ceil()
            };
            Value::Number(rounded * significance)
        }
        // MIRR: modified internal rate of return with separate finance/reinvest rates.
        "mirr" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let finance_rate = eval(&args[1], ctx).number();
            let reinvest_rate = eval(&args[2], ctx).number();
            let (Some(finance_rate), Some(reinvest_rate)) = (finance_rate, reinvest_rate) else {
                return Value::Error;
            };
            if values.len() < 2
                || !finance_rate.is_finite()
                || !reinvest_rate.is_finite()
                || finance_rate <= -1.0
                || reinvest_rate <= -1.0
                || !values.iter().any(|value| *value > 0.0)
                || !values.iter().any(|value| *value < 0.0)
            {
                return Value::Error;
            }
            let n = values.len() as i32;
            let future_positive = values
                .iter()
                .enumerate()
                .filter(|(_, value)| **value > 0.0)
                .map(|(period, value)| value * (1.0 + reinvest_rate).powi(n - 1 - period as i32))
                .sum::<f64>();
            let present_negative = values
                .iter()
                .enumerate()
                .filter(|(_, value)| **value < 0.0)
                .map(|(period, value)| value / (1.0 + finance_rate).powi(period as i32))
                .sum::<f64>();
            if future_positive <= 0.0 || present_negative >= 0.0 {
                return Value::Error;
            }
            let result = (future_positive / -present_negative).powf(1.0 / (n - 1) as f64) - 1.0;
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // FISHER/FISHERINV: Fisher z transformation and its inverse.
        "fisher" | "fisherinv" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(value) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !value.is_finite() {
                return Value::Error;
            }
            if lower == "fisher" {
                if value <= -1.0 || value >= 1.0 {
                    return Value::Error;
                }
                Value::Number(0.5 * ((1.0 + value) / (1.0 - value)).ln())
            } else {
                Value::Number(value.tanh())
            }
        }
        // SERIESSUM: Σ coefficient_k * x^(n + k*m).
        "seriessum" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(x), Some(n), Some(m)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            let Some(Expr::Range { from, to }) = args.get(3) else {
                return Value::Error;
            };
            let Some(coefficients) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            if !x.is_finite() || !n.is_finite() || !m.is_finite() || coefficients.is_empty() {
                return Value::Error;
            }
            let mut total = 0.0;
            for (index, coefficient) in coefficients.iter().enumerate() {
                let Some(coefficient) = coefficient
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
                else {
                    return Value::Error;
                };
                total += coefficient * x.powf(n + index as f64 * m);
                if !total.is_finite() {
                    return Value::Error;
                }
            }
            Value::Number(total)
        }
        // STDEVA/VARA: sample statistics with text=0 and logical TRUE/FALSE=1/0.
        "stdeva" | "vara" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let mut values = Vec::new();
            for cell in cells {
                let text = cell.trim();
                if text.is_empty() {
                    continue;
                }
                let value = match text.parse::<f64>() {
                    Ok(value) if value.is_finite() => value,
                    _ if text.eq_ignore_ascii_case("true") => 1.0,
                    _ if text.eq_ignore_ascii_case("false") => 0.0,
                    _ => 0.0,
                };
                values.push(value);
            }
            if values.len() < 2 {
                return Value::Error;
            }
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let variance = values
                .iter()
                .map(|value| (value - mean).powi(2))
                .sum::<f64>()
                / (values.len() - 1) as f64;
            Value::Number(if lower == "vara" {
                variance
            } else {
                variance.sqrt()
            })
        }
        // SUMXMY2/SUMX2MY2/SUMX2PY2: paired-range aggregate products.
        "sumxmy2" | "sumx2my2" | "sumx2py2" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (
                Some(Expr::Range {
                    from: x_from,
                    to: x_to,
                }),
                Some(Expr::Range {
                    from: y_from,
                    to: y_to,
                }),
            ) = (args.first(), args.get(1))
            else {
                return Value::Error;
            };
            let Some(xs) = range_values(x_from, x_to, ctx) else {
                return Value::Error;
            };
            let Some(ys) = range_values(y_from, y_to, ctx) else {
                return Value::Error;
            };
            if xs.len() != ys.len() || xs.is_empty() {
                return Value::Error;
            }
            let total = xs
                .iter()
                .zip(ys.iter())
                .map(|(x, y)| match lower.as_str() {
                    "sumxmy2" => (x - y).powi(2),
                    "sumx2my2" => x.powi(2) - y.powi(2),
                    _ => x.powi(2) + y.powi(2),
                })
                .sum::<f64>();
            if total.is_finite() {
                Value::Number(total)
            } else {
                Value::Error
            }
        }
        // STDEVPA/VARPA: population statistics with text=0 and logical 1/0.
        "stdevpa" | "varpa" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let mut values = Vec::new();
            for cell in cells {
                let text = cell.trim();
                if text.is_empty() {
                    continue;
                }
                let value = match text.parse::<f64>() {
                    Ok(value) if value.is_finite() => value,
                    _ if text.eq_ignore_ascii_case("true") => 1.0,
                    _ if text.eq_ignore_ascii_case("false") => 0.0,
                    _ => 0.0,
                };
                values.push(value);
            }
            if values.is_empty() {
                return Value::Error;
            }
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let variance = values
                .iter()
                .map(|value| (value - mean).powi(2))
                .sum::<f64>()
                / values.len() as f64;
            Value::Number(if lower == "varpa" {
                variance
            } else {
                variance.sqrt()
            })
        }
        // AVERAGEA: arithmetic mean with text=0 and logical TRUE/FALSE=1/0.
        "averagea" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let mut total = 0.0;
            let mut count = 0.0;
            for cell in cells {
                let text = cell.trim();
                if text.is_empty() {
                    continue;
                }
                total += match text.parse::<f64>() {
                    Ok(value) if value.is_finite() => value,
                    _ if text.eq_ignore_ascii_case("true") => 1.0,
                    _ if text.eq_ignore_ascii_case("false") => 0.0,
                    _ => 0.0,
                };
                count += 1.0;
            }
            if count == 0.0 {
                return Value::Error;
            }
            Value::Number(total / count)
        }
        // MAXA/MINA: extrema with text=0 and logical TRUE/FALSE=1/0.
        "maxa" | "mina" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(cells) = range_cells(from, to, ctx) else {
                return Value::Error;
            };
            let mut result: Option<f64> = None;
            for cell in cells {
                let text = cell.trim();
                if text.is_empty() {
                    continue;
                }
                let value = match text.parse::<f64>() {
                    Ok(value) if value.is_finite() => value,
                    _ if text.eq_ignore_ascii_case("true") => 1.0,
                    _ if text.eq_ignore_ascii_case("false") => 0.0,
                    _ => 0.0,
                };
                result = Some(match result {
                    None => value,
                    Some(current) if lower == "maxa" => current.max(value),
                    Some(current) => current.min(value),
                });
            }
            result.map_or(Value::Error, Value::Number)
        }
        // DOLLARDE/DOLLARFR: convert between decimal and fractional dollar notation.
        "dollarde" | "dollarfr" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(dollar) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let Some(fraction) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if !dollar.is_finite()
                || !fraction.is_finite()
                || fraction <= 0.0
                || fraction.trunc() != fraction
            {
                return Value::Error;
            }
            let sign = if dollar < 0.0 { -1.0 } else { 1.0 };
            let absolute = dollar.abs();
            let integer = absolute.trunc();
            let fractional_part = absolute - integer;
            let digits = fraction.log10().ceil().max(0.0) as i32;
            let scale = 10f64.powi(digits);
            let result = if lower == "dollarde" {
                integer + fractional_part * scale / fraction
            } else {
                integer + fractional_part * fraction / scale
            };
            Value::Number(sign * result)
        }
        // ENCODEURL: UTF-8 percent-encoding with RFC 3986 unreserved bytes kept.
        "encodeurl" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let mut encoded = String::with_capacity(text.len());
            for byte in text.as_bytes() {
                let keep = byte.is_ascii_alphanumeric()
                    || matches!(*byte, b'-' | b'_' | b'.' | b'~')
                    || *byte == b' ';
                if keep && *byte != b' ' {
                    encoded.push(*byte as char);
                } else if *byte == b' ' {
                    encoded.push_str("%20");
                } else {
                    encoded.push_str(&format!("%{:02X}", byte));
                }
            }
            Value::Text(encoded)
        }
        // ROMAN/ARABIC: convert integers 0..3999 to and from Roman numerals.
        "roman" | "arabic" => {
            if args.is_empty() || args.len() > 2 {
                return Value::Error;
            }
            if lower == "roman" {
                let Some(number) = eval(&args[0], ctx).number() else {
                    return Value::Error;
                };
                let form = args
                    .get(1)
                    .map(|arg| eval(arg, ctx).number())
                    .unwrap_or(Some(0.0))
                    .unwrap_or(f64::NAN);
                if !number.is_finite() || number.trunc() != number || !form.is_finite() {
                    return Value::Error;
                }
                if !(0.0..=3999.0).contains(&number) || !(0.0..=4.0).contains(&form) {
                    return Value::Error;
                }
                Value::Text(to_roman(number as u32))
            } else {
                if args.len() != 1 {
                    return Value::Error;
                }
                let text = eval(&args[0], ctx).text();
                let text = text.trim();
                if text.is_empty() || text == "0" {
                    return Value::Number(0.0);
                }
                from_roman(text).map_or(Value::Error, |value| Value::Number(value as f64))
            }
        }
        // FACTDOUBLE: n!! = n*(n-2)*(n-4)...; 0!! and 1!! are 1.
        "factdouble" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() || number.trunc() != number || number < 0.0 || number > 300.0 {
                return Value::Error;
            }
            let mut current = number as u64;
            let mut result = 1.0;
            while current > 0 {
                result *= current as f64;
                if !result.is_finite() {
                    return Value::Error;
                }
                current = current.saturating_sub(2);
            }
            Value::Number(result)
        }
        // MULTINOMIAL: (sum n_i)! / product(n_i!) over non-negative integers.
        "multinomial" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            if values.is_empty()
                || values
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.0 || value.trunc() != *value)
            {
                return Value::Error;
            }
            let total = values.iter().sum::<f64>() as u64;
            if total > 170 {
                return Value::Error;
            }
            let mut result = factorial(total);
            for value in values {
                result /= factorial(value as u64);
                if !result.is_finite() {
                    return Value::Error;
                }
            }
            Value::Number(result)
        }
        // COMPLEX/IMREAL/IMAGINARY/IMABS: complex-number construction and accessors.
        "complex" | "imreal" | "imaginary" | "imabs" => {
            if lower == "complex" {
                if args.len() < 2 || args.len() > 3 {
                    return Value::Error;
                }
                let (Some(real), Some(imaginary)) =
                    (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
                else {
                    return Value::Error;
                };
                if !real.is_finite() || !imaginary.is_finite() {
                    return Value::Error;
                }
                let suffix = args
                    .get(2)
                    .map(|arg| eval(arg, ctx).text().to_ascii_lowercase())
                    .unwrap_or_else(|| "i".to_string());
                if suffix != "i" && suffix != "j" {
                    return Value::Error;
                }
                Value::Text(format_complex(real, imaginary, &suffix))
            } else {
                if args.len() != 1 {
                    return Value::Error;
                }
                let text = eval(&args[0], ctx).text();
                let Some((real, imaginary, _suffix)) = parse_complex(&text) else {
                    return Value::Error;
                };
                Value::Number(match lower.as_str() {
                    "imreal" => real,
                    "imaginary" => imaginary,
                    _ => (real * real + imaginary * imaginary).sqrt(),
                })
            }
        }
        // GAMMALN: natural logarithm of the gamma function for positive numbers.
        "gammaln" | "gammaln.precise" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() || number <= 0.0 {
                return Value::Error;
            }
            let result = ln_gamma(number);
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // STEYX: standard error of the regression estimate.
        "steyx" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (
                Some(Expr::Range {
                    from: y_from,
                    to: y_to,
                }),
                Some(Expr::Range {
                    from: x_from,
                    to: x_to,
                }),
            ) = (args.first(), args.get(1))
            else {
                return Value::Error;
            };
            let Some(known_y) = range_values(y_from, y_to, ctx) else {
                return Value::Error;
            };
            let Some(known_x) = range_values(x_from, x_to, ctx) else {
                return Value::Error;
            };
            if known_y.len() != known_x.len() || known_y.len() < 3 {
                return Value::Error;
            }
            let Some((slope, intercept)) = linear_regression(&known_y, &known_x) else {
                return Value::Error;
            };
            let residual_square_sum = known_x
                .iter()
                .zip(known_y.iter())
                .map(|(x, y)| (y - (intercept + slope * x)).powi(2))
                .sum::<f64>();
            Value::Number((residual_square_sum / (known_y.len() - 2) as f64).sqrt())
        }
        // IMSUM/IMSUB: add or subtract two complex-number texts.
        "imsum" | "imsub" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let left_text = eval(&args[0], ctx).text();
            let right_text = eval(&args[1], ctx).text();
            let (Some((left_real, left_imaginary, suffix)), Some((right_real, right_imaginary, _))) =
                (parse_complex(&left_text), parse_complex(&right_text))
            else {
                return Value::Error;
            };
            let real = if lower == "imsum" {
                left_real + right_real
            } else {
                left_real - right_real
            };
            let imaginary = if lower == "imsum" {
                left_imaginary + right_imaginary
            } else {
                left_imaginary - right_imaginary
            };
            Value::Text(format_complex(real, imaginary, &suffix))
        }
        // IMPRODUCT/IMDIV: multiply or divide two complex-number texts.
        "improduct" | "imdiv" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let left_text = eval(&args[0], ctx).text();
            let right_text = eval(&args[1], ctx).text();
            let (Some((left_real, left_imaginary, suffix)), Some((right_real, right_imaginary, _))) =
                (parse_complex(&left_text), parse_complex(&right_text))
            else {
                return Value::Error;
            };
            if lower == "improduct" {
                let real = left_real * right_real - left_imaginary * right_imaginary;
                let imaginary = left_real * right_imaginary + left_imaginary * right_real;
                Value::Text(format_complex(real, imaginary, &suffix))
            } else {
                let denominator = right_real * right_real + right_imaginary * right_imaginary;
                if denominator == 0.0 {
                    return Value::Error;
                }
                let real =
                    (left_real * right_real + left_imaginary * right_imaginary) / denominator;
                let imaginary =
                    (left_imaginary * right_real - left_real * right_imaginary) / denominator;
                Value::Text(format_complex(real, imaginary, &suffix))
            }
        }
        // IMCONJUGATE/IMARGUMENT: complex conjugate and polar argument.
        "imconjugate" | "imargument" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            if lower == "imconjugate" {
                Value::Text(format_complex(real, -imaginary, &suffix))
            } else if real == 0.0 && imaginary == 0.0 {
                Value::Error
            } else {
                Value::Number(imaginary.atan2(real))
            }
        }
        // IMEXP/IMLOG10: complex exponential and base-10 logarithm.
        "imexp" | "imlog10" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            if lower == "imexp" {
                let magnitude = real.exp();
                Value::Text(format_complex(
                    magnitude * imaginary.cos(),
                    magnitude * imaginary.sin(),
                    &suffix,
                ))
            } else {
                let magnitude = (real * real + imaginary * imaginary).sqrt();
                if magnitude == 0.0 {
                    return Value::Error;
                }
                Value::Text(format_complex(
                    magnitude.log10(),
                    imaginary.atan2(real) / std::f64::consts::LN_10,
                    &suffix,
                ))
            }
        }
        // IMSQRT: principal square root of a complex number.
        "imsqrt" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            let magnitude = (real * real + imaginary * imaginary).sqrt();
            if magnitude == 0.0 {
                return Value::Number(0.0);
            }
            let root = magnitude.sqrt();
            let angle = imaginary.atan2(real) / 2.0;
            Value::Text(format_complex(
                root * angle.cos(),
                root * angle.sin(),
                &suffix,
            ))
        }
        // IMPOWER: raise a complex number to a real power.
        "impower" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            let Some(power) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if !power.is_finite() {
                return Value::Error;
            }
            let magnitude = (real * real + imaginary * imaginary).sqrt();
            if magnitude == 0.0 {
                if power < 0.0 {
                    return Value::Error;
                }
                return Value::Number(if power == 0.0 { 1.0 } else { 0.0 });
            }
            let angle = imaginary.atan2(real) * power;
            let radius = magnitude.powf(power);
            Value::Text(format_complex(
                radius * angle.cos(),
                radius * angle.sin(),
                &suffix,
            ))
        }
        // IMLN: principal natural logarithm of a complex number.
        "imln" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            let magnitude = (real * real + imaginary * imaginary).sqrt();
            if magnitude == 0.0 {
                return Value::Error;
            }
            Value::Text(format_complex(
                magnitude.ln(),
                imaginary.atan2(real),
                &suffix,
            ))
        }
        // IMLOG2: principal base-2 logarithm of a complex number.
        "imlog2" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            let magnitude = (real * real + imaginary * imaginary).sqrt();
            if magnitude == 0.0 {
                return Value::Error;
            }
            Value::Text(format_complex(
                magnitude.log2(),
                imaginary.atan2(real) / std::f64::consts::LN_2,
                &suffix,
            ))
        }
        // DELTA: Kronecker delta; 1 when both numbers are equal, otherwise 0.
        "delta" => {
            if args.is_empty() || args.len() > 2 {
                return Value::Error;
            }
            let Some(first) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let second = args
                .get(1)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !first.is_finite() || !second.is_finite() {
                return Value::Error;
            }
            Value::Number(if first == second { 1.0 } else { 0.0 })
        }
        // GESTEP: 1 when number >= step, otherwise 0; step defaults to 0.
        "gestep" => {
            if args.is_empty() || args.len() > 2 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let step = args
                .get(1)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !number.is_finite() || !step.is_finite() {
                return Value::Error;
            }
            Value::Number(if number >= step { 1.0 } else { 0.0 })
        }
        // ERF/ERFC: error function and complementary error function.
        "erf" | "erfc" => {
            if args.is_empty() || args.len() > 2 || (lower == "erfc" && args.len() != 1) {
                return Value::Error;
            }
            let Some(first) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let second = args
                .get(1)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(0.0))
                .unwrap_or(f64::NAN);
            if !first.is_finite() || !second.is_finite() {
                return Value::Error;
            }
            if lower == "erfc" {
                Value::Number(1.0 - erf_approx(first))
            } else if args.len() == 2 {
                Value::Number(erf_approx(second) - erf_approx(first))
            } else {
                Value::Number(erf_approx(first))
            }
        }
        // ERF.PRECISE/ERFC.PRECISE: single-argument exact error functions.
        "erf.precise" | "erfc.precise" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            let result = erf_approx(number);
            Value::Number(if lower == "erfc.precise" {
                1.0 - result
            } else {
                result
            })
        }
        // GAMMA: gamma function for positive numbers.
        "gamma" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() || number <= 0.0 {
                return Value::Error;
            }
            let result = ln_gamma(number).exp();
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // COMBINA: combinations with repetition C(n+k-1, k).
        "combina" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(n), Some(k)) = (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !n.is_finite()
                || !k.is_finite()
                || n.trunc() != n
                || k.trunc() != k
                || n < 0.0
                || k < 0.0
            {
                return Value::Error;
            }
            if n == 0.0 {
                return Value::Number(if k == 0.0 { 1.0 } else { 0.0 });
            }
            let total = n + k - 1.0;
            if total > 170.0 {
                return Value::Error;
            }
            Value::Number(
                factorial(total as u64) / (factorial(k as u64) * factorial((n - 1.0) as u64)),
            )
        }
        // COT/ACOT: cotangent and inverse cotangent in radians.
        "cot" | "acot" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            if lower == "cot" {
                let tangent = number.tan();
                if tangent == 0.0 {
                    return Value::Error;
                }
                Value::Number(1.0 / tangent)
            } else {
                Value::Number(std::f64::consts::FRAC_PI_2 - number.atan())
            }
        }
        // SINH/COSH/TANH: hyperbolic sine, cosine, and tangent.
        "sinh" | "cosh" | "tanh" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            let result = match lower.as_str() {
                "sinh" => number.sinh(),
                "cosh" => number.cosh(),
                _ => number.tanh(),
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // ASINH/ACOSH/ATANH: inverse hyperbolic functions.
        "asinh" | "acosh" | "atanh" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            if (lower == "acosh" && number < 1.0)
                || (lower == "atanh" && !(-1.0..1.0).contains(&number))
            {
                return Value::Error;
            }
            let result = match lower.as_str() {
                "asinh" => number.asinh(),
                "acosh" => number.acosh(),
                _ => number.atanh(),
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // ISO.CEILING: round toward +infinity using absolute significance.
        "iso.ceiling" => {
            if args.is_empty() || args.len() > 2 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let significance = args
                .get(1)
                .map(|arg| eval(arg, ctx).number())
                .unwrap_or(Some(1.0))
                .unwrap_or(f64::NAN);
            if !number.is_finite() || !significance.is_finite() {
                return Value::Error;
            }
            if significance == 0.0 {
                return Value::Number(0.0);
            }
            let significance = significance.abs();
            Value::Number((number / significance).ceil() * significance)
        }
        // DEGREES/RADIANS: convert between radians and degrees.
        "degrees" | "radians" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            Value::Number(if lower == "degrees" {
                number.to_degrees()
            } else {
                number.to_radians()
            })
        }
        // IMCOSH/IMSINH: complex hyperbolic cosine and sine.
        "imcosh" | "imsinh" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            let (real_part, imaginary_part) = if lower == "imsinh" {
                (real.sinh() * imaginary.cos(), real.cosh() * imaginary.sin())
            } else {
                (real.cosh() * imaginary.cos(), real.sinh() * imaginary.sin())
            };
            Value::Text(format_complex(real_part, imaginary_part, &suffix))
        }
        // CSCH/SECH/COTH: reciprocal hyperbolic functions.
        "csch" | "sech" | "coth" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            let result = match lower.as_str() {
                "csch" => number.sinh(),
                "sech" => number.cosh(),
                _ => number.tanh(),
            };
            if result == 0.0 {
                return Value::Error;
            }
            let reciprocal = 1.0 / result;
            if reciprocal.is_finite() {
                Value::Number(reciprocal)
            } else {
                Value::Error
            }
        }
        // IMSIN/IMCOS/IMTAN: complex trigonometric functions.
        "imsin" | "imcos" | "imtan" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            let sin_real = real.sin() * imaginary.cosh();
            let sin_imaginary = real.cos() * imaginary.sinh();
            let cos_real = real.cos() * imaginary.cosh();
            let cos_imaginary = -real.sin() * imaginary.sinh();
            let (real_part, imaginary_part) = if lower == "imsin" {
                (sin_real, sin_imaginary)
            } else if lower == "imcos" {
                (cos_real, cos_imaginary)
            } else {
                let denominator = cos_real * cos_real + cos_imaginary * cos_imaginary;
                if denominator == 0.0 {
                    return Value::Error;
                }
                (
                    (sin_real * cos_real + sin_imaginary * cos_imaginary) / denominator,
                    (sin_imaginary * cos_real - sin_real * cos_imaginary) / denominator,
                )
            };
            Value::Text(format_complex(real_part, imaginary_part, &suffix))
        }
        // IMCSC/IMSEC: complex cosecant and secant.
        "imcsc" | "imsec" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            let sin_real = real.sin() * imaginary.cosh();
            let sin_imaginary = real.cos() * imaginary.sinh();
            let cos_real = real.cos() * imaginary.cosh();
            let cos_imaginary = -real.sin() * imaginary.sinh();
            let (numerator_real, numerator_imaginary) = if lower == "imcsc" {
                (sin_real, sin_imaginary)
            } else {
                (cos_real, cos_imaginary)
            };
            let denominator =
                numerator_real * numerator_real + numerator_imaginary * numerator_imaginary;
            if denominator == 0.0 {
                return Value::Error;
            }
            Value::Text(format_complex(
                numerator_real / denominator,
                -numerator_imaginary / denominator,
                &suffix,
            ))
        }
        // IMCOT: complex cotangent = cos(z) / sin(z).
        "imcot" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some((real, imaginary, suffix)) = parse_complex(&text) else {
                return Value::Error;
            };
            let sin_real = real.sin() * imaginary.cosh();
            let sin_imaginary = real.cos() * imaginary.sinh();
            let cos_real = real.cos() * imaginary.cosh();
            let cos_imaginary = -real.sin() * imaginary.sinh();
            let denominator = sin_real * sin_real + sin_imaginary * sin_imaginary;
            if denominator == 0.0 {
                return Value::Error;
            }
            Value::Text(format_complex(
                (cos_real * sin_real + cos_imaginary * sin_imaginary) / denominator,
                (cos_imaginary * sin_real - cos_real * sin_imaginary) / denominator,
                &suffix,
            ))
        }
        // SQRTPI: square root of number times pi.
        "sqrtpi" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() || number < 0.0 {
                return Value::Error;
            }
            let result = (number * std::f64::consts::PI).sqrt();
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // PERMUTATIONA: permutations with repetition n^k.
        "permutationa" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(n), Some(k)) = (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !n.is_finite()
                || !k.is_finite()
                || n.trunc() != n
                || k.trunc() != k
                || n < 0.0
                || k < 0.0
            {
                return Value::Error;
            }
            let result = n.powf(k);
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // SEC/CSC: reciprocal trigonometric functions.
        "sec" | "csc" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            let base = if lower == "sec" {
                number.cos()
            } else {
                number.sin()
            };
            if base == 0.0 {
                return Value::Error;
            }
            let result = 1.0 / base;
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // GAUSS: standard normal cumulative probability minus one half.
        "gauss" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            Value::Number(0.5 * erf_approx(number / std::f64::consts::SQRT_2))
        }
        // N/T: Excel value coercion helpers.
        "n" | "t" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let value = eval(&args[0], ctx);
            if lower == "t" {
                return match value {
                    Value::Text(text) => Value::Text(text),
                    Value::Error => Value::Error,
                    _ => Value::Text(String::new()),
                };
            }
            match value {
                Value::Number(number) => Value::Number(number),
                Value::Bool(flag) => Value::Number(if flag { 1.0 } else { 0.0 }),
                Value::Error => Value::Error,
                _ => Value::Number(0.0),
            }
        }
        // NORM.S.INV: inverse standard normal cumulative distribution.
        "norm.s.inv" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(probability) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let value = normal_inverse(probability);
            if value.is_finite() {
                Value::Number(value)
            } else {
                Value::Error
            }
        }
        // PHI: standard normal probability density.
        "phi" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            Value::Number((-0.5 * number * number).exp() / (2.0 * std::f64::consts::PI).sqrt())
        }
        // NORM.S.DIST: standard normal cumulative or density.
        "norm.s.dist" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let Some(cumulative) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() || !cumulative.is_finite() {
                return Value::Error;
            }
            if cumulative != 0.0 {
                Value::Number(0.5 * (1.0 + erf_approx(number / std::f64::consts::SQRT_2)))
            } else {
                Value::Number((-0.5 * number * number).exp() / (2.0 * std::f64::consts::PI).sqrt())
            }
        }
        // POISSON.DIST: Poisson probability mass or cumulative probability.
        "poisson.dist" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(events), Some(mean), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !events.is_finite()
                || !mean.is_finite()
                || !cumulative.is_finite()
                || events < 0.0
                || events.trunc() != events
                || events > 170.0
                || mean <= 0.0
            {
                return Value::Error;
            }
            let mut term = (-mean).exp();
            let mut total = term;
            for index in 1..=events as u64 {
                term *= mean / index as f64;
                total += term;
            }
            let result = if cumulative != 0.0 { total } else { term };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // EXPON.DIST: exponential probability density or cumulative.
        "expon.dist" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(number), Some(lambda), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !lambda.is_finite()
                || !cumulative.is_finite()
                || number < 0.0
                || lambda <= 0.0
            {
                return Value::Error;
            }
            let result = if cumulative != 0.0 {
                1.0 - (-lambda * number).exp()
            } else {
                lambda * (-lambda * number).exp()
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // BINOM.DIST: binomial probability mass or cumulative probability.
        "binom.dist" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(successes), Some(trials), Some(probability), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !successes.is_finite()
                || !trials.is_finite()
                || !probability.is_finite()
                || !cumulative.is_finite()
                || successes < 0.0
                || trials < 0.0
                || trials > 170.0
                || successes.trunc() != successes
                || trials.trunc() != trials
                || successes > trials
                || !(0.0..=1.0).contains(&probability)
            {
                return Value::Error;
            }
            let pmf = |k: u64| {
                factorial(trials as u64) / (factorial(k) * factorial(trials as u64 - k))
                    * probability.powi(k as i32)
                    * (1.0 - probability).powi((trials as u64 - k) as i32)
            };
            let result = if cumulative != 0.0 {
                (0..=successes as u64).map(pmf).sum()
            } else {
                pmf(successes as u64)
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // LOGNORM.DIST: lognormal probability density or cumulative.
        "lognorm.dist" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(number), Some(mean), Some(deviation), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !mean.is_finite()
                || !deviation.is_finite()
                || !cumulative.is_finite()
                || number <= 0.0
                || deviation <= 0.0
            {
                return Value::Error;
            }
            let standardized = (number.ln() - mean) / deviation;
            let result = if cumulative != 0.0 {
                0.5 * (1.0 + erf_approx(standardized / std::f64::consts::SQRT_2))
            } else {
                (-0.5 * standardized * standardized).exp()
                    / (number * deviation * (2.0 * std::f64::consts::PI).sqrt())
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // WEIBULL.DIST: Weibull probability density or cumulative.
        "weibull.dist" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(number), Some(alpha), Some(beta), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !alpha.is_finite()
                || !beta.is_finite()
                || !cumulative.is_finite()
                || number < 0.0
                || alpha <= 0.0
                || beta <= 0.0
            {
                return Value::Error;
            }
            let scaled = (number / beta).powf(alpha);
            let result = if cumulative != 0.0 {
                1.0 - (-scaled).exp()
            } else {
                alpha / beta * (number / beta).powf(alpha - 1.0) * (-scaled).exp()
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // BETA.DIST: beta probability density or cumulative.
        "beta.dist" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(number), Some(alpha), Some(beta), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !alpha.is_finite()
                || !beta.is_finite()
                || !cumulative.is_finite()
                || !(0.0..=1.0).contains(&number)
                || alpha <= 0.0
                || beta <= 0.0
            {
                return Value::Error;
            }
            let result = if cumulative != 0.0 {
                regularized_beta(number, alpha, beta)
            } else {
                number.powf(alpha - 1.0) * (1.0 - number).powf(beta - 1.0)
                    / (ln_gamma(alpha) + ln_gamma(beta) - ln_gamma(alpha + beta)).exp()
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // GAMMA.DIST: gamma probability density or cumulative.
        "gamma.dist" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(number), Some(alpha), Some(beta), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !alpha.is_finite()
                || !beta.is_finite()
                || !cumulative.is_finite()
                || number < 0.0
                || alpha <= 0.0
                || beta <= 0.0
            {
                return Value::Error;
            }
            let result = if cumulative != 0.0 {
                regularized_gamma(alpha, number / beta)
            } else {
                number.powf(alpha - 1.0) * (-number / beta).exp()
                    / (beta.powf(alpha) * ln_gamma(alpha).exp())
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // CHISQ.DIST: chi-square probability density or cumulative.
        "chisq.dist" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(number), Some(degrees), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !degrees.is_finite()
                || !cumulative.is_finite()
                || number < 0.0
                || degrees <= 0.0
            {
                return Value::Error;
            }
            let half = degrees / 2.0;
            let result = if cumulative != 0.0 {
                regularized_gamma(half, number / 2.0)
            } else {
                number.powf(half - 1.0) * (-number / 2.0).exp()
                    / (2.0f64.powf(half) * ln_gamma(half).exp())
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // NORM.DIST: normal probability density or cumulative for any mean/deviation.
        "norm.dist" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(number), Some(mean), Some(deviation), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !mean.is_finite()
                || !deviation.is_finite()
                || !cumulative.is_finite()
                || deviation <= 0.0
            {
                return Value::Error;
            }
            let standardized = (number - mean) / deviation;
            let result = if cumulative != 0.0 {
                0.5 * (1.0 + erf_approx(standardized / std::f64::consts::SQRT_2))
            } else {
                (-0.5 * standardized * standardized).exp()
                    / (deviation * (2.0 * std::f64::consts::PI).sqrt())
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // F.DIST: F probability density or cumulative.
        "f.dist" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(number), Some(first_df), Some(second_df), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !first_df.is_finite()
                || !second_df.is_finite()
                || !cumulative.is_finite()
                || number < 0.0
                || first_df <= 0.0
                || second_df <= 0.0
            {
                return Value::Error;
            }
            let result = if cumulative != 0.0 {
                regularized_beta(
                    first_df * number / (first_df * number + second_df),
                    first_df / 2.0,
                    second_df / 2.0,
                )
            } else {
                let beta = (ln_gamma(first_df / 2.0) + ln_gamma(second_df / 2.0)
                    - ln_gamma((first_df + second_df) / 2.0))
                .exp();
                (first_df / second_df).powf(first_df / 2.0) * number.powf(first_df / 2.0 - 1.0)
                    / beta
                    * (1.0 + first_df * number / second_df).powf(-(first_df + second_df) / 2.0)
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // T.DIST: Student t probability density or left-tail cumulative.
        "t.dist" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(number), Some(degrees), Some(cumulative)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !degrees.is_finite()
                || !cumulative.is_finite()
                || degrees <= 0.0
            {
                return Value::Error;
            }
            let result = if cumulative != 0.0 {
                let tail = 0.5
                    * regularized_beta(degrees / (degrees + number * number), degrees / 2.0, 0.5);
                if number < 0.0 {
                    tail
                } else {
                    1.0 - tail
                }
            } else {
                let beta = (ln_gamma((degrees + 1.0) / 2.0) - ln_gamma(degrees / 2.0)).exp();
                beta / (degrees * std::f64::consts::PI).sqrt()
                    * (1.0 + number * number / degrees).powf(-(degrees + 1.0) / 2.0)
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // LOGNORM.INV: inverse lognormal cumulative distribution.
        "lognorm.inv" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(probability), Some(mean), Some(deviation)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !mean.is_finite()
                || !deviation.is_finite()
                || !(0.0..1.0).contains(&probability)
                || deviation <= 0.0
            {
                return Value::Error;
            }
            let result = (mean + deviation * normal_inverse(probability)).exp();
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // F.DIST.RT: F right-tail probability.
        "f.dist.rt" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(number), Some(first_df), Some(second_df)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !number.is_finite()
                || !first_df.is_finite()
                || !second_df.is_finite()
                || number < 0.0
                || first_df <= 0.0
                || second_df <= 0.0
            {
                return Value::Error;
            }
            let result = regularized_beta(
                second_df / (second_df + first_df * number),
                second_df / 2.0,
                first_df / 2.0,
            );
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // T.DIST.RT: Student t right-tail probability.
        "t.dist.rt" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(number), Some(degrees)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !number.is_finite() || !degrees.is_finite() || number < 0.0 || degrees <= 0.0 {
                return Value::Error;
            }
            let result =
                0.5 * regularized_beta(degrees / (degrees + number * number), degrees / 2.0, 0.5);
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // BINOM.INV: smallest k whose binomial cumulative probability reaches alpha.
        "binom.inv" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(trials), Some(probability), Some(alpha)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !trials.is_finite()
                || !probability.is_finite()
                || !alpha.is_finite()
                || trials < 0.0
                || trials > 170.0
                || trials.trunc() != trials
                || !(0.0..=1.0).contains(&probability)
                || !(0.0..=1.0).contains(&alpha)
            {
                return Value::Error;
            }
            let trials = trials as u64;
            let pmf = |k: u64| {
                factorial(trials) / (factorial(k) * factorial(trials - k))
                    * probability.powi(k as i32)
                    * (1.0 - probability).powi((trials - k) as i32)
            };
            let mut cumulative = 0.0;
            for k in 0..=trials {
                cumulative += pmf(k);
                if cumulative >= alpha {
                    return Value::Number(k as f64);
                }
            }
            Value::Number(trials as f64)
        }
        // T.DIST.2T: two-tailed Student t probability.
        "t.dist.2t" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(number), Some(degrees)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !number.is_finite() || !degrees.is_finite() || number < 0.0 || degrees <= 0.0 {
                return Value::Error;
            }
            let result =
                regularized_beta(degrees / (degrees + number * number), degrees / 2.0, 0.5);
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // T.INV: left-tail inverse of the Student t distribution.
        "t.inv" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(probability), Some(degrees)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !degrees.is_finite()
                || !(0.0..1.0).contains(&probability)
                || degrees <= 0.0
            {
                return Value::Error;
            }
            let cumulative = |value: f64| {
                let tail =
                    0.5 * regularized_beta(degrees / (degrees + value * value), degrees / 2.0, 0.5);
                if value < 0.0 {
                    tail
                } else {
                    1.0 - tail
                }
            };
            let mut low = -1.0;
            while cumulative(low) > probability {
                low *= 2.0;
                if low < -1.0e9 {
                    return Value::Error;
                }
            }
            let mut high = 1.0;
            while cumulative(high) < probability {
                high *= 2.0;
                if high > 1.0e9 {
                    return Value::Error;
                }
            }
            for _ in 0..200 {
                let middle = (low + high) / 2.0;
                if cumulative(middle) < probability {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            Value::Number((low + high) / 2.0)
        }
        // BINOM.DIST.RANGE: binomial probability over [s, t].
        "binom.dist.range" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(trials), Some(probability), Some(low), Some(high)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !trials.is_finite()
                || !probability.is_finite()
                || !low.is_finite()
                || !high.is_finite()
                || trials < 0.0
                || trials > 170.0
                || trials.trunc() != trials
                || !(0.0..=1.0).contains(&probability)
                || low < 0.0
                || high < 0.0
                || low.trunc() != low
                || high.trunc() != high
                || low > high
                || high > trials
            {
                return Value::Error;
            }
            let trials = trials as u64;
            let pmf = |k: u64| {
                factorial(trials) / (factorial(k) * factorial(trials - k))
                    * probability.powi(k as i32)
                    * (1.0 - probability).powi((trials - k) as i32)
            };
            let result: f64 = (low as u64..=high as u64).map(pmf).sum();
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // T.INV.2T: positive t quantile for a two-tailed probability.
        "t.inv.2t" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(probability), Some(degrees)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !degrees.is_finite()
                || !(0.0..1.0).contains(&probability)
                || degrees <= 0.0
            {
                return Value::Error;
            }
            let target = 1.0 - probability / 2.0;
            let cumulative = |value: f64| {
                let tail =
                    0.5 * regularized_beta(degrees / (degrees + value * value), degrees / 2.0, 0.5);
                if value < 0.0 {
                    tail
                } else {
                    1.0 - tail
                }
            };
            let mut low = 0.0;
            let mut high = 1.0;
            while cumulative(high) < target {
                high *= 2.0;
                if high > 1.0e9 {
                    return Value::Error;
                }
            }
            for _ in 0..200 {
                let middle = (low + high) / 2.0;
                if cumulative(middle) < target {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            Value::Number((low + high) / 2.0)
        }
        // NORM.INV: inverse normal cumulative distribution for any mean/deviation.
        "norm.inv" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(probability), Some(mean), Some(deviation)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !mean.is_finite()
                || !deviation.is_finite()
                || !(0.0..1.0).contains(&probability)
                || deviation <= 0.0
            {
                return Value::Error;
            }
            let result = mean + deviation * normal_inverse(probability);
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // CHISQ.DIST.RT: chi-square right-tail probability.
        "chisq.dist.rt" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(number), Some(degrees)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !number.is_finite() || !degrees.is_finite() || number < 0.0 || degrees <= 0.0 {
                return Value::Error;
            }
            let result = 1.0 - regularized_gamma(degrees / 2.0, number / 2.0);
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // ASECH/ACSCH/ACOTH: inverse reciprocal hyperbolic functions.
        "asech" | "acsch" | "acoth" => {
            if args.len() != 1 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            if !number.is_finite() {
                return Value::Error;
            }
            let result = match lower.as_str() {
                "asech" => {
                    if number <= 0.0 || number > 1.0 {
                        return Value::Error;
                    }
                    ((1.0 + (1.0 - number * number).sqrt()) / number).ln()
                }
                "acsch" => {
                    if number == 0.0 {
                        return Value::Error;
                    }
                    (1.0 / number).asinh()
                }
                _ => {
                    if number.abs() <= 1.0 {
                        return Value::Error;
                    }
                    0.5 * ((number + 1.0) / (number - 1.0)).ln()
                }
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // F.INV.RT: inverse right-tail F distribution.
        "f.inv.rt" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(probability), Some(first_df), Some(second_df)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !first_df.is_finite()
                || !second_df.is_finite()
                || !(0.0..1.0).contains(&probability)
                || first_df <= 0.0
                || second_df <= 0.0
            {
                return Value::Error;
            }
            let right_tail = |value: f64| {
                regularized_beta(
                    second_df / (second_df + first_df * value),
                    second_df / 2.0,
                    first_df / 2.0,
                )
            };
            let mut low = 0.0;
            let mut high = 1.0;
            while right_tail(high) > probability {
                high *= 2.0;
                if high > 1.0e9 {
                    return Value::Error;
                }
            }
            for _ in 0..200 {
                let middle = (low + high) / 2.0;
                if right_tail(middle) > probability {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            Value::Number((low + high) / 2.0)
        }
        // F.INV: inverse left-tail F distribution.
        "f.inv" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(probability), Some(first_df), Some(second_df)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !first_df.is_finite()
                || !second_df.is_finite()
                || probability <= 0.0
                || probability >= 1.0
                || first_df <= 0.0
                || second_df <= 0.0
            {
                return Value::Error;
            }
            let target = 1.0 - probability;
            let right_tail = |value: f64| {
                regularized_beta(
                    second_df / (second_df + first_df * value),
                    second_df / 2.0,
                    first_df / 2.0,
                )
            };
            let mut low = 0.0;
            let mut high = 1.0;
            while right_tail(high) > target {
                high *= 2.0;
                if high > 1.0e9 {
                    return Value::Error;
                }
            }
            for _ in 0..200 {
                let middle = (low + high) / 2.0;
                if right_tail(middle) > target {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            Value::Number((low + high) / 2.0)
        }
        // CHISQ.INV: inverse left-tail chi-square distribution.
        "chisq.inv" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(probability), Some(degrees)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !degrees.is_finite()
                || probability <= 0.0
                || probability >= 1.0
                || degrees <= 0.0
            {
                return Value::Error;
            }
            let cumulative = |value: f64| regularized_gamma(degrees / 2.0, value / 2.0);
            let mut low = 0.0;
            let mut high = 1.0;
            while cumulative(high) < probability {
                high *= 2.0;
                if high > 1.0e9 {
                    return Value::Error;
                }
            }
            for _ in 0..200 {
                let middle = (low + high) / 2.0;
                if cumulative(middle) < probability {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            Value::Number((low + high) / 2.0)
        }
        // CHISQ.INV.RT: inverse right-tail chi-square distribution.
        "chisq.inv.rt" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(probability), Some(degrees)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !degrees.is_finite()
                || probability <= 0.0
                || probability >= 1.0
                || degrees <= 0.0
            {
                return Value::Error;
            }
            let right_tail = |value: f64| 1.0 - regularized_gamma(degrees / 2.0, value / 2.0);
            let mut low = 0.0;
            let mut high = 1.0;
            while right_tail(high) > probability {
                high *= 2.0;
                if high > 1.0e9 {
                    return Value::Error;
                }
            }
            for _ in 0..200 {
                let middle = (low + high) / 2.0;
                if right_tail(middle) > probability {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            Value::Number((low + high) / 2.0)
        }
        // BETA.INV: inverse regularized beta distribution.
        "beta.inv" => {
            if args.len() < 4 || args.len() > 5 {
                return Value::Error;
            }
            let (Some(probability), Some(alpha), Some(beta)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            let lower = if args.len() == 5 {
                eval(&args[3], ctx).number()
            } else {
                Some(0.0)
            };
            let upper = if args.len() == 5 {
                eval(&args[4], ctx).number()
            } else {
                Some(1.0)
            };
            let (Some(lower), Some(upper)) = (lower, upper) else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !alpha.is_finite()
                || !beta.is_finite()
                || !lower.is_finite()
                || !upper.is_finite()
                || probability <= 0.0
                || probability >= 1.0
                || alpha <= 0.0
                || beta <= 0.0
                || lower >= upper
            {
                return Value::Error;
            }
            let span = upper - lower;
            let cumulative = |value: f64| regularized_beta((value - lower) / span, alpha, beta);
            let mut low = lower;
            let mut high = upper;
            for _ in 0..200 {
                let middle = (low + high) / 2.0;
                if cumulative(middle) < probability {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            Value::Number((low + high) / 2.0)
        }
        // GAMMA.INV: inverse gamma cumulative distribution.
        "gamma.inv" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(probability), Some(alpha), Some(beta)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !probability.is_finite()
                || !alpha.is_finite()
                || !beta.is_finite()
                || probability <= 0.0
                || probability >= 1.0
                || alpha <= 0.0
                || beta <= 0.0
            {
                return Value::Error;
            }
            let cumulative = |value: f64| regularized_gamma(alpha, value / beta);
            let mut low = 0.0;
            let mut high = 1.0;
            while cumulative(high) < probability {
                high *= 2.0;
                if high > 1.0e9 {
                    return Value::Error;
                }
            }
            for _ in 0..200 {
                let middle = (low + high) / 2.0;
                if cumulative(middle) < probability {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            Value::Number((low + high) / 2.0)
        }
        // Z.TEST: one-tailed normal test for a sample mean.
        "z.test" => {
            if args.len() < 2 || args.len() > 3 {
                return Value::Error;
            }
            let Some(Expr::Range { from, to }) = args.first() else {
                return Value::Error;
            };
            let Some(values) = range_values(from, to, ctx) else {
                return Value::Error;
            };
            let Some(target) = eval(&args[1], ctx).number() else {
                return Value::Error;
            };
            if values.is_empty() || !target.is_finite() {
                return Value::Error;
            }
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let deviation = if let Some(arg) = args.get(2) {
                let Some(sigma) = eval(arg, ctx).number() else {
                    return Value::Error;
                };
                if !sigma.is_finite() || sigma <= 0.0 {
                    return Value::Error;
                }
                sigma
            } else {
                if values.len() < 2 {
                    return Value::Error;
                }
                let variance = values
                    .iter()
                    .map(|value| (value - mean).powi(2))
                    .sum::<f64>()
                    / (values.len() - 1) as f64;
                variance.sqrt()
            };
            if deviation <= 0.0 {
                return Value::Error;
            }
            let statistic = (mean - target) / (deviation / (values.len() as f64).sqrt());
            let result = 0.5 * (1.0 - erf_approx(statistic / std::f64::consts::SQRT_2));
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // F.TEST: two-tailed F test for equal variances.
        "f.test" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (
                Expr::Range {
                    from: first_from,
                    to: first_to,
                },
                Expr::Range {
                    from: second_from,
                    to: second_to,
                },
            ) = (&args[0], &args[1])
            else {
                return Value::Error;
            };
            let (Some(first), Some(second)) = (
                range_values(first_from, first_to, ctx),
                range_values(second_from, second_to, ctx),
            ) else {
                return Value::Error;
            };
            if first.len() < 2 || second.len() < 2 {
                return Value::Error;
            }
            let variance = |values: &[f64]| {
                let mean = values.iter().sum::<f64>() / values.len() as f64;
                values
                    .iter()
                    .map(|value| (value - mean).powi(2))
                    .sum::<f64>()
                    / (values.len() - 1) as f64
            };
            let first_variance = variance(&first);
            let second_variance = variance(&second);
            if first_variance <= 0.0 || second_variance <= 0.0 {
                return Value::Error;
            }
            let ratio = first_variance / second_variance;
            let first_df = (first.len() - 1) as f64;
            let second_df = (second.len() - 1) as f64;
            let right = regularized_beta(
                second_df / (second_df + first_df * ratio),
                second_df / 2.0,
                first_df / 2.0,
            );
            let result = (2.0 * right.min(1.0 - right)).min(1.0);
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // CONFIDENCE.NORM: normal confidence interval half-width.
        "confidence.norm" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(alpha), Some(deviation), Some(size)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !alpha.is_finite()
                || !deviation.is_finite()
                || !size.is_finite()
                || alpha <= 0.0
                || alpha >= 1.0
                || deviation <= 0.0
                || size < 1.0
                || size.trunc() != size
            {
                return Value::Error;
            }
            let result = normal_inverse(1.0 - alpha / 2.0) * deviation / size.sqrt();
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // CONFIDENCE.T: t-based confidence interval half-width.
        "confidence.t" => {
            if args.len() != 3 {
                return Value::Error;
            }
            let (Some(alpha), Some(deviation), Some(size)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !alpha.is_finite()
                || !deviation.is_finite()
                || !size.is_finite()
                || alpha <= 0.0
                || alpha >= 1.0
                || deviation <= 0.0
                || size < 2.0
                || size.trunc() != size
            {
                return Value::Error;
            }
            let degrees = size - 1.0;
            let target = 1.0 - alpha / 2.0;
            let cumulative = |value: f64| {
                let tail =
                    0.5 * regularized_beta(degrees / (degrees + value * value), degrees / 2.0, 0.5);
                if value < 0.0 {
                    tail
                } else {
                    1.0 - tail
                }
            };
            let mut low = 0.0;
            let mut high = 1.0;
            while cumulative(high) < target {
                high *= 2.0;
                if high > 1.0e9 {
                    return Value::Error;
                }
            }
            for _ in 0..200 {
                let middle = (low + high) / 2.0;
                if cumulative(middle) < target {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            let quantile = (low + high) / 2.0;
            let result = quantile * deviation / size.sqrt();
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // T.TEST: Student t test p-value for paired or two-sample data.
        "t.test" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (
                Expr::Range {
                    from: first_from,
                    to: first_to,
                },
                Expr::Range {
                    from: second_from,
                    to: second_to,
                },
            ) = (&args[0], &args[1])
            else {
                return Value::Error;
            };
            let (Some(first), Some(second)) = (
                range_values(first_from, first_to, ctx),
                range_values(second_from, second_to, ctx),
            ) else {
                return Value::Error;
            };
            let (Some(tails), Some(kind)) =
                (eval(&args[2], ctx).number(), eval(&args[3], ctx).number())
            else {
                return Value::Error;
            };
            if !tails.is_finite()
                || !kind.is_finite()
                || tails.trunc() != tails
                || kind.trunc() != kind
            {
                return Value::Error;
            }
            let sample_variance = |values: &[f64]| {
                let mean = values.iter().sum::<f64>() / values.len() as f64;
                values
                    .iter()
                    .map(|value| (value - mean).powi(2))
                    .sum::<f64>()
                    / (values.len() - 1) as f64
            };
            let (statistic, degrees) = match kind as i64 {
                1 => {
                    if first.len() != second.len() || first.len() < 2 {
                        return Value::Error;
                    }
                    let differences: Vec<f64> = first
                        .iter()
                        .zip(second.iter())
                        .map(|(a, b)| a - b)
                        .collect();
                    let mean = differences.iter().sum::<f64>() / differences.len() as f64;
                    let variance = sample_variance(&differences);
                    if variance <= 0.0 {
                        return Value::Error;
                    }
                    (
                        mean / (variance.sqrt() / (differences.len() as f64).sqrt()),
                        (differences.len() - 1) as f64,
                    )
                }
                2 | 3 => {
                    if first.len() < 2 || second.len() < 2 {
                        return Value::Error;
                    }
                    let first_mean = first.iter().sum::<f64>() / first.len() as f64;
                    let second_mean = second.iter().sum::<f64>() / second.len() as f64;
                    let first_variance = sample_variance(&first);
                    let second_variance = sample_variance(&second);
                    if kind as i64 == 2 {
                        let pooled = ((first.len() - 1) as f64 * first_variance
                            + (second.len() - 1) as f64 * second_variance)
                            / (first.len() + second.len() - 2) as f64;
                        if pooled <= 0.0 {
                            return Value::Error;
                        }
                        (
                            (first_mean - second_mean)
                                / (pooled.sqrt()
                                    * (1.0 / first.len() as f64 + 1.0 / second.len() as f64)
                                        .sqrt()),
                            (first.len() + second.len() - 2) as f64,
                        )
                    } else {
                        let first_term = first_variance / first.len() as f64;
                        let second_term = second_variance / second.len() as f64;
                        let denominator = (first_term + second_term).sqrt();
                        if denominator <= 0.0 {
                            return Value::Error;
                        }
                        let degrees = (first_term + second_term).powi(2)
                            / ((first_term.powi(2) / (first.len() - 1) as f64)
                                + (second_term.powi(2) / (second.len() - 1) as f64));
                        ((first_mean - second_mean) / denominator, degrees)
                    }
                }
                _ => return Value::Error,
            };
            if !statistic.is_finite() || !degrees.is_finite() || degrees <= 0.0 {
                return Value::Error;
            }
            let two_tailed = regularized_beta(
                degrees / (degrees + statistic * statistic),
                degrees / 2.0,
                0.5,
            );
            let result = match tails as i64 {
                1 => two_tailed / 2.0,
                2 => two_tailed.min(1.0),
                _ => return Value::Error,
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // BESSELJ: Bessel function of the first kind, integer order.
        "besselj" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(number), Some(order)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !number.is_finite()
                || !order.is_finite()
                || order < 0.0
                || order > 100.0
                || order.trunc() != order
            {
                return Value::Error;
            }
            let order = order as u64;
            if number == 0.0 {
                return Value::Number(if order == 0 { 1.0 } else { 0.0 });
            }
            let half = number / 2.0;
            let mut term = half.powi(order as i32) / factorial(order);
            if !term.is_finite() {
                return Value::Error;
            }
            let mut sum = term;
            for k in 1..500u64 {
                term *= -(half * half) / ((k as f64) * ((k + order) as f64));
                let previous = sum;
                sum += term;
                if sum == previous || term.abs() < 1e-15 * sum.abs().max(1.0) {
                    break;
                }
            }
            if sum.is_finite() {
                Value::Number(sum)
            } else {
                Value::Error
            }
        }
        // BESSELI: modified Bessel function of the first kind, integer order.
        "besseli" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(number), Some(order)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !number.is_finite()
                || !order.is_finite()
                || order < 0.0
                || order > 100.0
                || order.trunc() != order
            {
                return Value::Error;
            }
            let order = order as u64;
            if number == 0.0 {
                return Value::Number(if order == 0 { 1.0 } else { 0.0 });
            }
            let half = number / 2.0;
            let mut term = half.powi(order as i32) / factorial(order);
            if !term.is_finite() {
                return Value::Error;
            }
            let mut sum = term;
            for k in 1..500u64 {
                term *= (half * half) / ((k as f64) * ((k + order) as f64));
                let previous = sum;
                sum += term;
                if sum == previous || term.abs() < 1e-15 * sum.abs().max(1.0) {
                    break;
                }
            }
            if sum.is_finite() {
                Value::Number(sum)
            } else {
                Value::Error
            }
        }
        // BESSELY: Bessel function of the second kind, integer order.
        "bessely" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(number), Some(order)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !number.is_finite()
                || !order.is_finite()
                || number <= 0.0
                || order < 0.0
                || order > 20.0
                || order.trunc() != order
            {
                return Value::Error;
            }
            let order = order as u64;
            let n = order as f64;
            let pi = std::f64::consts::PI;
            let steps = 4000u32;
            let h = pi / steps as f64;
            let mut first = 0.0;
            for step in 0..=steps {
                let t = step as f64 * h;
                let weight = if step == 0 || step == steps {
                    1.0
                } else if step % 2 == 0 {
                    2.0
                } else {
                    4.0
                };
                first += weight * (number * t.sin() - n * t).sin();
            }
            first *= h / 3.0;
            let upper = 14.0;
            let h = upper / steps as f64;
            let mut second = 0.0;
            let sign = if order.is_multiple_of(2) { 1.0 } else { -1.0 };
            for step in 0..=steps {
                let t = step as f64 * h;
                let weight = if step == 0 || step == steps {
                    1.0
                } else if step % 2 == 0 {
                    2.0
                } else {
                    4.0
                };
                let sinh = (t.exp() - (-t).exp()) / 2.0;
                let kernel = (n * t).exp() + sign * (-n * t).exp();
                second += weight * kernel * (-number * sinh).exp();
            }
            second *= h / 3.0;
            let result = (first - second) / pi;
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // HYPGEOM.DIST: hypergeometric probability mass or cumulative.
        "hypgeom.dist" => {
            if args.len() != 5 {
                return Value::Error;
            }
            let (
                Some(sample_successes),
                Some(sample_size),
                Some(population_successes),
                Some(population_size),
                Some(cumulative),
            ) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
                eval(&args[4], ctx).number(),
            )
            else {
                return Value::Error;
            };
            if !sample_successes.is_finite()
                || !sample_size.is_finite()
                || !population_successes.is_finite()
                || !population_size.is_finite()
                || !cumulative.is_finite()
                || sample_successes < 0.0
                || sample_size < 0.0
                || population_successes < 0.0
                || population_size <= 0.0
                || sample_successes.trunc() != sample_successes
                || sample_size.trunc() != sample_size
                || population_successes.trunc() != population_successes
                || population_size.trunc() != population_size
                || sample_size > population_size
                || population_successes > population_size
                || sample_successes > sample_size
                || sample_successes > population_successes
                || sample_size - sample_successes > population_size - population_successes
            {
                return Value::Error;
            }
            let successes = population_successes as u64;
            let failures = (population_size - population_successes) as u64;
            let draws = sample_size as u64;
            let observed = sample_successes as u64;
            let total = population_size as u64;
            let ln_combination = |a: u64, b: u64| {
                if b > a {
                    return f64::NEG_INFINITY;
                }
                ln_gamma(a as f64 + 1.0) - ln_gamma(b as f64 + 1.0) - ln_gamma((a - b) as f64 + 1.0)
            };
            let log_denominator = ln_combination(total, draws);
            let mass = |value: u64| {
                (ln_combination(successes, value) + ln_combination(failures, draws - value)
                    - log_denominator)
                    .exp()
            };
            let result = if cumulative != 0.0 {
                let low = draws.saturating_sub(failures);
                (low..=observed).map(&mass).sum::<f64>()
            } else {
                mass(observed)
            };
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // BESSELK: modified Bessel function of the second kind, integer order.
        "besselk" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (Some(number), Some(order)) =
                (eval(&args[0], ctx).number(), eval(&args[1], ctx).number())
            else {
                return Value::Error;
            };
            if !number.is_finite()
                || !order.is_finite()
                || number <= 0.0
                || order < 0.0
                || order > 20.0
                || order.trunc() != order
            {
                return Value::Error;
            }
            let order = order as u64;
            let steps = 2000u32;
            let upper = 10.0;
            let h = upper / steps as f64;
            let mut k0 = 0.0;
            let mut k1 = 0.0;
            for step in 0..=steps {
                let t = step as f64 * h;
                let weight = if step == 0 || step == steps {
                    1.0
                } else if step % 2 == 0 {
                    2.0
                } else {
                    4.0
                };
                let cosh = ((t).exp() + (-t).exp()) / 2.0;
                let kernel = (-number * cosh).exp();
                k0 += weight * kernel;
                k1 += weight * kernel * cosh;
            }
            k0 *= h / 3.0;
            k1 *= h / 3.0;
            if !k0.is_finite() || !k1.is_finite() {
                return Value::Error;
            }
            let mut previous = k0;
            let mut current = k1;
            for index in 1..order {
                let next = previous + 2.0 * index as f64 * current / number;
                if !next.is_finite() {
                    return Value::Error;
                }
                previous = current;
                current = next;
            }
            Value::Number(if order == 0 { k0 } else { current })
        }
        // HYPGEOM.INV: smallest hypergeometric quantile reaching alpha.
        "hypgeom.inv" => {
            if args.len() != 4 {
                return Value::Error;
            }
            let (Some(sample_size), Some(population_successes), Some(population_size), Some(alpha)) = (
                eval(&args[0], ctx).number(),
                eval(&args[1], ctx).number(),
                eval(&args[2], ctx).number(),
                eval(&args[3], ctx).number(),
            ) else {
                return Value::Error;
            };
            if !sample_size.is_finite()
                || !population_successes.is_finite()
                || !population_size.is_finite()
                || !alpha.is_finite()
                || sample_size < 0.0
                || population_successes < 0.0
                || population_size <= 0.0
                || sample_size.trunc() != sample_size
                || population_successes.trunc() != population_successes
                || population_size.trunc() != population_size
                || sample_size > population_size
                || population_successes > population_size
                || !(0.0..=1.0).contains(&alpha)
            {
                return Value::Error;
            }
            let draws = sample_size as u64;
            let successes = population_successes as u64;
            let failures = (population_size - population_successes) as u64;
            let total = population_size as u64;
            let ln_combination = |a: u64, b: u64| {
                if b > a {
                    return f64::NEG_INFINITY;
                }
                ln_gamma(a as f64 + 1.0) - ln_gamma(b as f64 + 1.0) - ln_gamma((a - b) as f64 + 1.0)
            };
            let denominator = ln_combination(total, draws);
            let mass = |value: u64| {
                (ln_combination(successes, value) + ln_combination(failures, draws - value)
                    - denominator)
                    .exp()
            };
            let low = draws.saturating_sub(failures);
            let high = draws.min(successes);
            let mut cumulative = 0.0;
            for value in low..=high {
                cumulative += mass(value);
                if cumulative >= alpha {
                    return Value::Number(value as f64);
                }
            }
            Value::Number(high as f64)
        }
        // CHISQ.TEST: one-row goodness-of-fit chi-square test.
        "chisq.test" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let (
                Expr::Range {
                    from: observed_from,
                    to: observed_to,
                },
                Expr::Range {
                    from: expected_from,
                    to: expected_to,
                },
            ) = (&args[0], &args[1])
            else {
                return Value::Error;
            };
            let (Some(observed), Some(expected)) = (
                range_values(observed_from, observed_to, ctx),
                range_values(expected_from, expected_to, ctx),
            ) else {
                return Value::Error;
            };
            if observed.len() != expected.len() || observed.len() < 2 {
                return Value::Error;
            }
            let mut statistic = 0.0;
            for (value, target) in observed.iter().zip(expected.iter()) {
                if *target <= 0.0 {
                    return Value::Error;
                }
                statistic += (value - target).powi(2) / target;
            }
            if !statistic.is_finite() || statistic < 0.0 {
                return Value::Error;
            }
            let degrees = (observed.len() - 1) as f64;
            let result = 1.0 - regularized_gamma(degrees / 2.0, statistic / 2.0);
            if result.is_finite() {
                Value::Number(result)
            } else {
                Value::Error
            }
        }
        // DECIMAL/BASE convert between a radix-2..36 string and a number.
        "decimal" => {
            if args.len() != 2 {
                return Value::Error;
            }
            let text = eval(&args[0], ctx).text();
            let Some(radix) = eval(&args[1], ctx).number().and_then(radix_of) else {
                return Value::Error;
            };
            let mut text = text.trim();
            let negative = text.starts_with('-');
            if negative || text.starts_with('+') {
                text = &text[1..];
            }
            if text.is_empty() {
                return Value::Number(0.0);
            }
            let mut value = 0.0f64;
            for ch in text.chars() {
                let Some(digit) = ch.to_digit(radix) else {
                    return Value::Error;
                };
                if digit >= radix {
                    return Value::Error;
                }
                value = value * radix as f64 + digit as f64;
                if !value.is_finite() {
                    return Value::Error;
                }
            }
            Value::Number(if negative { -value } else { value })
        }
        "base" => {
            if args.len() < 2 || args.len() > 3 {
                return Value::Error;
            }
            let Some(number) = eval(&args[0], ctx).number() else {
                return Value::Error;
            };
            let Some(radix) = eval(&args[1], ctx).number().and_then(radix_of) else {
                return Value::Error;
            };
            let min_length = match args.get(2) {
                Some(arg) => match eval(arg, ctx).number() {
                    Some(value)
                        if value.is_finite()
                            && value >= 0.0
                            && value.trunc() == value
                            && value <= 255.0 =>
                    {
                        value as usize
                    }
                    _ => return Value::Error,
                },
                None => 0,
            };
            if !number.is_finite() || number < 0.0 || number.trunc() != number {
                return Value::Error;
            }
            if number > u64::MAX as f64 {
                return Value::Error;
            }
            let digits = to_base(number as u64, radix);
            let padded = if digits.len() < min_length {
                format!("{}{digits}", "0".repeat(min_length - digits.len()))
            } else {
                digits
            };
            Value::Text(padded)
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
                    if !return_type.is_finite() || return_type.fract() != 0.0 {
                        return Value::Error;
                    }
                    let return_type = return_type as u32;
                    if return_type == 21 {
                        return Value::Number(iso_week_number(year, month, day) as f64);
                    }
                    // 11–17 map Monday–Sunday; 1/2 are Sunday/Monday.
                    let start = match return_type {
                        1 | 17 => 1,
                        2 | 11 => 2,
                        12 => 3,
                        13 => 4,
                        14 => 5,
                        15 => 6,
                        16 => 7,
                        _ => return Value::Error,
                    };
                    let offset = (weekday(year, 1, 1) + 7 - start) % 7;
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
        "networkdays.intl" | "workday.intl" => {
            if args.len() < 2 || args.len() > 4 {
                return Value::Error;
            }
            let Some(mask) = weekend_mask(args.get(2), ctx) else {
                return Value::Error;
            };
            let holidays = if args.len() > 3 {
                match holiday_days(&args[3..], ctx) {
                    Ok(holidays) => holidays,
                    Err(_) => return Value::Error,
                }
            } else {
                Vec::new()
            };
            if lower == "networkdays.intl" {
                let start = eval(&args[0], ctx).text();
                let end = eval(&args[1], ctx).text();
                let (Some(start), Some(end)) = (parse_date(&start), parse_date(&end)) else {
                    return Value::Error;
                };
                match networkdays_with_mask(start, end, &mask, &holidays) {
                    Some(count) => Value::Number(count as f64),
                    None => Value::Error,
                }
            } else {
                let start = eval(&args[0], ctx).text();
                let Some(days) = eval(&args[1], ctx).number() else {
                    return Value::Error;
                };
                if !days.is_finite() {
                    return Value::Error;
                }
                let Some(start) = parse_date(&start) else {
                    return Value::Error;
                };
                match workday_with_mask(start, days.trunc() as i64, &mask, &holidays) {
                    Some(date) => Value::Text(date),
                    None => Value::Error,
                }
            }
        }
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
    if args.len() > 6 {
        return Value::Error;
    }
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
    let mut match_mode = 0u32;
    let mut match_end = false;
    let mut if_not_found = None;
    if let Some(arg) = args.get(3) {
        let value = eval(arg, ctx);
        if let Some(number) = value.number() {
            if !number.is_finite() || !matches!(number as u32, 0 | 1) || number.fract() != 0.0 {
                return Value::Error;
            }
            match_mode = number as u32;
        } else {
            if_not_found = Some(value.text());
        }
    }
    if let Some(arg) = args.get(4) {
        let Some(number) = eval(arg, ctx).number() else {
            return Value::Error;
        };
        if !number.is_finite() || !matches!(number as u32, 0 | 1) || number.fract() != 0.0 {
            return Value::Error;
        }
        match_end = number as u32 == 1;
    }
    if let Some(arg) = args.get(5) {
        if_not_found = Some(eval(arg, ctx).text());
    }
    let miss = || match &if_not_found {
        Some(text) => Value::Text(text.clone()),
        None => Value::Text("#N/A".to_string()),
    };
    let haystack = if match_mode == 1 {
        text.to_ascii_lowercase()
    } else {
        text.clone()
    };
    let needle = if match_mode == 1 {
        delimiter.to_ascii_lowercase()
    } else {
        delimiter.clone()
    };
    let mut cursor = 0usize;
    let mut where_delimiter = None;
    for _ in 0..instance as usize {
        match haystack[cursor..].find(&needle) {
            Some(index) => {
                where_delimiter = Some(cursor + index);
                cursor += index + delimiter.len();
            }
            None => {
                where_delimiter = None;
                break;
            }
        }
    }
    match where_delimiter {
        Some(index) if before => Value::Text(text[..index].to_string()),
        Some(index) => Value::Text(text[index + delimiter.len()..].to_string()),
        None if match_end => Value::Text(if before { text.clone() } else { String::new() }),
        None => miss(),
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

fn weekend_mask(arg: Option<&Expr>, ctx: &Ctx<'_>) -> Option<[bool; 7]> {
    let Some(arg) = arg else {
        return Some([false, false, false, false, false, true, true]);
    };
    let raw = eval(arg, ctx).text();
    let text = raw.trim();
    // A 7-character 0/1 string is a weekend mask; only otherwise is it a code.
    if text.chars().count() == 7 && text.chars().all(|ch| ch == '0' || ch == '1') {
        let mut mask = [false; 7];
        for (index, ch) in text.chars().enumerate() {
            mask[index] = ch == '1';
        }
        if mask.iter().all(|weekend| *weekend) {
            return None;
        }
        return Some(mask);
    }
    text.parse::<u64>().ok().and_then(weekend_mask_from_number)
}

fn weekend_mask_from_number(code: u64) -> Option<[bool; 7]> {
    let mask = match code {
        1 => [false, false, false, false, false, true, true],
        2 => [true, false, false, false, false, false, true],
        3 => [true, true, false, false, false, false, false],
        4 => [false, true, true, false, false, false, false],
        5 => [false, false, true, true, false, false, false],
        6 => [false, false, false, true, true, false, false],
        7 => [false, false, false, false, true, true, false],
        11 => [false, false, false, false, false, false, true],
        12 => [true, false, false, false, false, false, false],
        13 => [false, true, false, false, false, false, false],
        14 => [false, false, true, false, false, false, false],
        15 => [false, false, false, true, false, false, false],
        16 => [false, false, false, false, true, false, false],
        17 => [false, false, false, false, false, true, false],
        _ => return None,
    };
    Some(mask)
}

fn mask_index(day: i64, mask: &[bool; 7]) -> Option<bool> {
    let weekday = (day + 4).rem_euclid(7) + 1;
    let index = ((weekday + 5) % 7) as usize;
    mask.get(index).copied()
}

fn networkdays_with_mask(
    start: (i32, u32, u32),
    end: (i32, u32, u32),
    mask: &[bool; 7],
    holidays: &[i64],
) -> Option<u64> {
    let start_day = days_from_civil(start.0, start.1, start.2);
    let end_day = days_from_civil(end.0, end.1, end.2);
    if end_day < start_day {
        return None;
    }
    let workdays_per_week = mask.iter().filter(|weekend| !**weekend).count() as u64;
    if workdays_per_week == 0 {
        return None;
    }
    let total = (end_day - start_day + 1) as u64;
    let full_weeks = total / 7 * workdays_per_week;
    let remainder = total % 7;
    let start_index = ((weekday(start.0, start.1, start.2) + 5) % 7) as usize;
    let mut extra = 0;
    for step in 0..remainder {
        if !mask[(start_index + step as usize) % 7] {
            extra += 1;
        }
    }
    let mut count = full_weeks + extra;
    let mut unique = std::collections::HashSet::new();
    for holiday in holidays {
        if *holiday >= start_day
            && *holiday <= end_day
            && !mask_index(*holiday, mask).unwrap_or(true)
            && unique.insert(*holiday)
        {
            count = count.saturating_sub(1);
        }
    }
    Some(count)
}

fn workday_with_mask(
    start: (i32, u32, u32),
    days: i64,
    mask: &[bool; 7],
    holidays: &[i64],
) -> Option<String> {
    if mask.iter().all(|weekend| *weekend) {
        return None;
    }
    let mut civil = days_from_civil(start.0, start.1, start.2);
    let step = days.signum();
    let mut left = days.abs();
    while left > 0 {
        civil += step;
        if mask_index(civil, mask).unwrap_or(true) {
            continue;
        }
        if holidays.contains(&civil) {
            continue;
        }
        left -= 1;
    }
    let (year, month, day) = civil_from_days(civil);
    Some(format!("{year:04}-{month:02}-{day:02}"))
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

fn payment_parts(
    rate: f64,
    period: f64,
    nper: f64,
    pv: f64,
    fv: f64,
    payment_type: f64,
) -> Option<(f64, f64)> {
    let payment = if rate == 0.0 {
        -(pv + fv) / nper
    } else {
        let growth = (1.0 + rate).powf(nper);
        -(fv + pv * growth) * rate / ((1.0 + rate * payment_type) * (growth - 1.0))
    };
    let interest = if rate == 0.0 || (payment_type == 1.0 && period == 1.0) {
        0.0
    } else if payment_type == 1.0 {
        let months = period - 2.0;
        let balance = pv * (1.0 + rate).powf(months)
            + payment * ((1.0 + rate).powf(months + 1.0) - 1.0) / rate;
        -balance * rate
    } else {
        let months = period - 1.0;
        let balance =
            pv * (1.0 + rate).powf(months) + payment * ((1.0 + rate).powf(months) - 1.0) / rate;
        -balance * rate
    };
    if payment.is_finite() && interest.is_finite() {
        Some((interest, payment - interest))
    } else {
        None
    }
}

fn pearson_correlation(y: &[f64], x: &[f64]) -> Option<f64> {
    if y.len() != x.len() || y.len() < 2 {
        return None;
    }
    let n = y.len() as f64;
    let mean_y = y.iter().sum::<f64>() / n;
    let mean_x = x.iter().sum::<f64>() / n;
    let sum_xx = x.iter().map(|value| (value - mean_x).powi(2)).sum::<f64>();
    let sum_yy = y.iter().map(|value| (value - mean_y).powi(2)).sum::<f64>();
    let sum_xy = x
        .iter()
        .zip(y.iter())
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>();
    if sum_xx == 0.0 || sum_yy == 0.0 {
        return None;
    }
    Some(sum_xy / (sum_xx * sum_yy).sqrt())
}

fn to_roman(mut number: u32) -> String {
    if number == 0 {
        return "0".to_string();
    }
    const TABLE: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut result = String::new();
    for (value, numeral) in TABLE {
        while number >= value {
            result.push_str(numeral);
            number -= value;
        }
    }
    result
}

fn from_roman(text: &str) -> Option<u32> {
    let mut total = 0i32;
    let mut previous = 0i32;
    for character in text.chars().rev() {
        let value = match character.to_ascii_uppercase() {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            'L' => 50,
            'C' => 100,
            'D' => 500,
            'M' => 1000,
            _ => return None,
        };
        if value < previous {
            total -= value;
        } else {
            total += value;
            previous = value;
        }
    }
    u32::try_from(total).ok().filter(|value| *value <= 3999)
}

fn ln_gamma(value: f64) -> f64 {
    const COEFFICIENTS: [f64; 9] = [
        0.9999999999998099,
        676.5203681218851,
        -1259.1392167224028,
        771.3234287776531,
        -176.6150291621406,
        12.507343278686905,
        -0.13857109526572012,
        9.984369578019571e-6,
        1.5056327351493116e-7,
    ];
    if value < 0.5 {
        return (std::f64::consts::PI).ln()
            - (std::f64::consts::PI * value).sin().ln()
            - ln_gamma(1.0 - value);
    }
    let shifted = value - 1.0;
    let mut series = COEFFICIENTS[0];
    for (index, coefficient) in COEFFICIENTS.iter().enumerate().skip(1) {
        series += coefficient / (shifted + index as f64);
    }
    let t = shifted + 7.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (shifted + 0.5) * t.ln() - t + series.ln()
}

fn format_number(value: f64) -> String {
    if value == 0.0 {
        "0".to_string()
    } else {
        value.to_string()
    }
}

fn format_complex(real: f64, imaginary: f64, suffix: &str) -> String {
    let normalize = |value: f64| {
        if value.abs() < 1e-12 {
            0.0
        } else if (value - value.round()).abs() < 1e-12 {
            value.round()
        } else {
            value
        }
    };
    let real = normalize(real);
    let imaginary = normalize(imaginary);
    if imaginary == 0.0 {
        return format_number(real);
    }
    let imaginary_part = if imaginary == 1.0 {
        suffix.to_string()
    } else if imaginary == -1.0 {
        format!("-{suffix}")
    } else {
        format!("{}{suffix}", format_number(imaginary))
    };
    if real == 0.0 {
        imaginary_part
    } else if imaginary > 0.0 {
        format!("{}+{imaginary_part}", format_number(real))
    } else {
        format!("{}{}", format_number(real), imaginary_part)
    }
}

fn parse_complex(text: &str) -> Option<(f64, f64, String)> {
    let trimmed = text.trim();
    let (body, suffix) = match trimmed.chars().last() {
        Some(character)
            if character.eq_ignore_ascii_case(&'i') || character.eq_ignore_ascii_case(&'j') =>
        {
            (
                &trimmed[..trimmed.len() - 1],
                character.to_ascii_lowercase().to_string(),
            )
        }
        _ => (trimmed, "i".to_string()),
    };
    if body.is_empty() {
        return Some((0.0, 1.0, suffix));
    }
    let bytes = body.as_bytes();
    let mut split = None;
    for index in (1..bytes.len()).rev() {
        if (bytes[index] == b'+' || bytes[index] == b'-')
            && bytes[index - 1] != b'e'
            && bytes[index - 1] != b'E'
        {
            split = Some(index);
            break;
        }
    }
    if let Some(index) = split {
        let real = body[..index].trim().parse::<f64>().ok()?;
        let imaginary_text = body[index + 1..].trim();
        let sign = if bytes[index] == b'-' { -1.0 } else { 1.0 };
        let imaginary = if imaginary_text.is_empty() {
            sign
        } else {
            imaginary_text.parse::<f64>().ok()? * sign
        };
        if !real.is_finite() || !imaginary.is_finite() {
            return None;
        }
        Some((real, imaginary, suffix))
    } else {
        let imaginary = if body == "+" {
            1.0
        } else if body == "-" {
            -1.0
        } else {
            body.parse::<f64>().ok()?
        };
        if !imaginary.is_finite() {
            return None;
        }
        Some((0.0, imaginary, suffix))
    }
}

fn gamma_series(alpha: f64, value: f64) -> f64 {
    const MAX_ITERATIONS: usize = 200;
    const EPSILON: f64 = 1.0e-15;
    let mut ap = alpha;
    let mut sum = 1.0 / alpha;
    let mut term = sum;
    for _ in 0..MAX_ITERATIONS {
        ap += 1.0;
        term *= value / ap;
        sum += term;
        if term.abs() < sum.abs() * EPSILON {
            break;
        }
    }
    sum * (-value + alpha * value.ln() - ln_gamma(alpha)).exp()
}

fn gamma_continued_fraction(alpha: f64, value: f64) -> f64 {
    const MAX_ITERATIONS: usize = 200;
    const EPSILON: f64 = 1.0e-15;
    const TINY: f64 = 1.0e-300;
    let mut b = value + 1.0 - alpha;
    let mut c = 1.0 / TINY;
    let mut d = 1.0 / b;
    let mut result = d;
    for step in 1..=MAX_ITERATIONS {
        let numerator = -(step as f64) * (step as f64 - alpha);
        b += 2.0;
        d = numerator * d + b;
        if d.abs() < TINY {
            d = TINY;
        }
        c = b + numerator / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = d * c;
        result *= delta;
        if (delta - 1.0).abs() < EPSILON {
            break;
        }
    }
    result * (-value + alpha * value.ln() - ln_gamma(alpha)).exp()
}

fn regularized_gamma(alpha: f64, value: f64) -> f64 {
    if value <= 0.0 {
        return 0.0;
    }
    if value < alpha + 1.0 {
        gamma_series(alpha, value)
    } else {
        1.0 - gamma_continued_fraction(alpha, value)
    }
}

fn beta_continued_fraction(alpha: f64, beta: f64, value: f64) -> f64 {
    const MAX_ITERATIONS: usize = 200;
    const EPSILON: f64 = 3.0e-14;
    const TINY: f64 = 1.0e-300;
    let sum = alpha + beta;
    let alpha_plus_one = alpha + 1.0;
    let alpha_minus_one = alpha - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - sum * value / alpha_plus_one;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut result = d;
    for step in 1..=MAX_ITERATIONS {
        let twice = 2.0 * step as f64;
        let numerator = step as f64 * (beta - step as f64) * value
            / ((alpha_minus_one + twice) * (alpha + twice));
        d = 1.0 + numerator * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + numerator / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        result *= d * c;
        let numerator = -(alpha + step as f64) * (sum + step as f64) * value
            / ((alpha + twice) * (alpha_plus_one + twice));
        d = 1.0 + numerator * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + numerator / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = d * c;
        result *= delta;
        if (delta - 1.0).abs() < EPSILON {
            break;
        }
    }
    result
}

fn regularized_beta(value: f64, alpha: f64, beta: f64) -> f64 {
    if value <= 0.0 {
        return 0.0;
    }
    if value >= 1.0 {
        return 1.0;
    }
    let factor = (ln_gamma(alpha + beta) - ln_gamma(alpha) - ln_gamma(beta)
        + alpha * value.ln()
        + beta * (1.0 - value).ln())
    .exp();
    if value < (alpha + 1.0) / (alpha + beta + 2.0) {
        factor * beta_continued_fraction(alpha, beta, value) / alpha
    } else {
        1.0 - factor * beta_continued_fraction(beta, alpha, 1.0 - value) / beta
    }
}

fn normal_inverse(probability: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.38357751867269e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    if !(0.0..1.0).contains(&probability) {
        return f64::NAN;
    }
    const LOW: f64 = 0.02425;
    const HIGH: f64 = 1.0 - LOW;
    if probability < LOW {
        let q = (-2.0 * probability.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if probability <= HIGH {
        let q = probability - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - probability).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

fn erf_approx(value: f64) -> f64 {
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    let absolute = value.abs();
    if absolute == 0.0 {
        return 0.0;
    }
    if absolute > 5.0 {
        return sign;
    }
    let t = 1.0 / (1.0 + 0.3275911 * absolute);
    let polynomial = (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736)
        * t
        + 0.254829592)
        * t;
    sign * (1.0 - polynomial * (-absolute * absolute).exp())
}

fn factorial(number: u64) -> f64 {
    let mut result = 1.0;
    for value in 2..=number {
        result *= value as f64;
    }
    result
}

fn linear_regression(y: &[f64], x: &[f64]) -> Option<(f64, f64)> {
    if y.len() != x.len() || y.len() < 2 {
        return None;
    }
    let n = y.len() as f64;
    let mean_y = y.iter().sum::<f64>() / n;
    let mean_x = x.iter().sum::<f64>() / n;
    let sum_xx = x.iter().map(|value| (value - mean_x).powi(2)).sum::<f64>();
    let sum_xy = x
        .iter()
        .zip(y.iter())
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>();
    if sum_xx == 0.0 {
        return None;
    }
    let slope = sum_xy / sum_xx;
    Some((slope, mean_y - slope * mean_x))
}

fn sample_skewness(values: &[f64]) -> Option<f64> {
    let n = values.len();
    if n < 3 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let sum_squares = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>();
    if sum_squares == 0.0 {
        return None;
    }
    let standard_deviation = (sum_squares / (n - 1) as f64).sqrt();
    let sum_cubes = values
        .iter()
        .map(|v| ((v - mean) / standard_deviation).powi(3))
        .sum::<f64>();
    Some(n as f64 / ((n - 1) as f64 * (n - 2) as f64) * sum_cubes)
}

fn sample_kurtosis(values: &[f64]) -> Option<f64> {
    let n = values.len();
    if n < 4 {
        return None;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let sum_squares = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>();
    if sum_squares == 0.0 {
        return None;
    }
    let standard_deviation = (sum_squares / (n - 1) as f64).sqrt();
    let sum_fourths = values
        .iter()
        .map(|v| ((v - mean) / standard_deviation).powi(4))
        .sum::<f64>();
    let n = n as f64;
    let first = n * (n + 1.0) / ((n - 1.0) * (n - 2.0) * (n - 3.0)) * sum_fourths;
    Some(first - 3.0 * (n - 1.0).powi(2) / ((n - 2.0) * (n - 3.0)))
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
fn radix_of(value: f64) -> Option<u32> {
    if value.is_finite() && value.trunc() == value && (2.0..=36.0).contains(&value) {
        Some(value as u32)
    } else {
        None
    }
}

fn to_base(mut number: u64, radix: u32) -> String {
    if number == 0 {
        return "0".to_string();
    }
    let mut digits = String::new();
    while number > 0 {
        let digit = (number % radix as u64) as u32;
        digits.push(
            char::from_digit(digit, radix)
                .unwrap_or('0')
                .to_ascii_uppercase(),
        );
        number /= radix as u64;
    }
    digits.chars().rev().collect()
}

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
