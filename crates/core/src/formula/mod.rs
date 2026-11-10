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
mod sheetref;
mod state;

pub use state::{RowNeeds, RowState};

pub use sheetref::{Refs, SheetRef, DEFAULT_MAX_KEYS};

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    /// `[name]` — always a column, even when the name looks like a cell (`[A1]`).
    Column(String),
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
    /// `B2` — column B, row 2. Only the current row is resolvable while
    /// streaming, so a reference to another row evaluates to `#REF!`.
    CellRef {
        column: String,
        row: u64,
    },
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
    /// Evaluate against one row, without cross-sheet references.
    pub fn eval(&self, row: &HashMap<String, String>) -> Value {
        let refs = Refs::default();
        let columns: Vec<String> = row.keys().cloned().collect();
        eval(
            &self.expr,
            &Ctx {
                cells: row,
                state: None,
                columns: &columns,
                refs: &refs,
                row: 0,
            },
        )
    }

    /// Evaluate against one row, resolving `ref(...)` against loaded sheets.
    pub fn eval_with(&self, row: &HashMap<String, String>, refs: &Refs) -> Value {
        let columns: Vec<String> = row.keys().cloned().collect();
        self.eval_at(row, &columns, refs, 0)
    }

    /// Evaluate for a specific row number, which `row()` reports.
    pub fn eval_at(
        &self,
        row: &HashMap<String, String>,
        columns: &[String],
        refs: &Refs,
        row_number: u64,
    ) -> Value {
        self.eval_stateful(row, columns, refs, row_number, None)
    }

    /// Evaluate with row history available, so `lag` and `running_total` work.
    pub fn eval_stateful(
        &self,
        row: &HashMap<String, String>,
        columns: &[String],
        refs: &Refs,
        row_number: u64,
        state: Option<&crate::formula::RowState>,
    ) -> Value {
        eval(
            &self.expr,
            &Ctx {
                cells: row,
                state,
                columns,
                refs,
                row: row_number,
            },
        )
    }

    /// Columns this formula needs history for (`lag`, `running_total`).
    pub fn row_needs(&self) -> crate::formula::RowNeeds {
        let mut needs = crate::formula::RowNeeds::default();
        collect_row_needs(&self.expr, &mut needs);
        needs
    }

    /// The `ref(...)` calls this formula makes, so they can be loaded up front.
    pub fn sheet_references(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        collect_sheet_refs(&self.expr, &mut out);
        out
    }
}

/// Walk the expression for `lag(col[, k])` and `running_total(col)`.
///
/// Only literal column names are understood here: `lag([a] , 1)` is a column, but
/// `lag(if(..), 1)` would need the value before deciding the column, which the
/// caller cannot pre-allocate. Such forms evaluate to #VALUE! at run time.
fn collect_row_needs(expr: &Expr, out: &mut crate::formula::RowNeeds) {
    match expr {
        Expr::Call(name, args) if name.eq_ignore_ascii_case("lag") => {
            if let Some(Expr::Column(column)) = args.first() {
                let distance = match args.get(1) {
                    Some(Expr::Number(n)) if *n >= 1.0 => *n as usize,
                    _ => 1,
                };
                out.absorb(&crate::formula::RowNeeds {
                    lags: vec![(column.clone(), distance)],
                    running: Vec::new(),
                });
            }
            for arg in args {
                collect_row_needs(arg, out);
            }
        }
        Expr::Call(name, args)
            if name.eq_ignore_ascii_case("running_total")
                || name.eq_ignore_ascii_case("cumulative")
                || name.eq_ignore_ascii_case("cumsum") =>
        {
            if let Some(Expr::Column(column)) = args.first() {
                out.absorb(&crate::formula::RowNeeds {
                    lags: Vec::new(),
                    running: vec![column.clone()],
                });
            }
            for arg in args {
                collect_row_needs(arg, out);
            }
        }
        Expr::Call(_, args) => args.iter().for_each(|a| collect_row_needs(a, out)),
        Expr::Binary(_, l, r) | Expr::Compare(_, l, r) => {
            collect_row_needs(l, out);
            collect_row_needs(r, out);
        }
        Expr::Negate(x) | Expr::Not(x) => collect_row_needs(x, out),
        Expr::Number(_) | Expr::Text(_) | Expr::Column(_) | Expr::CellRef { .. } => {}
    }
}

