//! Excel-style formulas for computed columns.
//!
//! A formula column is evaluated **per row, at read time** — nothing is stored.
//! That is what keeps a computed column free: adding `total = amount * qty` to a
//! 100-million-row file costs one arithmetic operation per rendered row, not a
//! second copy of the data.
//!
//! Supported grammar (deliberately small — a spreadsheet's full function zoo is
//! not worth a parser nobody can predict):
//!
//! ```text
//! expr    := term (('+' | '-') term)*
//! term    := factor (('*' | '/' | '%') factor)*
//! factor  := number | column | '(' expr ')' | '-' factor | function '(' args ')'
//! column  := name | [name]        (brackets for names with spaces)
//! function:= round(x[, digits]) | abs(x) | min(...) | max(...) | len(x) | upper(x) | lower(x)
//! ```
//!
//! Cell values that are not numbers evaluate to `#VALUE!` in numeric positions,
//! exactly like a spreadsheet, instead of silently becoming zero.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
    Comma,
}

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Number(f64),
    Column(String),
    Binary(char, Box<Expr>, Box<Expr>),
    Negate(Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Clone, Debug)]
pub struct Formula {
    source: String,
    expr: Expr,
}

/// A value a formula produced.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    Text(String),
    /// Non-numeric input where a number was required (spreadsheet `#VALUE!`).
    Error,
}

impl Value {
    /// Render the way a cell would show it.
    pub fn render(&self) -> String {
        match self {
            Self::Number(n) => format_number(*n),
            Self::Text(s) => s.clone(),
            Self::Error => "#VALUE!".to_string(),
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Number(n) => format_number(*n),
            Self::Text(s) => s.clone(),
            Self::Error => "#VALUE!".to_string(),
        }
    }
}

/// Same formatting rule as aggregated results: integers as integers, everything
/// else trimmed to the precision f64 can actually support.
fn format_number(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.6}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed == "-0" || trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

impl Formula {
    /// Parse a formula, reporting the offending piece on failure.
    pub fn parse(source: &str) -> Result<Self, String> {
        let tokens = tokenize(source)?;
        let mut parser = Parser { tokens, pos: 0 };
        let expr = parser.expr()?;
        if parser.pos != parser.tokens.len() {
            return Err(format!(
                "unexpected '{}' in formula '{}'",
                parser.tokens[parser.pos].describe(),
                source
            ));
        }
        Ok(Self {
            source: source.to_string(),
            expr,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Column names the formula reads (used to check them against the header).
    pub fn references(&self) -> Vec<String> {
        let mut out = Vec::new();
        collect_refs(&self.expr, &mut out);
        out.sort();
        out.dedup();
        out
    }

    /// Evaluate against one row.
    pub fn eval(&self, row: &HashMap<String, String>) -> Value {
        eval(&self.expr, row)
    }
}

fn collect_refs(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Column(name) => out.push(name.clone()),
        Expr::Binary(_, l, r) => {
            collect_refs(l, out);
            collect_refs(r, out);
        }
        Expr::Negate(x) => collect_refs(x, out),
        Expr::Call(_, args) => args.iter().for_each(|a| collect_refs(a, out)),
        Expr::Number(_) => {}
    }
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Self::Number(n) => format_number(*n),
            Self::Ident(s) => s.clone(),
            Self::Plus => "+".into(),
            Self::Minus => "-".into(),
            Self::Star => "*".into(),
            Self::Slash => "/".into(),
            Self::Percent => "%".into(),
            Self::LParen => "(".into(),
            Self::RParen => ")".into(),
            Self::Comma => ",".into(),
        }
    }
}

fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '+' => {
                chars.next();
                tokens.push(Token::Plus);
            }
            '-' => {
                chars.next();
                tokens.push(Token::Minus);
            }
            '*' => {
                chars.next();
                tokens.push(Token::Star);
            }
            '/' => {
                chars.next();
                tokens.push(Token::Slash);
            }
            '%' => {
                chars.next();
                tokens.push(Token::Percent);
            }
            '(' => {
                chars.next();
                tokens.push(Token::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            ',' => {
                chars.next();
                tokens.push(Token::Comma);
            }
            '[' => {
                chars.next();
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some(']') => break,
                        Some(ch) => name.push(ch),
                        None => return Err("unclosed '[' in formula".to_string()),
                    }
                }
                tokens.push(Token::Ident(name));
            }
            c if c.is_ascii_digit() || c == '.' => {
                let mut text = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_ascii_digit() || ch == '.' {
                        text.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let n: f64 = text
                    .parse()
                    .map_err(|_| format!("bad number '{text}' in formula"))?;
                tokens.push(Token::Number(n));
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut name = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_alphanumeric() || ch == '_' {
                        name.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Ident(name));
            }
            '$' => {
                return Err(
                    "A1-style references ($A$1) are not supported; use the column name".to_string(),
                )
            }
            other => return Err(format!("unexpected character '{other}' in formula")),
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn take(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut left = self.term()?;
        loop {
            match self.peek() {
                Some(Token::Plus) => {
                    self.take();
                    left = Expr::Binary('+', Box::new(left), Box::new(self.term()?));
                }
                Some(Token::Minus) => {
                    self.take();
                    left = Expr::Binary('-', Box::new(left), Box::new(self.term()?));
                }
                _ => return Ok(left),
            }
        }
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut left = self.factor()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => '*',
                Some(Token::Slash) => '/',
                Some(Token::Percent) => '%',
                _ => return Ok(left),
            };
            self.take();
            left = Expr::Binary(op, Box::new(left), Box::new(self.factor()?));
        }
    }

    fn factor(&mut self) -> Result<Expr, String> {
        match self.take() {
            Some(Token::Number(n)) => Ok(Expr::Number(n)),
            Some(Token::Minus) => Ok(Expr::Negate(Box::new(self.factor()?))),
            Some(Token::LParen) => {
                let inner = self.expr()?;
                match self.take() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err("missing ')' in formula".to_string()),
                }
            }
            Some(Token::Ident(name)) => {
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.take();
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some(Token::RParen)) {
                        loop {
                            args.push(self.expr()?);
                            match self.peek() {
                                Some(Token::Comma) => {
                                    self.take();
                                }
                                _ => break,
                            }
                        }
                    }
                    match self.take() {
                        Some(Token::RParen) => Ok(Expr::Call(name, args)),
                        _ => Err("missing ')' after function arguments".to_string()),
                    }
                } else {
                    Ok(Expr::Column(name))
                }
            }
            other => Err(format!(
                "expected a value, found {}",
                other
                    .map(|t| t.describe())
                    .unwrap_or_else(|| "end of formula".to_string())
            )),
        }
    }
}

