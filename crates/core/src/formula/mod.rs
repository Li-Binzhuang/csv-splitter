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

mod functions;

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
    Ampersand,
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
    Str(String),
    LParen,
    RParen,
    Comma,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    Number(f64),
    Text(String),
    Column(String),
    Binary(char, Box<Expr>, Box<Expr>),
    Compare(&'static str, Box<Expr>, Box<Expr>),
    Negate(Box<Expr>),
    Not(Box<Expr>),
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
    Bool(bool),
    Text(String),
    /// Non-numeric input where a number was required (spreadsheet `#VALUE!`).
    Error,
}

impl Value {
    /// Render the way a cell would show it.
    pub fn render(&self) -> String {
        match self {
            Self::Number(n) => format_number(*n),
            Self::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
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
        self.render()
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
        Expr::Binary(_, l, r) | Expr::Compare(_, l, r) => {
            collect_refs(l, out);
            collect_refs(r, out);
        }
        Expr::Negate(x) | Expr::Not(x) => collect_refs(x, out),
        Expr::Text(_) => {}
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
            Self::Ampersand => "&".into(),
            Self::Gt => ">".into(),
            Self::Ge => ">=".into(),
            Self::Lt => "<".into(),
            Self::Le => "<=".into(),
            Self::Eq => "=".into(),
            Self::Ne => "<>".into(),
            Self::Str(text) => format!("\"{text}\""),
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
            '&' => {
                chars.next();
                tokens.push(Token::Ampersand);
            }
            '>' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(Token::Ge);
                } else {
                    tokens.push(Token::Gt);
                }
            }
            '<' => {
                chars.next();
                match chars.peek() {
                    Some('=') => {
                        chars.next();
                        tokens.push(Token::Le);
                    }
                    Some('>') => {
                        chars.next();
                        tokens.push(Token::Ne);
                    }
                    _ => tokens.push(Token::Lt),
                }
            }
            '=' => {
                chars.next();
                tokens.push(Token::Eq);
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut text = String::new();
                loop {
                    match chars.next() {
                        Some(ch) if ch == quote => break,
                        Some(ch) => text.push(ch),
                        None => return Err("unclosed string in formula".to_string()),
                    }
                }
                tokens.push(Token::Str(text));
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
        let left = self.concat()?;
        let op = match self.peek() {
            Some(Token::Gt) => Some(">"),
            Some(Token::Ge) => Some(">="),
            Some(Token::Lt) => Some("<"),
            Some(Token::Le) => Some("<="),
            Some(Token::Eq) => Some("="),
            Some(Token::Ne) => Some("<>"),
            _ => None,
        };
        match op {
            Some(op) => {
                self.take();
                Ok(Expr::Compare(op, Box::new(left), Box::new(self.concat()?)))
            }
            None => Ok(left),
        }
    }