fn collect_sheet_refs(expr: &Expr, out: &mut Vec<(String, String, String)>) {
    match expr {
        Expr::Call(name, args) if name.eq_ignore_ascii_case("ref") => {
            if args.len() == 3 {
                if let (Expr::Text(sheet), Expr::Text(key), Expr::Text(value)) =
                    (&args[0], &args[1], &args[2])
                {
                    out.push((sheet.clone(), key.clone(), value.clone()));
                }
            }
            for arg in args {
                collect_sheet_refs(arg, out);
            }
        }
        Expr::Call(_, args) => args.iter().for_each(|a| collect_sheet_refs(a, out)),
        Expr::Binary(_, l, r) | Expr::Compare(_, l, r) => {
            collect_sheet_refs(l, out);
            collect_sheet_refs(r, out);
        }
        Expr::Negate(x) | Expr::Not(x) => collect_sheet_refs(x, out),
        Expr::Number(_) | Expr::Text(_) | Expr::Column(_) | Expr::CellRef { .. } => {}
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
        Expr::Number(_) | Expr::CellRef { .. } => {}
    }
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Self::Number(n) => format_number(*n),
            Self::Ident(s) | Self::Column(s) => s.clone(),
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
                tokens.push(Token::Column(name));
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
            Some(Token::Column(name)) => Ok(Expr::Column(name)),
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
                } else if let Some((column, row)) = split_cell_ref(&name) {
                    Ok(Expr::CellRef { column, row })
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

/// The row a column formula is "written on": the first data row, counting the
/// header as row 1 the way a spreadsheet does. References are relative to it.
const FIRST_DATA_ROW: i64 = 2;

/// Split `B2` into (`B`, 2); `None` when the identifier is not a cell reference.
///
/// The pattern is letters followed by digits, which is how spreadsheets write a
/// cell. A bare `A1`-looking *column name* is therefore read as a reference — the
/// only place this can surprise someone is a column literally named `A1`, and the
/// `[A1]` bracket form still reaches it.
fn split_cell_ref(name: &str) -> Option<(String, u64)> {
    let letters: String = name
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let digits: String = name.chars().skip(letters.len()).collect();
    if letters.is_empty() || digits.is_empty() || letters.len() + digits.len() != name.len() {
        return None;
    }
    let row: u64 = digits.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((letters.to_ascii_uppercase(), row))
}

/// Evaluate one expression against a row.
/// Everything an expression may look at while it is evaluated for one row.
pub(crate) struct Ctx<'a> {
    pub cells: &'a HashMap<String, String>,
    /// History and running totals, when the formula needs them.
    pub state: Option<&'a crate::formula::RowState>,
    /// Column order, so `A1`-style references can be turned into column names.
    pub columns: &'a [String],
    pub refs: &'a Refs,
    /// 0-based logical row; `row()` reports it as the 1-based number a spreadsheet
    /// would show, because that is what people type (`row() = 1` for the first row).
    pub row: u64,
}

/// Turn `B` into the second column's name, using the sheet's column order.
fn column_from_letters(letters: &str, columns: &[String]) -> Option<String> {
    let mut index: usize = 0;
    for ch in letters.chars() {
        let digit = (ch as u8).checked_sub(b'A')? as usize;
        index = index.checked_mul(26)?.checked_add(digit + 1)?;
    }
    columns.get(index.checked_sub(1)?).cloned()
}