fn eval(expr: &Expr, row: &HashMap<String, String>) -> Value {
    match expr {
        Expr::Number(n) => Value::Number(*n),
        Expr::Column(name) => match row.get(name) {
            None => Value::Error,
            Some(raw) => match raw.trim().parse::<f64>() {
                Ok(n) if n.is_finite() => Value::Number(n),
                _ if raw.trim().is_empty() => Value::Number(0.0),
                _ => Value::Text(raw.clone()),
            },
        },
        Expr::Negate(x) => match eval(x, row) {
            Value::Number(n) => Value::Number(-n),
            _ => Value::Error,
        },
        Expr::Binary(op, l, r) => {
            let a = eval(l, row);
            let b = eval(r, row);
            // `&`-style concatenation is out of scope; arithmetic needs numbers.
            let (Some(x), Some(y)) = (a.number(), b.number()) else {
                return Value::Error;
            };
            match op {
                '+' => Value::Number(x + y),
                '-' => Value::Number(x - y),
                '*' => Value::Number(x * y),
                '/' => {
                    if y == 0.0 {
                        Value::Error
                    } else {
                        Value::Number(x / y)
                    }
                }
                '%' => {
                    if y == 0.0 {
                        Value::Error
                    } else {
                        Value::Number(x % y)
                    }
                }
                _ => Value::Error,
            }
        }
        Expr::Call(name, args) => match name.to_ascii_lowercase().as_str() {
            "round" => {
                let Some(x) = args.first().and_then(|a| eval(a, row).number()) else {
                    return Value::Error;
                };
                let digits = args
                    .get(1)
                    .and_then(|a| eval(a, row).number())
                    .unwrap_or(0.0) as i32;
                let factor = 10f64.powi(digits);
                Value::Number((x * factor).round() / factor)
            }
            "abs" => match args.first().map(|a| eval(a, row)) {
                Some(Value::Number(n)) => Value::Number(n.abs()),
                _ => Value::Error,
            },
            "min" | "max" => {
                let mut nums = Vec::new();
                for arg in args {
                    match eval(arg, row).number() {
                        Some(n) => nums.push(n),
                        None => return Value::Error,
                    }
                }
                if nums.is_empty() {
                    return Value::Error;
                }
                let picked = if name.eq_ignore_ascii_case("min") {
                    nums.iter().cloned().fold(f64::INFINITY, f64::min)
                } else {
                    nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                };
                Value::Number(picked)
            }
            "len" => match args.first().map(|a| eval(a, row)) {
                Some(v) => Value::Number(v.text().chars().count() as f64),
                None => Value::Error,
            },
            "upper" => match args.first().map(|a| eval(a, row)) {
                Some(v) => Value::Text(v.text().to_uppercase()),
                None => Value::Error,
            },
            "lower" => match args.first().map(|a| eval(a, row)) {
                Some(v) => Value::Text(v.text().to_lowercase()),
                None => Value::Error,
            },
            _ => Value::Error,
        },
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

    fn eval_str(formula: &str, cells: &[(&str, &str)]) -> String {
        let f = Formula::parse(formula).unwrap();
        f.eval(&row(cells)).render()
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(
            eval_str("amount * qty", &[("amount", "10"), ("qty", "3")]),
            "30"
        );
        assert_eq!(eval_str("1 + 2 * 3", &[]), "7");
        assert_eq!(eval_str("(1 + 2) * 3", &[]), "9");
        assert_eq!(eval_str("-4 + 10", &[]), "6");
        assert_eq!(eval_str("10 % 3", &[]), "1");
    }

    #[test]
    fn column_names_with_spaces_use_brackets() {
        assert_eq!(eval_str("[客户编号] + 1", &[("客户编号", "41")]), "42");
    }

    #[test]
    fn division_by_zero_is_an_error_not_a_panic() {
        assert_eq!(eval_str("1/0", &[]), "#VALUE!");
        assert_eq!(eval_str("1/(2-2)", &[]), "#VALUE!");
    }

    #[test]
    fn non_numeric_cells_are_an_error_in_numeric_positions() {
        assert_eq!(eval_str("amount + 1", &[("amount", "N/A")]), "#VALUE!");
        // An empty cell counts as zero, which is what a spreadsheet does.
        assert_eq!(eval_str("amount + 1", &[("amount", "")]), "1");
        // A missing column is an error, not zero: it means the formula is wrong.
        assert_eq!(eval_str("nope + 1", &[]), "#VALUE!");
    }

    #[test]
    fn functions() {
        assert_eq!(eval_str("round(3.14159, 2)", &[]), "3.14");
        assert_eq!(
            eval_str("round(amount / qty, 1)", &[("amount", "10"), ("qty", "3")]),
            "3.3"
        );
        assert_eq!(eval_str("abs(0 - 5)", &[]), "5");
        assert_eq!(
            eval_str("max(amount, qty)", &[("amount", "10"), ("qty", "3")]),
            "10"
        );
        assert_eq!(eval_str("min(5, 2, 9)", &[]), "2");
        assert_eq!(eval_str("len(city)", &[("city", "CDMX")]), "4");
        assert_eq!(eval_str("upper(city)", &[("city", "cdmx")]), "CDMX");
        assert_eq!(eval_str("lower(city)", &[("city", "TiJUANA")]), "tijuana");
        assert_eq!(eval_str("nope(1)", &[]), "#VALUE!");
    }

    #[test]
    fn references_are_reported_for_header_validation() {
        let f = Formula::parse("round(amount * qty, 2) + [城市]").unwrap();
        // Sorted and deduplicated, so a caller can validate against the header.
        assert_eq!(
            f.references(),
            vec!["amount", "qty", "城市"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn syntax_errors_are_reported_clearly() {
        assert!(Formula::parse("amount *").is_err());
        assert!(Formula::parse("(1 + 2").is_err());
        assert!(Formula::parse("[unclosed").is_err());
        assert!(Formula::parse("1 + $").is_err());
        let err = Formula::parse("amount @ 2").unwrap_err();
        assert!(err.contains('@'), "{err}");
    }

    #[test]
    fn numbers_render_without_float_noise() {
        assert_eq!(eval_str("0.1 + 0.2", &[]), "0.3");
        assert_eq!(eval_str("10 / 4", &[]), "2.5");
        assert_eq!(eval_str("100 / 3", &[]), "33.333333");
    }
}
