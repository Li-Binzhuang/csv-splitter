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

pub(super) fn call(name: &str, args: &[Expr], ctx: &Ctx<'_>) -> Value {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        // History functions: they read the state the exporter maintains, so a formula
        // can see the row before it without the file being loaded.
        // Conditional aggregates: the criteria (">1000", "CDMX", "<>10") were compiled
        // and evaluated in the same pre-pass as the plain totals.
        "countifs" | "sumifs" | "averageifs" | "minifs" | "maxifs" => {
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
            let (Some(Expr::Range { from: a, to: b }), Some(Expr::Range { from: c, to: d })) =
                (args.first(), args.get(1))
            else {
                return Value::Error;
            };
            let (Some(left), Some(right)) = (range_cells(a, b, ctx), range_cells(c, d, ctx)) else {
                return Value::Error;
            };
            if left.len() != right.len() || left.is_empty() {
                return Value::Error;
            }
            let mut total = 0.0;
            for (l, r) in left.iter().zip(right.iter()) {
                let l = l.trim().parse::<f64>().unwrap_or(0.0);
                let r = r.trim().parse::<f64>().unwrap_or(0.0);
                if l.is_finite() && r.is_finite() {
                    total += l * r;
                }
            }
            Value::Number(total)
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
        // Sample standard deviation / variance: Excel's STDEV.S and VAR.S. The pre-pass
        // kept the sum of squares, so this is O(1) per row.
        "stdev" | "var" | "stdevp" | "varp" => {
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
        "iferror" => match args.first() {
            Some(arg) => {
                let value = eval(arg, ctx);
                if matches!(value, Value::Error) {
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
        "replace" => {
            let Some(text) = args.first().map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            let Some(from) = args.get(1).map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            let Some(to) = args.get(2).map(|a| eval(a, ctx).text()) else {
                return Value::Error;
            };
            if from.is_empty() {
                return Value::Text(text);
            }
            Value::Text(text.replace(&from, &to))
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
        // Excel's SUBSTITUTE(text, old, new) replaces every occurrence. Our `replace`
        // already had that meaning, so this is the name a spreadsheet user expects.
        "substitute" => call("replace", args, ctx),
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
                Some((year, month, day)) => Value::Number(weekday(year, month, day) as f64),
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

        // ---- row position ---------------------------------------------------
        "row" => Value::Number((ctx.row + 1) as f64),

        // ---- cross-sheet reference -----------------------------------------
        // ref("customers.csv", "cust_no", "name"): this row's cust_no looked up
        // in that sheet. The referenced sheet is already in memory; the current
        // sheet stays streamed.
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