pub(crate) fn eval(expr: &Expr, ctx: &Ctx<'_>) -> Value {
    match expr {
        Expr::Number(n) => Value::Number(*n),
        Expr::Text(t) => Value::Text(t.clone()),
        Expr::Column(name) => match ctx.cells.get(name) {
            None => Value::Error,
            Some(raw) => match raw.trim().parse::<f64>() {
                Ok(n) if n.is_finite() => Value::Number(n),
                _ if raw.trim().is_empty() => Value::Number(0.0),
                _ => Value::Text(raw.clone()),
            },
        },
        Expr::CellRef { column, row } => {
            // A column formula behaves like a spreadsheet formula filled down: the
            // reference is *relative* to the row it was written on, and that row is
            // the first data row (row 2 in a sheet whose row 1 is the header).
            // So `B2` means "column B of this row" on every row, and `B3` would need
            // the next row, which a forward streaming pass cannot see.
            let offset = *row as i64 - FIRST_DATA_ROW;
            if offset != 0 {
                return Value::Error;
            }
            match column_from_letters(column, ctx.columns) {
                Some(name) => match ctx.cells.get(&name) {
                    None => Value::Error,
                    Some(raw) => match raw.trim().parse::<f64>() {
                        Ok(n) if n.is_finite() => Value::Number(n),
                        _ if raw.trim().is_empty() => Value::Number(0.0),
                        _ => Value::Text(raw.clone()),
                    },
                },
                None => Value::Error,
            }
        }
        Expr::Negate(x) => match eval(x, ctx) {
            Value::Number(n) => Value::Number(-n),
            _ => Value::Error,
        },
        Expr::Not(x) => Value::Bool(!truthy(&eval(x, ctx))),
        Expr::Binary(op, l, r) => {
            let a = eval(l, ctx);
            let b = eval(r, ctx);
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
            let a = eval(l, ctx);
            let b = eval(r, ctx);
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
        Expr::Call(name, args) => functions::call(name, args, ctx),
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
    fn row_numbers_are_one_based_like_a_spreadsheet() {
        let formula = Formula::parse("row()").unwrap();
        let row = row(&[]);
        assert_eq!(formula.eval_with(&row, &Refs::default()).render(), "1");
        assert_eq!(
            formula.eval_at(&row, &[], &Refs::default(), 41).render(),
            "42",
            "0-based internally, 1-based to the user"
        );
    }

    #[test]
    fn a_sequence_column_can_be_built_from_row() {
        // `seq` = the row number; `odd` = parity flag. Both are per-row values with
        // no per-row storage.
        let seq = Formula::parse("[id] & \"-\" & row()").unwrap();
        let odd = Formula::parse("if(row() % 2 = 1, \"odd\", \"even\")").unwrap();
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "A".to_string());
        assert_eq!(
            seq.eval_at(&cells, &[], &Refs::default(), 0).render(),
            "A-1"
        );
        assert_eq!(
            odd.eval_at(&cells, &[], &Refs::default(), 0).render(),
            "odd"
        );
        assert_eq!(
            odd.eval_at(&cells, &[], &Refs::default(), 1).render(),
            "even"
        );
    }

    #[test]
    fn a1_references_read_the_current_row() {
        // Columns: id, amount, qty. `= B2 * C2` on row 2 uses amount × qty.
        let columns = vec!["id".to_string(), "amount".to_string(), "qty".to_string()];
        let formula = Formula::parse("B2 * C2").unwrap();
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "7".to_string());
        cells.insert("amount".to_string(), "10".to_string());
        cells.insert("qty".to_string(), "3".to_string());
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "30"
        );

        // Filled down, `B2 * C2` stays "this row's B and C" on every row.
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 2)
                .render(),
            "30",
            "references are relative to the first data row, like a filled-down formula"
        );

        // A reference that needs another row cannot be served by a streaming pass.
        let next_row = Formula::parse("B3").unwrap();
        assert_eq!(
            next_row
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!",
            "a reference to the next row must fail loudly, not read this row"
        );
    }

    #[test]
    fn references_work_by_letter_across_multi_letter_columns() {
        // AA is the 27th column.
        let mut columns: Vec<String> = (0..27).map(|i| format!("c{i}")).collect();
        columns[26] = "target".to_string();
        let mut cells = HashMap::new();
        for column in &columns {
            cells.insert(column.clone(), "1".to_string());
        }
        cells.insert("target".to_string(), "42".to_string());
        let formula = Formula::parse("AA2").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 0)
                .render(),
            "42"
        );

        // A reference beyond the last column is an error, not a panic.
        let formula = Formula::parse("ZZ2").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 0)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn a_column_named_like_a_cell_can_still_be_used_with_brackets() {
        let mut cells = HashMap::new();
        cells.insert("A1".to_string(), "5".to_string());
        cells.insert("other".to_string(), "1".to_string());
        let formula = Formula::parse("[A1] + other").unwrap();
        assert_eq!(formula.eval(&cells).render(), "6");
    }

    #[test]
    fn numbers_render_without_float_noise() {
        assert_eq!(eval_str("0.1 + 0.2", &[]), "0.3");
        assert_eq!(eval_str("10 / 4", &[]), "2.5");
        assert_eq!(eval_str("100 / 3", &[]), "33.333333");
    }
}
