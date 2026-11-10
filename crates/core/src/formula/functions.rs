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

use super::{eval, truthy, Ctx, Expr, Value};

pub(super) fn call(name: &str, args: &[Expr], ctx: &Ctx<'_>) -> Value {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
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
        // Alias for `mid`, which is the name people reach for first.
        "substr" => call("mid", args, ctx),

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