    /// `a & b & c`: text concatenation binds tighter than comparison, looser than
    /// arithmetic, which is where a spreadsheet puts it.
    fn concat(&mut self) -> Result<Expr, String> {
        let mut left = self.additive()?;
        while matches!(self.peek(), Some(Token::Ampersand)) {
            self.take();
            left = Expr::Binary('&', Box::new(left), Box::new(self.additive()?));
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, String> {
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
            Some(Token::Str(text)) => Ok(Expr::Text(text)),
            Some(Token::Plus) => self.factor(),
            Some(Token::Minus) => Ok(Expr::Negate(Box::new(self.factor()?))),
            Some(Token::Ident(name)) if name.eq_ignore_ascii_case("not") => {
                Ok(Expr::Not(Box::new(self.factor()?)))
            }
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

/// Evaluate one expression against a row.
pub(crate) fn eval(expr: &Expr, row: &HashMap<String, String>) -> Value {
    match expr {
        Expr::Number(n) => Value::Number(*n),
        Expr::Text(t) => Value::Text(t.clone()),
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
        Expr::Not(x) => Value::Bool(!truthy(&eval(x, row))),
        Expr::Binary(op, l, r) => {
            let a = eval(l, row);
            let b = eval(r, row);
            match op {
                '&' => Value::Text(format!("{}{}", a.text(), b.text())),
                '+' | '-' | '*' | '/' | '%' => {
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
                _ => Value::Error,
            }
        }
        Expr::Compare(op, l, r) => {
            let a = eval(l, row);
            let b = eval(r, row);
            let ordering = match (a.number(), b.number()) {
                (Some(x), Some(y)) => x.partial_cmp(&y),
                _ => Some(a.text().cmp(&b.text())),
            };
            let Some(ordering) = ordering else {
                return Value::Bool(false);
            };
            Value::Bool(match *op {
                ">" => ordering.is_gt(),
                ">=" => ordering.is_ge(),
                "<" => ordering.is_lt(),
                "<=" => ordering.is_le(),
                "=" => ordering.is_eq(),
                "<>" => ordering.is_ne(),
                _ => false,
            })
        }
        Expr::Call(name, args) => functions::call(name, args, row),
    }
}

/// Truthiness, the way a spreadsheet decides it: non-zero numbers, `true`/`yes`/`1`
/// as text, and anything else false.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => *n != 0.0,
        Value::Text(t) => {
            let lower = t.trim().to_ascii_lowercase();
            matches!(lower.as_str(), "true" | "yes" | "1")
        }
        Value::Error => false,
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
    fn conditionals_only_evaluate_the_branch_they_need() {
        // The unguarded branch would divide by zero; if() must not evaluate it.
        assert_eq!(
            eval_str(
                "if(qty = 0, 0, amount / qty)",
                &[("qty", "0"), ("amount", "10")]
            ),
            "0"
        );
        assert_eq!(
            eval_str(
                "if(qty = 0, 0, amount / qty)",
                &[("qty", "2"), ("amount", "10")]
            ),
            "5"
        );
        assert_eq!(
            eval_str("if(amount > 100, \"big\", \"small\")", &[("amount", "50")]),
            "small"
        );
        assert_eq!(
            eval_str("if(amount > 100, \"big\", \"small\")", &[("amount", "500")]),
            "big"
        );
        assert_eq!(eval_str("if(1, 2, 3)", &[]), "2");
        // A one-argument if has no else; it answers FALSE rather than erroring.
        assert_eq!(eval_str("if(0, 2)", &[]), "FALSE");
    }

    #[test]
    fn comparisons_return_booleans() {
        assert_eq!(eval_str("amount >= 10", &[("amount", "10")]), "TRUE");
        assert_eq!(eval_str("amount <> 10", &[("amount", "11")]), "TRUE");
        assert_eq!(eval_str("city = \"CDMX\"", &[("city", "CDMX")]), "TRUE");
        assert_eq!(eval_str("city = \"cdmx\"", &[("city", "CDMX")]), "FALSE");
        assert_eq!(eval_str("not(1)", &[]), "FALSE");
        assert_eq!(eval_str("and(1, 0)", &[]), "FALSE");
        assert_eq!(eval_str("or(1, 0)", &[]), "TRUE");
    }

    #[test]
    fn text_concatenation_and_slicing() {
        assert_eq!(
            eval_str(
                "city & \"-\" & month",
                &[("city", "CDMX"), ("month", "Jan")]
            ),
            "CDMX-Jan"
        );
        assert_eq!(eval_str("left(city, 2)", &[("city", "CDMX")]), "CD");
        assert_eq!(eval_str("right(city, 2)", &[("city", "CDMX")]), "MX");
        assert_eq!(eval_str("mid(city, 2, 2)", &[("city", "CDMX")]), "DM");
        assert_eq!(eval_str("upper(city)", &[("city", "cdmx")]), "CDMX");
        assert_eq!(eval_str("trim(city)", &[("city", "  x  ")]), "x");
        assert_eq!(
            eval_str("replace(city, \"C\", \"X\")", &[("city", "CDMX")]),
            "XDMX"
        );
    }

    #[test]
    fn date_parts_read_iso_prefixes() {
        assert_eq!(eval_str("year(d)", &[("d", "2024-03-15")]), "2024");
        assert_eq!(eval_str("month(d)", &[("d", "2024-03-15")]), "3");
        assert_eq!(eval_str("day(d)", &[("d", "2024-03-15")]), "15");
        // Timestamps work too: the prefix is what matters.
        assert_eq!(eval_str("month(d)", &[("d", "2024-03-15 10:00:00")]), "3");
        // A non-date is an error, not a silent zero.
        assert_eq!(eval_str("year(d)", &[("d", "not a date")]), "#VALUE!");
        assert_eq!(eval_str("month(d)", &[("d", "2024-13-01")]), "#VALUE!");
    }

    #[test]
    fn numeric_helpers() {
        assert_eq!(eval_str("floor(3.7)", &[]), "3");
        assert_eq!(eval_str("ceil(3.2)", &[]), "4");
        assert_eq!(eval_str("int(-3.7)", &[]), "-3");
        assert_eq!(eval_str("sqrt(16)", &[]), "4");
        assert_eq!(eval_str("sqrt(-4)", &[]), "#VALUE!");
        assert_eq!(eval_str("mod(7, 3)", &[]), "1");
        assert_eq!(eval_str("mod(7, 0)", &[]), "#VALUE!");
        assert_eq!(eval_str("iferror(mod(7,0), 0)", &[]), "0");
    }

    #[test]
    fn numbers_render_without_float_noise() {
        assert_eq!(eval_str("0.1 + 0.2", &[]), "0.3");
        assert_eq!(eval_str("10 / 4", &[]), "2.5");
        assert_eq!(eval_str("100 / 3", &[]), "33.333333");
    }
}
