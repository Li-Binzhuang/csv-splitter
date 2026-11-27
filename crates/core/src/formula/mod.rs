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
//! function:= round(x[, digits]) | roundup(x[, digits]) | rounddown(x[, digits]) | abs(x)
//!          | min(...) | max(...) | min(range) | max(range) | stdev(col) | var(col) | stdevp(col) | varp(col) | countblank(col) | sign(x) | power(x, y) | exp(x) | ln(x) | log(x[, base])
//!          | len(x) | upper(x) | lower(x) | proper(x) | trim(x) | left(x, n) | right(x, n)
//!          | mid(x, start[, len]) | replace(x, old, new) | substitute(x, old, new)
//!          | find(needle, x[, start]) | search(needle, x[, start]) | rept(x, n)
//!          | concat(...) | concatenate(...) | textjoin(sep, ignore_empty, ...) | char(n) | code(text)
//!          | value(x) | exact(a, b)
//!          | year(x) | month(x) | day(x) | date(y, m, d) | datestring(x)
//!          | edate(date, months) | eomonth(date, months) | weekday(date) | weeknum(date) | networkdays(start, end) | workday(start, days) | days(end, start)
//!          | if(cond, a[, b]) | and(...) | or(...) | not(x) | iferror(x, fallback)
//!          | isblank(x) | isnumber(x) | row | lag(col[, n]) | running_total(col)
//!          | sum(col) | average(col) | count(col)   (whole-column, constant per row)
//!          | countif(col, "criteria") | sumif(col, "criteria") | averageif(col, "criteria")
//!
//! `column := name | [name]` — bracket a name that contains spaces **or ends in digits**,
//! because `city2` bare is an A1-style cell reference (column CITY, row 2).
//! ```
//!
//! Cell values that are not numbers evaluate to `#VALUE!` in numeric positions,
//! exactly like a spreadsheet, instead of silently becoming zero.

mod functions;
mod sheetref;
mod state;
mod totals;

pub use state::{RowNeeds, RowState};
pub use totals::column_totals;

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
    /// The `:` in `B2:C2`.
    Colon,
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
    /// `B2:C2` — a same-row range. Only aggregates read it; a bare range is an error.
    Range {
        from: (String, u64),
        to: (String, u64),
    },
    /// A criteria literal (`">1000"`) compiled once at parse time, so `countif` does
    /// not re-parse the string on every row.
    Criteria {
        text: String,
        spec: crate::filter::FilterSpec,
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
    /// An empty cell. Distinct from `Text("")` on purpose: a spreadsheet shows an empty
    /// cell as nothing, while a text function applied to it yields "" and `len` yields 0.
    /// It still counts as 0 in arithmetic, which is what a spreadsheet does.
    Empty,
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
            Self::Empty => String::new(),
            Self::Error => "#VALUE!".to_string(),
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            // A spreadsheet reads an empty cell as 0 in arithmetic.
            Self::Empty => Some(0.0),
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

    /// True when the value is the same on every row: whole-column aggregates and
    /// literals only. No reference to the current row's cells, no `lag`/`running_total`,
    /// no A1 cell reference — so the export can write it once instead of per row.
    pub fn is_row_constant(&self) -> bool {
        fn constant(expr: &Expr) -> bool {
            match expr {
                Expr::Number(_) | Expr::Text(_) => true,
                Expr::Column(_) | Expr::CellRef { .. } | Expr::Range { .. } => false,
                Expr::Criteria { .. } => false,
                Expr::Binary(_, left, right) | Expr::Compare(_, left, right) => {
                    constant(left) && constant(right)
                }
                Expr::Negate(inner) | Expr::Not(inner) => constant(inner),
                Expr::Call(name, _) if is_aggregate(name) => true,
                // A conditional aggregate over a whole column is constant; over a
                // same-row range it depends on the row.
                Expr::Call(name, args) if is_filtered_aggregate(name) => {
                    matches!(args.first(), Some(Expr::Column(_)))
                }
                Expr::Call(name, args) if is_multi_filtered(name) => args
                    .iter()
                    .all(|a| matches!(a, Expr::Column(_) | Expr::Criteria { .. })),
                // These read something about the row even when every argument is a
                // literal: `row()` its number, `ref`/`vlookup` the row's key cell.
                Expr::Call(name, _)
                    if matches!(
                        name.to_ascii_lowercase().as_str(),
                        "ref" | "vlookup" | "xlookup" | "row"
                    ) =>
                {
                    false
                }
                Expr::Call(_, args) => args.iter().all(constant),
            }
        }
        constant(&self.expr)
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

/// The functions whose value is the same on every row of a column.
fn is_aggregate(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "sum"
            | "average"
            | "avg"
            | "count"
            | "min"
            | "max"
            | "stdev"
            | "var"
            | "stdevp"
            | "varp"
            | "countblank"
    )
}

/// Multi-criteria aggregates: `countifs`, `sumifs`.
fn is_multi_filtered(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "countifs" | "sumifs" | "averageifs" | "minifs" | "maxifs"
    )
}

/// Aggregates that count/sum only the cells matching a criteria string.
fn is_filtered_aggregate(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "countif" | "sumif" | "averageif"
    )
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
                    aggregates: Vec::new(),
                    filtered: Vec::new(),
                    sets: Vec::new(),
                });
            }
            for arg in args {
                collect_row_needs(arg, out);
            }
        }
        Expr::Call(name, args) if is_multi_filtered(name) => {
            let lower = name.to_ascii_lowercase();
            let op = match lower.as_str() {
                "sumifs" => crate::formula::state::SetOp::Sum,
                "averageifs" => crate::formula::state::SetOp::Average,
                "minifs" => crate::formula::state::SetOp::Min,
                "maxifs" => crate::formula::state::SetOp::Max,
                _ => crate::formula::state::SetOp::Count,
            };
            let (value, start) = if matches!(op, crate::formula::state::SetOp::Count) {
                (None, 0usize)
            } else {
                match args.first() {
                    Some(Expr::Column(column)) => (Some(column.clone()), 1usize),
                    _ => (None, 0usize),
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
                    i += 1;
                }
            }
            if !pairs.is_empty() {
                out.sets
                    .push(crate::formula::state::CriteriaSet { op, value, pairs });
            }
        }
        Expr::Call(name, args) if is_filtered_aggregate(name) => {
            if let (Some(Expr::Column(column)), Some(Expr::Criteria { text, .. })) =
                (args.first(), args.get(1))
            {
                let pair = (column.clone(), text.clone());
                if !out.filtered.contains(&pair) {
                    out.filtered.push(pair);
                }
            }
            for arg in args {
                collect_row_needs(arg, out);
            }
        }
        Expr::Call(name, args) if is_aggregate(name) => {
            if let Some(Expr::Column(column)) = args.first() {
                if !out.aggregates.contains(column) {
                    out.aggregates.push(column.clone());
                }
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
                    aggregates: Vec::new(),
                    filtered: Vec::new(),
                    sets: Vec::new(),
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
        Expr::Range { .. } | Expr::Criteria { .. } => {}
    }
}

fn collect_sheet_refs(expr: &Expr, out: &mut Vec<(String, String, String)>) {
    match expr {
        Expr::Call(name, args)
            if name.eq_ignore_ascii_case("ref")
                || name.eq_ignore_ascii_case("vlookup")
                || name.eq_ignore_ascii_case("xlookup") =>
        {
            if args.len() >= 3 {
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
        Expr::Range { .. } | Expr::Criteria { .. } => {}
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
        Expr::Number(_) | Expr::CellRef { .. } | Expr::Range { .. } | Expr::Criteria { .. } => {}
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
            Self::Colon => ":".into(),
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
            ':' => {
                chars.next();
                tokens.push(Token::Colon);
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
                    if ch.is_alphanumeric() || ch == '_' || ch == '.' {
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
                        Some(Token::RParen) => {
                            let mut args = args;
                            // `countif`'s criteria is a literal; compile it once here
                            // rather than on every row the formula is evaluated on.
                            if matches!(
                                name.to_ascii_lowercase().as_str(),
                                "countif" | "sumif" | "averageif"
                            ) {
                                if let Some(Expr::Text(text)) = args.get(1).cloned() {
                                    if let Some(spec) = criteria_value(&text) {
                                        args[1] = Expr::Criteria { text, spec };
                                    }
                                }
                            }
                            if matches!(
                                name.to_ascii_lowercase().as_str(),
                                "countifs" | "sumifs" | "averageifs" | "minifs" | "maxifs"
                            ) {
                                for arg in args.iter_mut() {
                                    if let Expr::Text(text) = arg {
                                        if let Some(spec) = criteria_value(text) {
                                            *arg = Expr::Criteria {
                                                text: text.clone(),
                                                spec,
                                            };
                                        }
                                    }
                                }
                            }
                            Ok(Expr::Call(name, args))
                        }
                        _ => Err("missing ')' after function arguments".to_string()),
                    }
                } else if let Some((column, row)) = split_cell_ref(&name) {
                    // `B2:C2` is a range; a lone `B2` stays a single-cell reference.
                    if matches!(self.peek(), Some(Token::Colon)) {
                        self.take();
                        let Some(Token::Ident(end)) = self.take() else {
                            return Err("a range needs a cell after ':'".to_string());
                        };
                        let Some((to_column, to_row)) = split_cell_ref(&end) else {
                            return Err(format!("'{end}' is not a cell reference"));
                        };
                        Ok(Expr::Range {
                            from: (column, row),
                            to: (to_column, to_row),
                        })
                    } else {
                        Ok(Expr::CellRef { column, row })
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

/// The row a column formula is "written on": the first data row, counting the
/// header as row 1 the way a spreadsheet does. References are relative to it.
pub(crate) const FIRST_DATA_ROW: i64 = 2;

/// Split `B2` into (`B`, 2); `None` when the identifier is not a cell reference.
///
/// The pattern is letters followed by digits, which is how spreadsheets write a
/// cell. A bare `A1`-looking *column name* is therefore read as a reference — the
/// only place this can surprise someone is a column literally named `A1`, and the
/// `[A1]` bracket form still reaches it.
/// Canonical key for a multi-criteria request, shared by the parser, the pre-pass and
/// the evaluator so a `countifs` call finds the numbers computed for it.
pub(crate) fn criteria_key(
    op: crate::formula::state::SetOp,
    value: Option<&str>,
    pairs: &[(String, String)],
) -> String {
    let mut key = String::new();
    key.push_str(op.tag());
    key.push('\u{3}');
    key.push_str(value.unwrap_or("-"));
    for (column, criteria) in pairs {
        key.push('\u{1}');
        key.push_str(column);
        key.push('\u{2}');
        key.push_str(criteria);
    }
    key
}

/// Compile a spreadsheet criteria literal (`">1000"`, `"<>10"`, `"CDMX"`) into a
/// `FilterSpec`. The column name is a placeholder: `matches` never reads it.
fn criteria_value(text: &str) -> Option<crate::filter::FilterSpec> {
    let trimmed = text.trim();
    if trimmed.contains('*') || trimmed.contains('?') {
        return Some(crate::filter::FilterSpec {
            column: "_".to_string(),
            op: crate::filter::Op::Wildcard,
            value: trimmed.to_string(),
        });
    }
    let expression = if let Some(rest) = trimmed.strip_prefix("<>") {
        format!("_!={rest}")
    } else if matches!(
        trimmed.chars().next(),
        Some('=') | Some('<') | Some('>') | Some('~')
    ) {
        format!("_{trimmed}")
    } else {
        format!("_={trimmed}")
    };
    crate::filter::FilterSpec::parse(&expression).ok()
}

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
                _ if raw.trim().is_empty() => Value::Empty,
                _ => Value::Text(raw.clone()),
            },
        },
        // A range is only meaningful as an aggregate argument; bare it is an error.
        Expr::Range { .. } => Value::Error,
        // Criteria are read by `countif`/`sumif`, never as a value.
        Expr::Criteria { .. } => Value::Error,
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
                        _ if raw.trim().is_empty() => Value::Empty,
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
        Value::Empty => false,
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
            eval_str("substitute(city, \"C\", \"X\")", &[("city", "CDMX")]),
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
    fn criteria_wildcards_match_like_a_spreadsheet() {
        let columns = vec!["city".to_string(), "note".to_string()];
        let mut cells = HashMap::new();
        cells.insert("city".to_string(), "CDMX".to_string());
        cells.insert("note".to_string(), "ready".to_string());
        let count = Formula::parse(r#"countif(A2:A2, "*CDM*")"#).unwrap();
        assert_eq!(
            count
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1"
        );
        let prefix = Formula::parse(r#"countif(A2:A2, "CD*")"#).unwrap();
        assert_eq!(
            prefix
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1"
        );
        // Case-insensitive, like a spreadsheet.
        let lower = Formula::parse(r#"countif(A2:A2, "*cdm*")"#).unwrap();
        assert_eq!(
            lower
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1"
        );
    }

    #[test]
    fn subtotal_rejects_an_all_empty_range() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), String::new());
        cells.insert("b".to_string(), String::new());
        let formula = Formula::parse("subtotal(9, A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn subtotal_supports_the_excel_function_numbers() {
        let columns = vec![
            "id".to_string(),
            "amount".to_string(),
            "qty".to_string(),
            "note".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("id", "7"),
            ("amount", "10"),
            ("qty", "3"),
            ("note", "plain"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (code, expected) in [
            (1, "6.5"),
            (2, "2"),
            (3, "3"),
            (4, "10"),
            (5, "3"),
            (6, "30"),
            (7, "4.949747"),
            (8, "3.5"),
            (9, "13"),
            (10, "24.5"),
            (11, "12.25"),
            (101, "6.5"),
            (111, "12.25"),
        ] {
            let formula = Formula::parse(&format!("subtotal({code}, B2:D2)")).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "code={code}"
            );
        }
        for source in [
            "subtotal(0, B2:D2)",
            "subtotal(12, B2:D2)",
            "subtotal(100, B2:D2)",
            "subtotal(9, B3:D3)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn aggregate_covers_subtotal_and_rank_families() {
        let columns = vec![
            "id".to_string(),
            "amount".to_string(),
            "qty".to_string(),
            "note".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("id", "7"),
            ("amount", "10"),
            ("qty", "3"),
            ("note", "plain"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("aggregate(1, 0, B2:D2)", "6.5"),
            ("aggregate(2, 0, B2:D2)", "2"),
            ("aggregate(3, 0, B2:D2)", "3"),
            ("aggregate(4, 0, B2:D2)", "10"),
            ("aggregate(9, 0, B2:D2)", "13"),
            ("aggregate(9, 4, B2:D2)", "13"),
            ("aggregate(9, 6, B2:D2)", "13"),
            ("aggregate(14, 0, B2:D2, 2)", "3"),
            ("aggregate(15, 0, B2:D2, 1)", "3"),
            ("aggregate(16, 0, B2:D2, 0.5)", "6.5"),
            ("aggregate(17, 0, B2:D2, 2)", "6.5"),
            ("aggregate(18, 0, B2:D2, 0.5)", "6.5"),
            ("aggregate(19, 0, B2:D2, 2)", "6.5"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in [
            "aggregate(12, 0, B2:D2)",
            "aggregate(13, 0, B2:D2)",
            "aggregate(20, 0, B2:D2)",
            "aggregate(1, 5, B2:D2)",
            "aggregate(14, 0, B2:D2)",
            "aggregate(14, 0, B2:D2, 0)",
            "aggregate(14, 0, B2:D2, 3)",
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
    }

    #[test]
    fn intl_workday_functions_work() {
        for (source, expected) in [
            (
                r#"networkdays.intl("2024-01-01", "2024-01-07", "0000011")"#,
                "5",
            ),
            (
                r#"networkdays.intl("2024-01-01", "2024-01-07", "0000001")"#,
                "6",
            ),
            (r#"networkdays.intl("2024-01-01", "2024-01-07", 11)"#, "6"),
            (r#"networkdays.intl("2024-01-01", "2024-01-07", 2)"#, "5"),
            (
                r#"networkdays.intl("2024-01-01", "2024-01-07", "0000011", "2024-01-03")"#,
                "4",
            ),
            (r#"workday.intl("2024-01-01", 1, "0000001")"#, "2024-01-02"),
            (r#"workday.intl("2024-01-05", 1, "0000010")"#, "2024-01-07"),
            (r#"workday.intl("2024-01-07", 1, "0000001")"#, "2024-01-08"),
            (
                r#"workday.intl("2024-01-01", 2, "0000011", "2024-01-03")"#,
                "2024-01-04",
            ),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"networkdays.intl("2024-01-01", "2024-01-07", "000000")"#,
            r#"networkdays.intl("2024-01-01", "2024-01-07", "1111111")"#,
            r#"networkdays.intl("2024-01-01", "2024-01-07", 99)"#,
            r#"workday.intl("2024-01-01", 1, "1111111")"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn avedev_measures_mean_absolute_deviation() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4"), ("e", "5")] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("avedev(A2:E2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1.2"
        );
        let formula = Formula::parse("avedev(A2:A2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0"
        );
        let columns = vec!["a".to_string(), "b".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), String::new());
        cells.insert("b".to_string(), String::new());
        let formula = Formula::parse("avedev(A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        assert_eq!(eval_str("avedev(1, 2)", &[]), "#VALUE!");
    }

    #[test]
    fn forecast_predicts_from_a_line() {
        let columns = vec![
            "y1".to_string(),
            "y2".to_string(),
            "y3".to_string(),
            "x1".to_string(),
            "x2".to_string(),
            "x3".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("y1", "2"),
            ("y2", "4"),
            ("y3", "6"),
            ("x1", "1"),
            ("x2", "2"),
            ("x3", "3"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("forecast(4, A2:C2, D2:F2)", "8"),
            ("forecast.linear(0, A2:C2, D2:F2)", "0"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["forecast(4, A2:C2, D2:E2)", "forecast(4, A2:C2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
        for name in ["x1", "x2", "x3"] {
            cells.insert(name.to_string(), "1".to_string());
        }
        let formula = Formula::parse("forecast(4, A2:C2, D2:F2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn covariance_functions_work() {
        let columns = vec![
            "y1".to_string(),
            "y2".to_string(),
            "y3".to_string(),
            "x1".to_string(),
            "x2".to_string(),
            "x3".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("y1", "1"),
            ("y2", "2"),
            ("y3", "3"),
            ("x1", "2"),
            ("x2", "4"),
            ("x3", "6"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("covar(A2:C2, D2:F2)", "1.333333"),
            ("covariance.p(A2:C2, D2:F2)", "1.333333"),
            ("covariance.s(A2:C2, D2:F2)", "2"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["covar(A2:C2, D2:E2)", "covariance.s(A2:A2, D2:D2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
    }

    #[test]
    fn devsq_sums_squared_deviations() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4"), ("e", "5")] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("devsq(A2:E2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "10"
        );
        let formula = Formula::parse("devsq(A2:A2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0"
        );
        let columns = vec!["a".to_string(), "b".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), String::new());
        cells.insert("b".to_string(), String::new());
        let formula = Formula::parse("devsq(A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        assert_eq!(eval_str("devsq(1, 2)", &[]), "#VALUE!");
    }

    #[test]
    fn pearson_and_rsq_measure_correlation() {
        let columns = vec![
            "y1".to_string(),
            "y2".to_string(),
            "y3".to_string(),
            "x1".to_string(),
            "x2".to_string(),
            "x3".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("y1", "1"),
            ("y2", "2"),
            ("y3", "3"),
            ("x1", "1"),
            ("x2", "2"),
            ("x3", "3"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [("pearson(A2:C2, D2:F2)", "1"), ("rsq(A2:C2, D2:F2)", "1")] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        cells.insert("y1".to_string(), "3".to_string());
        cells.insert("y3".to_string(), "1".to_string());
        let formula = Formula::parse("pearson(A2:C2, D2:F2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "-1"
        );
        for source in ["pearson(A2:C2, D2:E2)", "rsq(A2:C2, D2:E2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
        for name in ["x1", "x2", "x3"] {
            cells.insert(name.to_string(), "1".to_string());
        }
        let formula = Formula::parse("pearson(A2:C2, D2:F2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn slope_and_intercept_fit_a_line() {
        let columns = vec![
            "y1".to_string(),
            "y2".to_string(),
            "y3".to_string(),
            "x1".to_string(),
            "x2".to_string(),
            "x3".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("y1", "2"),
            ("y2", "4"),
            ("y3", "6"),
            ("x1", "1"),
            ("x2", "2"),
            ("x3", "3"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("slope(A2:C2, D2:F2)", "2"),
            ("intercept(A2:C2, D2:F2)", "0"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["slope(A2:C2, D2:E2)", "intercept(A2:C2, D2:E2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
        for name in ["x1", "x2", "x3"] {
            cells.insert(name.to_string(), "1".to_string());
        }
        let formula = Formula::parse("slope(A2:C2, D2:F2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn standardize_returns_z_scores() {
        assert_eq!(eval_str("standardize(90, 80, 10)", &[]), "1");
        assert_eq!(eval_str("standardize(70, 80, 10)", &[]), "-1");
        assert_eq!(eval_str("standardize(80, 80, 10)", &[]), "0");
        assert_eq!(eval_str("standardize(80, 80, 5)", &[]), "0");
        for source in [
            "standardize(90, 80, 0)",
            "standardize(90, 80, -1)",
            "standardize(90, 80)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn geometric_and_harmonic_means_work() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "4"), ("c", "16")] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("geomean(A2:C2)", "4"),
            ("harmean(A2:C2)", "2.285714"),
            ("geomean(A2:A2)", "1"),
            ("harmean(A2:A2)", "1"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        cells.insert("c".to_string(), "0".to_string());
        for source in ["geomean(A2:C2)", "harmean(A2:C2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
        cells.insert("c".to_string(), "-1".to_string());
        let formula = Formula::parse("geomean(A2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn skew_and_kurtosis_measure_distribution_shape() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4"), ("e", "5")] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [("skew(A2:E2)", "0"), ("kurt(A2:E2)", "-1.2")] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["skew(A2:B2)", "kurt(A2:C2)", "skew(A2:E2)"] {
            if source == "skew(A2:E2)" {
                continue;
            }
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
        for name in ["a", "b", "c", "d", "e"] {
            cells.insert(name.to_string(), "3".to_string());
        }
        let formula = Formula::parse("skew(A2:E2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn pduration_reports_growth_periods() {
        assert_eq!(eval_str("pduration(0.1, 100, 121)", &[]), "2");
        assert_eq!(eval_str("pduration(0.1, 100, 110)", &[]), "1");
        assert_eq!(eval_str("pduration(0.1, 100, 100)", &[]), "0");
        for source in [
            "pduration(-1, 100, 121)",
            "pduration(0.1, 0, 121)",
            "pduration(0.1, 100, 0)",
            "pduration(0, 100, 121)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
        assert_eq!(eval_str("pduration(0, 100, 100)", &[]), "0");
    }

    #[test]
    fn vdb_varies_declining_balance_depreciation() {
        assert_eq!(eval_str("vdb(2400, 300, 10, 0, 1)", &[]), "480");
        assert_eq!(eval_str("vdb(2400, 300, 10, 1, 2)", &[]), "384");
        assert_eq!(eval_str("vdb(2400, 300, 10, 0, 2)", &[]), "864");
        assert_eq!(eval_str("vdb(2400, 300, 10, 0, 1, 2, true())", &[]), "480");
        for source in [
            "vdb(2400, 300, 10, 1, 1)",
            "vdb(2400, 300, 10, 0, 11)",
            "vdb(2400, 300, 10, 0, 1, 0)",
            "vdb(2400, 300, 10, 0.5, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn cumulative_interest_and_principal_work() {
        assert_eq!(
            eval_str("cumipmt(0.1, 2, 1000, 1, 2, 0)", &[]),
            "-152.380952"
        );
        assert_eq!(eval_str("cumprinc(0.1, 2, 1000, 1, 2, 0)", &[]), "-1000");
        assert_eq!(eval_str("cumipmt(0, 10, 1000, 1, 10, 0)", &[]), "0");
        assert_eq!(eval_str("cumprinc(0, 10, 1000, 1, 10, 0)", &[]), "-1000");
        assert_eq!(
            eval_str("cumipmt(0.1, 2, 1000, 1, 2, 1)", &[]),
            "-47.619048"
        );
        assert_eq!(eval_str("cumprinc(0.1, 2, 1000, 1, 2, 1)", &[]), "-1000");
        for source in [
            "cumipmt(0.1, 2, 1000, 0, 2, 0)",
            "cumipmt(0.1, 2, 1000, 1, 3, 0)",
            "cumipmt(0.1, 2, 1000, 2, 1, 0)",
            "cumprinc(0.1, 2, 1000, 1, 2, 2)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn fvschedule_compounds_a_rate_schedule() {
        let columns = vec!["r1".to_string(), "r2".to_string()];
        let mut cells = HashMap::new();
        cells.insert("r1".to_string(), "0.1".to_string());
        cells.insert("r2".to_string(), "0.2".to_string());
        for (source, expected) in [
            ("fvschedule(100, A2:B2)", "132"),
            ("fvschedule(100, A2:A2)", "110"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        cells.insert("r1".to_string(), "-1".to_string());
        let formula = Formula::parse("fvschedule(100, A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        cells.insert("r1".to_string(), "x".to_string());
        let formula = Formula::parse("fvschedule(100, A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        assert_eq!(eval_str("fvschedule(100)", &[]), "#VALUE!");
    }

    #[test]
    fn xirr_solves_date_based_cash_flows() {
        let columns = vec![
            "cash1".to_string(),
            "cash2".to_string(),
            "date1".to_string(),
            "date2".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("cash1", "-100"),
            ("cash2", "110"),
            ("date1", "2023-01-01"),
            ("date2", "2024-01-01"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("xirr(A2:B2, C2:D2)", "0.1"),
            ("xirr(A2:B2, C2:D2, 0.2)", "0.1"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["xirr(A2:B2, C2:D2, -1)", "xirr(A2:B2, C2:C2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
        cells.insert("cash2".to_string(), "20".to_string());
        let formula = Formula::parse("xirr(A2:B2, C2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!",
            "all-positive"
        );
    }

    #[test]
    fn ipmt_and_ppmt_split_payments() {
        assert_eq!(eval_str("ipmt(0, 1, 10, 1000)", &[]), "0");
        assert_eq!(eval_str("ppmt(0, 1, 10, 1000)", &[]), "-100");
        assert_eq!(eval_str("ipmt(0.1, 1, 2, 1000)", &[]), "-100");
        assert_eq!(eval_str("ppmt(0.1, 1, 2, 1000)", &[]), "-476.190476");
        assert_eq!(eval_str("ipmt(0.1, 2, 2, 1000)", &[]), "-52.380952");
        assert_eq!(eval_str("ppmt(0.1, 2, 2, 1000)", &[]), "-523.809524");
        assert_eq!(eval_str("ipmt(0.1, 1, 2, 1000, 0, 1)", &[]), "0");
        assert_eq!(eval_str("ppmt(0.1, 1, 2, 1000, 0, 1)", &[]), "-523.809524");
        assert_eq!(eval_str("ipmt(0.1, 2, 2, 1000, 0, 1)", &[]), "-47.619048");
        for source in [
            "ipmt(0.1, 0, 2, 1000)",
            "ipmt(0.1, 3, 2, 1000)",
            "ipmt(0.1, 1, 2, 1000, 0, 2)",
            "ppmt(0.1, 1, 0, 1000)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn rri_reports_equivalent_growth_rate() {
        assert_eq!(eval_str("rri(10, 1000, 2000)", &[]), "0.071773");
        assert_eq!(eval_str("rri(1, 100, 110)", &[]), "0.1");
        assert_eq!(eval_str("rri(2, 100, 121)", &[]), "0.1");
        for source in ["rri(0, 100, 110)", "rri(10, 0, 110)", "rri(10, 100, -110)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn xnpv_discounts_by_calendar_dates() {
        let columns = vec![
            "cash1".to_string(),
            "cash2".to_string(),
            "date1".to_string(),
            "date2".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("cash1", "-100"),
            ("cash2", "110"),
            ("date1", "2023-01-01"),
            ("date2", "2024-01-01"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("xnvp(0, A2:B2, C2:D2)", "10"),
            ("xnvp(0.1, A2:B2, C2:D2)", "0"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["xnvp(-1, A2:B2, D2:E2)", "xnvp(0.1, A2:B2, D2:D2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
        cells.insert("date1".to_string(), "not-a-date".to_string());
        let formula = Formula::parse("xnvp(0.1, A2:B2, C2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn declining_balance_depreciation_works() {
        assert_eq!(eval_str("db(1000, 100, 5, 1)", &[]), "369");
        assert_eq!(eval_str("db(1000, 100, 5, 2)", &[]), "232.839");
        assert_eq!(
            eval_str("db(1000000, 100000, 6, 1, 7)", &[]),
            "186083.333333"
        );
        assert_eq!(
            eval_str("db(1000000, 100000, 6, 2, 7)", &[]),
            "259639.416667"
        );
        assert_eq!(eval_str("ddb(2400, 300, 10, 1)", &[]), "480");
        assert_eq!(eval_str("ddb(2400, 300, 10, 2)", &[]), "384");
        assert_eq!(eval_str("ddb(2400, 300, 10, 3)", &[]), "307.2");
        for source in [
            "db(1000, 100, 5, 0)",
            "db(1000, 100, 5, 6)",
            "db(1000, 100, 5, 1, 13)",
            "ddb(1000, 100, 5, 0)",
            "ddb(1000, 100, 5, 6)",
            "ddb(1000, 100, 5, 1, 0)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn straight_line_and_sum_of_years_depreciation() {
        assert_eq!(eval_str("sln(10000, 1000, 5)", &[]), "1800");
        assert_eq!(eval_str("sln(1000, 0, 4)", &[]), "250");
        assert_eq!(eval_str("syd(10000, 1000, 5, 1)", &[]), "3000");
        assert_eq!(eval_str("syd(10000, 1000, 5, 2)", &[]), "2400");
        assert_eq!(eval_str("syd(10000, 1000, 5, 5)", &[]), "600");
        for source in [
            "sln(1000, 0, 0)",
            "sln(1000, 0, -1)",
            "syd(1000, 0, 0, 1)",
            "syd(1000, 0, 5, 0)",
            "syd(1000, 0, 5, 6)",
            "syd(1000, 0, 5.5, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn effect_and_nominal_convert_rates() {
        assert_eq!(eval_str("effect(0.12, 12)", &[]), "0.126825");
        assert_eq!(eval_str("effect(0.12, 4)", &[]), "0.125509");
        assert_eq!(eval_str("effect(0.12, 1)", &[]), "0.12");
        assert_eq!(eval_str("effect(0, 1)", &[]), "0");
        assert_eq!(eval_str("nominal(0.126824, 12)", &[]), "0.119999");
        assert_eq!(eval_str("nominal(0.12, 1)", &[]), "0.12");
        assert_eq!(eval_str("nominal(0, 1)", &[]), "0");
        for source in [
            "effect(0.12, 0)",
            "effect(0.12, 1.5)",
            "effect(-0.1, 12)",
            "nominal(-0.1, 12)",
            "nominal(0.12)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn irr_solves_periodic_cash_flows() {
        let columns = vec!["cash1".to_string(), "cash2".to_string()];
        let mut cells = HashMap::new();
        cells.insert("cash1".to_string(), "-100".to_string());
        cells.insert("cash2".to_string(), "110".to_string());
        let formula = Formula::parse("irr(A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0.1"
        );
        let columns = vec![
            "cash1".to_string(),
            "cash2".to_string(),
            "cash3".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("cash1".to_string(), "-100".to_string());
        cells.insert("cash2".to_string(), "0".to_string());
        cells.insert("cash3".to_string(), "121".to_string());
        let formula = Formula::parse("irr(A2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0.1"
        );
        let columns = vec!["cash1".to_string(), "cash2".to_string()];
        let mut cells = HashMap::new();
        cells.insert("cash1".to_string(), "10".to_string());
        cells.insert("cash2".to_string(), "20".to_string());
        for source in ["irr(A2:B2)", "irr(A2:B2, -1)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
    }

    #[test]
    fn rate_solves_the_annuity_equation() {
        assert_eq!(eval_str("rate(10, -100, 1000)", &[]), "0");
        assert_eq!(eval_str("rate(1, -1100, 1000)", &[]), "0.1");
        assert_eq!(eval_str("rate(1, -1100, 1000, 0, 0, 0.2)", &[]), "0.1");
        for source in [
            "rate(0, -100, 1000)",
            "rate(1, -1100, 1000, 0, 2)",
            "rate(1, -1100, 1000, 0, 0, -1)",
            "rate(1, -1100)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn npv_discounts_a_cash_flow_series() {
        assert_eq!(eval_str("npv(0, -100, 200)", &[]), "100");
        assert_eq!(eval_str("npv(0, 100)", &[]), "100");
        assert_eq!(eval_str("npv(0.1, 110, -121)", &[]), "0");
        assert_eq!(eval_str("npv(0.1, 0, 121)", &[]), "100");
        assert_eq!(eval_str("npv(0.1, 100)", &[]), "90.909091");
        let columns = vec!["amount".to_string(), "qty".to_string()];
        let mut cells = HashMap::new();
        cells.insert("amount".to_string(), "10".to_string());
        cells.insert("qty".to_string(), "3".to_string());
        let formula = Formula::parse("npv(0, A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "13"
        );
        for source in ["npv(-1, 100)", "npv(0, \"x\")"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn nper_reports_the_number_of_periods() {
        assert_eq!(eval_str("nper(0, -100, 1000)", &[]), "10");
        assert_eq!(eval_str("nper(0, -150, 1000, 500)", &[]), "10");
        assert_eq!(eval_str("nper(0.1, -1100, 1000)", &[]), "1");
        assert_eq!(eval_str("nper(0.1, 0, 1000, -1210)", &[]), "2");
        assert_eq!(eval_str("nper(0.1, -162.745447, 1000)", &[]), "9.999995");
        for source in [
            "nper(0, 0, 1000)",
            "nper(-1, -100, 1000)",
            "nper(0.1, -100, 1000, 0, 2)",
            "nper(0.1, -100)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn pv_discounts_future_cash_flows() {
        assert_eq!(eval_str("pv(0, 10, 100)", &[]), "-1000");
        assert_eq!(eval_str("pv(0, 10, 100, 500)", &[]), "-1500");
        assert_eq!(eval_str("pv(0.1, 1, -1100)", &[]), "1000");
        assert_eq!(eval_str("pv(0.1, 1, -1100, 0, 1)", &[]), "1100");
        assert_eq!(eval_str("pv(0.1, 2, -576.190476)", &[]), "1000");
        assert_eq!(eval_str("pv(0.1, 2, 0, -1210)", &[]), "1000");
        for source in ["pv(-1, 1, 100)", "pv(0.1, 1, 100, 0, 2)", "pv(0.1, 1)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn fv_projects_future_value() {
        assert_eq!(eval_str("fv(0, 10, -100, 1000)", &[]), "0");
        assert_eq!(eval_str("fv(0, 10, -100)", &[]), "1000");
        assert_eq!(eval_str("fv(0.1, 1, 0, 1000)", &[]), "-1100");
        assert_eq!(eval_str("fv(0.1, 2, 0, 1000)", &[]), "-1210");
        assert_eq!(eval_str("fv(0.1, 1, -100, 1000)", &[]), "-1000");
        assert_eq!(eval_str("fv(0.1, 1, -100, 1000, 1)", &[]), "-990");
        for source in ["fv(-1, 1, 0, 1000)", "fv(0.1, 1, 0, 1000, 2)", "fv(0.1, 1)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn pmt_calculates_loan_payments() {
        assert_eq!(eval_str("pmt(0, 10, 1000)", &[]), "-100");
        assert_eq!(eval_str("pmt(0, 10, 1000, 500)", &[]), "-150");
        assert_eq!(eval_str("pmt(0, 10, 1000, 0, 1)", &[]), "-100");
        assert_eq!(eval_str("pmt(0.1, 1, 1000)", &[]), "-1100");
        assert_eq!(eval_str("pmt(0.1, 2, 1000)", &[]), "-576.190476");
        assert_eq!(eval_str("pmt(0.1, 1, 1000, 0, 1)", &[]), "-1000");
        for source in [
            "pmt(0.1, 0, 1000)",
            "pmt(-1, 1, 1000)",
            "pmt(0.1, 1, 1000, 0, 2)",
            "pmt(0.1, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn decimal_and_base_convert_radices() {
        assert_eq!(eval_str(r#"decimal("FF", 16)"#, &[]), "255");
        assert_eq!(eval_str(r#"decimal("ff", 16)"#, &[]), "255");
        assert_eq!(eval_str(r#"decimal("1010", 2)"#, &[]), "10");
        assert_eq!(eval_str(r#"decimal("zz", 36)"#, &[]), "1295");
        assert_eq!(eval_str(r#"decimal("-FF", 16)"#, &[]), "-255");
        assert_eq!(eval_str(r#"decimal("", 2)"#, &[]), "0");
        for source in [
            r#"decimal("2", 2)"#,
            r#"decimal("FF", 1)"#,
            r#"decimal("FF", 37)"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
        assert_eq!(eval_str("base(255, 16)", &[]), "FF");
        assert_eq!(eval_str("base(255, 16, 4)", &[]), "00FF");
        assert_eq!(eval_str("base(10, 2)", &[]), "1010");
        assert_eq!(eval_str("base(0, 2)", &[]), "0");
        assert_eq!(eval_str("base(0, 2, 3)", &[]), "000");
        for source in [
            "base(-1, 2)",
            "base(1.5, 2)",
            "base(10, 1)",
            "base(10, 37)",
            "base(10, 2, 256)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn error_type_maps_excel_codes() {
        for (source, expected) in [
            (r##"error.type("#NULL!")"##, "1"),
            (r##"error.type("#DIV/0!")"##, "2"),
            (r##"error.type("#VALUE!")"##, "3"),
            (r##"error.type("#REF!")"##, "4"),
            (r##"error.type("#NAME?")"##, "5"),
            (r##"error.type("#NUM!")"##, "6"),
            (r##"error.type("#N/A")"##, "7"),
            ("error.type(na())", "7"),
            ("error.type(1/0)", "3"),
            ("error.type(1)", "#N/A"),
            (r#"error.type("x")"#, "#N/A"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        assert_eq!(eval_str("error.type()", &[]), "#VALUE!");
        assert_eq!(eval_str("error.type(1, 2)", &[]), "#VALUE!");
    }

    #[test]
    fn engineering_rounding_works() {
        assert_eq!(eval_str("mround(10, 3)", &[]), "9");
        assert_eq!(eval_str("mround(11, 3)", &[]), "12");
        assert_eq!(eval_str("mround(-10, -3)", &[]), "-9");
        assert_eq!(eval_str("mround(-11, -3)", &[]), "-12");
        assert_eq!(eval_str("mround(10, -3)", &[]), "#VALUE!");
        assert_eq!(eval_str("mround(10, 0)", &[]), "#VALUE!");
        assert_eq!(eval_str("ceiling.precise(4.3, 2)", &[]), "6");
        assert_eq!(eval_str("ceiling.precise(-4.3, 2)", &[]), "-4");
        assert_eq!(eval_str("ceiling.precise(4.3, -2)", &[]), "6");
        assert_eq!(eval_str("ceiling.precise(5)", &[]), "5");
        assert_eq!(eval_str("floor.precise(4.3, 2)", &[]), "4");
        assert_eq!(eval_str("floor.precise(-4.3, 2)", &[]), "-6");
        assert_eq!(eval_str("floor.precise(-4.3, -2)", &[]), "-6");
        assert_eq!(eval_str("floor.precise(5)", &[]), "5");
        assert_eq!(eval_str("ceiling.precise(5, 0)", &[]), "0");
    }

    #[test]
    fn bitwise_functions_work() {
        assert_eq!(eval_str("bitand(5, 3)", &[]), "1");
        assert_eq!(eval_str("bitor(5, 3)", &[]), "7");
        assert_eq!(eval_str("bitxor(5, 3)", &[]), "6");
        assert_eq!(eval_str("bitand(0, 0)", &[]), "0");
        assert_eq!(
            eval_str("bitor(281474976710655, 0)", &[]),
            "281474976710655"
        );
        for source in [
            "bitand(-1, 3)",
            "bitand(2.5, 3)",
            "bitor(281474976710656, 0)",
            "bitxor(5)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn number_theory_functions_work() {
        assert_eq!(eval_str("gcd(12, 18)", &[]), "6");
        assert_eq!(eval_str("gcd(0, 5)", &[]), "5");
        assert_eq!(eval_str("gcd(12, 18, 24)", &[]), "6");
        assert_eq!(eval_str("gcd(-1, 5)", &[]), "#VALUE!");
        assert_eq!(eval_str("gcd(1.5, 2)", &[]), "#VALUE!");
        assert_eq!(eval_str("lcm(4, 6)", &[]), "12");
        assert_eq!(eval_str("lcm(4, 6, 8)", &[]), "24");
        assert_eq!(eval_str("lcm(0, 6)", &[]), "0");
        assert_eq!(eval_str("lcm(1.5, 6)", &[]), "#VALUE!");
        assert_eq!(eval_str("fact(5)", &[]), "120");
        assert_eq!(eval_str("fact(0)", &[]), "1");
        assert_eq!(eval_str("fact(171)", &[]), "#VALUE!");
        assert_eq!(eval_str("fact(2.5)", &[]), "#VALUE!");
        assert_eq!(eval_str("combin(5, 2)", &[]), "10");
        assert_eq!(eval_str("combin(5, 0)", &[]), "1");
        assert_eq!(eval_str("combin(2, 5)", &[]), "#VALUE!");
        assert_eq!(eval_str("permut(5, 2)", &[]), "20");
        assert_eq!(eval_str("permut(5, 0)", &[]), "1");
        assert_eq!(eval_str("permut(2, 5)", &[]), "#VALUE!");
    }

    #[test]
    fn fixed_dollar_and_numbervalue_work() {
        assert_eq!(eval_str("fixed(1234.5678)", &[]), "1,234.57");
        assert_eq!(eval_str("fixed(1234.5678, 0)", &[]), "1,235");
        assert_eq!(eval_str("fixed(1234.5678, 2, true())", &[]), "1234.57");
        assert_eq!(eval_str("fixed(-1234.5, 2)", &[]), "-1,234.50");
        assert_eq!(eval_str("fixed(1234.5678, -2)", &[]), "1,200");
        assert_eq!(eval_str("dollar(1234.5678)", &[]), "$1,234.57");
        assert_eq!(eval_str("dollar(-1234.5678)", &[]), "($1,234.57)");
        assert_eq!(eval_str("dollar(1234.5678, 0)", &[]), "$1,235");
        assert_eq!(eval_str("dollar(1234.5678, 4)", &[]), "$1,234.5678");
        for source in [
            "dollar(1234.5678, 5)",
            "dollar(1234.5678, -1)",
            "dollar(1234.5678, 1.5)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
        assert_eq!(
            eval_str(r#"numbervalue("1.234,56", ",", ".")"#, &[]),
            "1234.56"
        );
        assert_eq!(eval_str(r#"numbervalue("1,234.56")"#, &[]), "1234.56");
        assert_eq!(eval_str(r#"numbervalue("12%")"#, &[]), "0.12");
        assert_eq!(eval_str(r#"numbervalue("(123)")"#, &[]), "-123");
        assert_eq!(eval_str(r#"numbervalue("1.23.4")"#, &[]), "#VALUE!");
        for (source, expected) in [
            (r#"value("$1,234.56")"#, "1234.56"),
            (r#"value("12%")"#, "0.12"),
            (r#"value("(123)")"#, "-123"),
            (r#"value(" 42 ")"#, "42"),
            (r#"value("1,23")"#, "123"),
            (r#"value("€100")"#, "100"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"value("abc")"#, r#"value("")"#, r#"value("1", 2)"#] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
        assert_eq!(eval_str(r#"numbervalue("1,23", ",", ",")"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"numbervalue("")"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"numbervalue("  ")"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"numbervalue("1.2", "..", ",")"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"numbervalue("1.2", "", ",")"#, &[]), "#VALUE!");
    }

    #[test]
    fn textjoin_char_and_code_work_like_a_spreadsheet() {
        let columns = vec!["city".to_string(), "note".to_string()];
        let mut cells = HashMap::new();
        cells.insert("city".to_string(), "CDMX".to_string());
        cells.insert("note".to_string(), String::new());
        let joined = Formula::parse(r#"textjoin("-", 1, city, note)"#).unwrap();
        assert_eq!(
            joined
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "CDMX",
            "ignore_empty=1 drops the empty part"
        );
        let kept = Formula::parse(r#"textjoin("-", 0, city, note)"#).unwrap();
        assert_eq!(
            kept.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "CDMX-",
            "ignore_empty=0 keeps the separator"
        );
        cells.insert("note".to_string(), "ready".to_string());
        assert_eq!(
            joined
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "CDMX-ready"
        );

        for source in [
            r#"textjoin("-", 2, "a", "b")"#,
            r#"textjoin("-", 1.5, "a", "b")"#,
            r#"textjoin("-", "x", "a", "b")"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
        assert_eq!(eval_str("char(65)", &[]), "A");
        assert_eq!(eval_str("char(255)", &[]), "ÿ");
        for source in [
            "char(0)",
            "char(256)",
            "char(-1)",
            r#"char("x")"#,
            "char(65, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
        assert_eq!(eval_str("code(\"A\")", &[]), "65");
        assert_eq!(eval_str("code(\"\")", &[]), "#VALUE!");
        assert_eq!(eval_str("code(\"A\", 1)", &[]), "#VALUE!");
        assert_eq!(eval_str("unichar(20013)", &[]), "中");
        assert_eq!(eval_str("unicode(\"中\")", &[]), "20013");
        assert_eq!(eval_str("unichar(128512)", &[]), "😀");
        assert_eq!(eval_str("unicode(\"😀\")", &[]), "128512");
        assert_eq!(eval_str("unicode(\"\")", &[]), "#VALUE!");
        assert_eq!(eval_str("unicode(\"中\", 1)", &[]), "#VALUE!");
        assert_eq!(eval_str("unichar(0)", &[]), "#VALUE!");
        assert_eq!(eval_str("unichar(55296)", &[]), "#VALUE!");
        assert_eq!(eval_str("unicode(\"\")", &[]), "#VALUE!");
    }

    #[test]
    fn multi_criteria_aggregates_work_over_same_row_ranges() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
            "f".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "10"),
            ("b", "3"),
            ("c", "7"),
            ("d", "5"),
            ("e", "0"),
            ("f", "2"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let sum = Formula::parse(r#"sumifs(A2:C2, D2:F2, ">0")"#).unwrap();
        assert_eq!(
            sum.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "17",
            "10 + 7; the pair whose criterion is 0 is excluded"
        );
        let count = Formula::parse(r#"countifs(D2:F2, ">0")"#).unwrap();
        assert_eq!(
            count
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2"
        );
        let average = Formula::parse(r#"averageifs(A2:C2, D2:F2, ">0")"#).unwrap();
        assert_eq!(
            average
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "8.5"
        );
        let narrow = Formula::parse(r#"sumifs(A2:C2, D2:F2, ">0", D2:F2, "<3")"#).unwrap();
        assert_eq!(
            narrow
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "7",
            "only the position whose criterion cell is 2 passes both pairs"
        );
        let minimum = Formula::parse(r#"minifs(A2:C2, D2:F2, ">0")"#).unwrap();
        assert_eq!(
            minimum
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "7",
            "10 and 7 match; 3 is paired with a 0 criterion"
        );
        let maximum = Formula::parse(r#"maxifs(A2:C2, D2:F2, ">0")"#).unwrap();
        assert_eq!(
            maximum
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "10"
        );
        let mismatched = Formula::parse(r#"sumifs(A2:C2, D2:D2, ">0")"#).unwrap();
        assert_eq!(
            mismatched
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!",
            "ranges of different widths cannot be paired"
        );
    }

    #[test]
    fn xlookup_preloads_its_reference_with_or_without_a_fallback() {
        for text in [
            r#"xlookup("people.csv", "cust", "name")"#,
            r#"xlookup("people.csv", "cust", "name", "unknown")"#,
            r#"xlookup("people.csv", "cust", "name", "unknown", 2)"#,
            r#"xlookup("people.csv", "cust", "name", "unknown", 1)"#,
        ] {
            let formula = Formula::parse(text).unwrap();
            assert_eq!(
                formula.sheet_references(),
                vec![(
                    "people.csv".to_string(),
                    "cust".to_string(),
                    "name".to_string()
                )],
                "{text}"
            );
        }
    }

    #[test]
    fn vlookup_preloads_the_same_reference_as_ref() {
        for text in [
            r#"vlookup("people.csv", "cust", "name")"#,
            r#"vlookup("people.csv", "cust", "name", 2)"#,
            r#"vlookup("people.csv", "cust", "name", 1)"#,
        ] {
            let formula = Formula::parse(text).unwrap();
            assert_eq!(
                formula.sheet_references(),
                vec![(
                    "people.csv".to_string(),
                    "cust".to_string(),
                    "name".to_string()
                )],
                "{text}: the alias must feed the loader, not only the evaluator"
            );
        }
    }

    #[test]
    fn iserror_and_istext_report_types() {
        assert_eq!(eval_str("iserror(1/0)", &[]), "TRUE");
        assert_eq!(eval_str("iserror(1)", &[]), "FALSE");
        // ref/vlookup spell a miss as text, so iserror must own that spelling too.
        assert_eq!(eval_str("iserror(\"#N/A\")", &[]), "TRUE");
        assert_eq!(eval_str("isna(\"#N/A\")", &[]), "TRUE");
        assert_eq!(eval_str("isna(1/0)", &[]), "FALSE");
        assert_eq!(eval_str("isna(1)", &[]), "FALSE");
        assert_eq!(eval_str(r#"istext("A")"#, &[]), "TRUE");
        assert_eq!(eval_str("istext(1)", &[]), "FALSE");
        assert_eq!(eval_str("istext(1/0)", &[]), "FALSE");
    }

    #[test]
    fn weeknum_return_types_control_week_start() {
        for (source, expected) in [
            (r#"weeknum("2024-01-07")"#, "2"),
            (r#"weeknum("2024-01-07", 2)"#, "1"),
            (r#"weeknum("2024-01-07", 11)"#, "1"),
            (r#"weeknum("2024-01-02", 12)"#, "2"),
            (r#"weeknum("2024-01-03", 13)"#, "2"),
            (r#"weeknum("2024-01-04", 14)"#, "2"),
            (r#"weeknum("2024-01-05", 15)"#, "2"),
            (r#"weeknum("2024-01-06", 16)"#, "2"),
            (r#"weeknum("2024-01-07", 17)"#, "2"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"weeknum("2024-01-01", 0)"#,
            r#"weeknum("2024-01-01", 3)"#,
            r#"weeknum("2024-01-01", 2.5)"#,
            "weeknum(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn time_isoweeknum_and_yearfrac_work() {
        assert_eq!(eval_str("time(13, 4, 5)", &[]), "0.544502");
        assert_eq!(eval_str("time(24, 0, 0)", &[]), "#VALUE!");
        assert_eq!(eval_str(r#"isoweeknum("2024-01-01")"#, &[]), "1");
        assert_eq!(eval_str(r#"isoweeknum("2023-12-31")"#, &[]), "52");
        assert_eq!(
            eval_str(r#"yearfrac("2024-01-01", "2025-01-01", 0)"#, &[]),
            "1"
        );
        assert_eq!(
            eval_str(r#"yearfrac("2024-01-01", "2025-01-01", 1)"#, &[]),
            "1.002053"
        );
        assert_eq!(
            eval_str(r#"yearfrac("2024-01-01", "2025-01-01", 2)"#, &[]),
            "1.016667"
        );
        assert_eq!(
            eval_str(r#"yearfrac("2024-01-01", "2025-01-01", 3)"#, &[]),
            "1.00274"
        );
        assert_eq!(
            eval_str(r#"yearfrac("2024-01-01", "2025-01-01", 4)"#, &[]),
            "1"
        );
        assert_eq!(
            eval_str(r#"yearfrac("2024-01-02", "2024-01-01", 0)"#, &[]),
            "#VALUE!"
        );
    }

    #[test]
    fn datedif_supports_ym_md_and_yd() {
        assert_eq!(
            eval_str(r#"datedif("2020-01-15", "2024-02-15", "YM")"#, &[]),
            "1"
        );
        assert_eq!(
            eval_str(r#"datedif("2020-01-01", "2020-01-31", "MD")"#, &[]),
            "30"
        );
        assert_eq!(
            eval_str(r#"datedif("2020-01-01", "2021-01-01", "YD")"#, &[]),
            "0"
        );
        assert_eq!(
            eval_str(r#"datedif("2024-01-02", "2024-01-01", "MD")"#, &[]),
            "#VALUE!"
        );
    }

    #[test]
    fn iso_week_equivalence_and_reverse_ranges_work() {
        for date in ["2024-01-01", "2024-12-30", "2025-01-01"] {
            assert_eq!(
                eval_str(&format!(r#"weeknum("{date}", 21)"#), &[]),
                eval_str(&format!(r#"isoweeknum("{date}")"#), &[]),
                "{date}"
            );
        }
        assert_eq!(eval_str(r#"workday("2024-01-02", 0)"#, &[]), "2024-01-02");
        assert_eq!(
            eval_str(r#"networkdays("2024-01-05", "2024-01-01")"#, &[]),
            "#VALUE!"
        );
        assert_eq!(
            eval_str(
                r#"networkdays("2024-01-01", "2024-01-05", "2024-01-02", "2024-01-02")"#,
                &[]
            ),
            "4",
            "duplicate holidays count once"
        );
    }

    #[test]
    fn sunday_to_friday_and_zero_day_holiday_blank_work() {
        assert_eq!(
            eval_str(r#"networkdays("2024-01-07", "2024-01-12")"#, &[]),
            "5"
        );
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-03"),
            ("b", ""),
            ("c", ""),
            ("d", "2024-01-12"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("workday(A2, 0, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-03"
        );
        assert_eq!(eval_str(r#"days360("2024-03-24", "2024-03-31")"#, &[]), "7");
    }

    #[test]
    fn sunday_to_thursday_and_blank_zero_day_weekend_work() {
        assert_eq!(
            eval_str(
                r#"networkdays("2024-01-07", "2024-01-11", "2024-01-10")"#,
                &[]
            ),
            "3"
        );
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-06"),
            ("b", ""),
            ("c", ""),
            ("d", "2024-01-11"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("workday(A2, 0, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-06"
        );
        assert_eq!(eval_str(r#"days360("2024-03-25", "2024-03-31")"#, &[]), "6");
    }

    #[test]
    fn sunday_to_wednesday_and_zero_day_weekend_work() {
        assert_eq!(
            eval_str(r#"networkdays("2024-01-07", "2024-01-10")"#, &[]),
            "3"
        );
        assert_eq!(
            eval_str(r#"workday("2024-01-06", 0, "2024-01-06")"#, &[]),
            "2024-01-06"
        );
        assert_eq!(eval_str(r#"days360("2024-03-26", "2024-03-31")"#, &[]), "5");
    }

    #[test]
    fn tuesday_holiday_and_blank_zero_day_work() {
        assert_eq!(
            eval_str(
                r#"networkdays("2024-01-07", "2024-01-09", "2024-01-09")"#,
                &[]
            ),
            "1"
        );
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-06"),
            ("b", ""),
            ("c", ""),
            ("d", "2024-01-09"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("workday(A2, 0, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-06"
        );
        assert_eq!(eval_str(r#"days360("2024-03-27", "2024-03-31")"#, &[]), "4");
    }

    #[test]
    fn zero_day_weekend_holiday_and_tuesday_holiday_work() {
        assert_eq!(
            eval_str(r#"workday("2024-01-06", 0, "2024-01-06")"#, &[]),
            "2024-01-06"
        );
        assert_eq!(
            eval_str(
                r#"networkdays("2024-01-07", "2024-01-09", "2024-01-08")"#,
                &[]
            ),
            "1"
        );
        assert_eq!(eval_str(r#"days360("2024-03-28", "2024-03-31")"#, &[]), "3");
    }

    #[test]
    fn sunday_to_tuesday_and_zero_day_holiday_work() {
        assert_eq!(
            eval_str(r#"networkdays("2024-01-07", "2024-01-09")"#, &[]),
            "2"
        );
        assert_eq!(
            eval_str(r#"workday("2024-01-03", 0, "2024-01-03")"#, &[]),
            "2024-01-03"
        );
        assert_eq!(eval_str(r#"days360("2024-03-29", "2024-03-31")"#, &[]), "2");
    }

    #[test]
    fn sunday_to_monday_and_zero_day_edges_work() {
        assert_eq!(
            eval_str(r#"networkdays("2024-01-07", "2024-01-08")"#, &[]),
            "1"
        );
        assert_eq!(eval_str(r#"workday("2024-01-07", 0)"#, &[]), "2024-01-07");
        assert_eq!(eval_str(r#"days360("2024-03-30", "2024-03-31")"#, &[]), "0");
    }

    #[test]
    fn saturday_to_monday_and_zero_day_workdays_work() {
        assert_eq!(
            eval_str(r#"networkdays("2024-01-06", "2024-01-08")"#, &[]),
            "1"
        );
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-06"),
            ("b", ""),
            ("c", ""),
            ("d", "2024-01-08"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("workday(A2, 0, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-06"
        );
        assert_eq!(
            eval_str(r#"days360("2024-03-30", "2024-03-31", 1)"#, &[]),
            "0"
        );
    }

    #[test]
    fn friday_to_monday_and_blank_weekend_work() {
        assert_eq!(
            eval_str(r#"networkdays("2024-01-05", "2024-01-08")"#, &[]),
            "2",
            "Friday and Monday are both working days"
        );
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-06"),
            ("b", ""),
            ("c", ""),
            ("d", "2024-01-09"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("workday(A2, 2, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-09"
        );
        assert_eq!(
            eval_str(r#"days360("2024-03-01", "2024-03-31", 1)"#, &[]),
            "29"
        );
    }

    #[test]
    fn all_blank_holiday_ranges_and_friday_to_sunday_work() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-01"),
            ("b", ""),
            ("c", ""),
            ("d", "2024-01-05"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("workday(A2, 2, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-03"
        );
        assert_eq!(
            eval_str(r#"days360("2024-02-28", "2024-03-31", 1)"#, &[]),
            "32"
        );
        assert_eq!(
            eval_str(r#"networkdays("2024-01-05", "2024-01-07")"#, &[]),
            "1",
            "Friday to Sunday counts only Friday"
        );
    }

    #[test]
    fn two_day_weekends_and_half_open_february_work() {
        assert_eq!(
            eval_str(r#"networkdays("2024-01-06", "2024-01-07")"#, &[]),
            "0"
        );
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-06"),
            ("b", ""),
            ("c", "2024-01-09"),
            ("d", "2024-01-12"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("workday(A2, 2, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-10"
        );
        assert_eq!(
            eval_str(r#"days360("2024-02-01", "2024-02-28", 1)"#, &[]),
            "27"
        );
    }

    #[test]
    fn weekend_only_ranges_and_blank_holiday_ranges_work() {
        assert_eq!(
            eval_str(r#"networkdays("2024-01-07", "2024-01-07")"#, &[]),
            "0",
            "a Sunday-only range has no working days"
        );
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-01"),
            ("b", ""),
            ("c", "2024-01-03"),
            ("d", "2024-01-05"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("workday(A2, 2, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-04"
        );
        assert_eq!(
            eval_str(r#"days360("2024-02-01", "2024-02-29", 1)"#, &[]),
            "28"
        );
    }

    #[test]
    fn workday_range_duplicates_and_weekend_starts_work() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-01"),
            ("b", "2024-01-03"),
            ("c", "2024-01-03"),
            ("d", "2024-01-05"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("workday(A2, 2, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-04"
        );
        assert_eq!(
            eval_str(r#"networkdays("2024-01-06", "2024-01-08")"#, &[]),
            "1",
            "Saturday to Monday counts only Monday"
        );
        assert_eq!(
            eval_str(r#"days360("2024-02-01", "2024-02-29")"#, &[]),
            "28"
        );
    }

    #[test]
    fn duplicate_range_holidays_and_feb29_work() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-01"),
            ("b", "2024-01-02"),
            ("c", "2024-01-02"),
            ("d", "2024-01-05"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("networkdays(A2, D2, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "4",
            "a duplicated date in the holiday range counts once"
        );
        assert_eq!(
            eval_str(r#"workday("2024-01-06", 1, "2024-01-08")"#, &[]),
            "2024-01-09"
        );
        assert_eq!(
            eval_str(r#"days360("2024-02-29", "2024-03-31")"#, &[]),
            "32"
        );
        assert_eq!(
            eval_str(r#"days360("2024-02-29", "2024-03-31", 1)"#, &[]),
            "31"
        );
    }

    #[test]
    fn holidays_before_start_and_leap_day_ends_work() {
        assert_eq!(
            eval_str(
                r#"networkdays("2024-03-01", "2024-03-04", "2024-01-01")"#,
                &[]
            ),
            "2",
            "holiday before start is ignored"
        );
        assert_eq!(
            eval_str(r#"workday("2024-01-03", 2, "2024-01-03")"#, &[]),
            "2024-01-05"
        );
        assert_eq!(eval_str(r#"days360("2024-02-28", "2024-02-29")"#, &[]), "1");
        assert_eq!(eval_str(r#"days360("2024-02-29", "2024-03-01")"#, &[]), "2");
    }

    #[test]
    fn out_of_range_holidays_and_leap_february_work() {
        assert_eq!(
            eval_str(
                r#"networkdays("2024-01-01", "2024-01-05", "2024-01-10")"#,
                &[]
            ),
            "5",
            "a holiday outside the range is ignored"
        );
        assert_eq!(
            eval_str(r#"workday("2024-01-08", -1, "2024-01-08")"#, &[]),
            "2024-01-05"
        );
        assert_eq!(
            eval_str(r#"days360("2024-02-28", "2024-03-31")"#, &[]),
            "33"
        );
        assert_eq!(
            eval_str(r#"days360("2024-02-28", "2024-03-31", 1)"#, &[]),
            "32"
        );
    }

    #[test]
    fn all_holiday_and_cross_year_edges_work() {
        assert_eq!(
            eval_str(
                r#"networkdays("2024-01-01", "2024-01-05", "2024-01-01", "2024-01-02", "2024-01-03", "2024-01-04", "2024-01-05")"#,
                &[]
            ),
            "0"
        );
        assert_eq!(
            eval_str(r#"workday("2024-01-03", 1, "2024-01-03")"#, &[]),
            "2024-01-04"
        );
        assert_eq!(
            eval_str(r#"days360("2023-12-31", "2024-01-31")"#, &[]),
            "30"
        );
    }

    #[test]
    fn fractional_days_and_european_edge_cases_work() {
        assert_eq!(eval_str(r#"workday("2024-01-02", 1.9)"#, &[]), "2024-01-03");
        assert_eq!(
            eval_str(r#"workday("2024-01-05", -1.9)"#, &[]),
            "2024-01-04"
        );
        assert_eq!(
            eval_str(r#"days360("2024-01-29", "2024-01-31", 1)"#, &[]),
            "1"
        );
        assert_eq!(
            eval_str(r#"days360("2024-01-30", "2024-01-31", 1)"#, &[]),
            "0"
        );
    }

    #[test]
    fn holiday_range_skips_blank_cells() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-01"),
            ("b", ""),
            ("c", "2024-01-03"),
            ("d", "2024-01-05"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("networkdays(A2, D2, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "4"
        );
    }

    #[test]
    fn workday_weekend_start_and_holiday_weekends_work() {
        assert_eq!(eval_str(r#"workday("2024-01-06", 1)"#, &[]), "2024-01-08");
        assert_eq!(
            eval_str(
                r#"networkdays("2024-01-01", "2024-01-05", "2024-01-06")"#,
                &[]
            ),
            "5",
            "a weekend holiday does not reduce the count"
        );
        assert_eq!(eval_str(r#"days360("2024-01-29", "2024-01-31")"#, &[]), "2");
        assert_eq!(eval_str(r#"days360("2024-01-30", "2024-01-31")"#, &[]), "0");
    }

    #[test]
    fn weekend_and_31_day_boundaries_work() {
        assert_eq!(eval_str(r#"workday("2024-01-06", 0)"#, &[]), "2024-01-06");
        assert_eq!(
            eval_str(r#"days360("2024-01-31", "2024-03-31", 1)"#, &[]),
            "60"
        );
        assert_eq!(eval_str(r#"weeknum("2025-12-29", 21)"#, &[]), "1");
        assert_eq!(eval_str(r#"weeknum("2026-01-01", 21)"#, &[]), "1");
        assert_eq!(eval_str(r#"weeknum("2025-12-28", 21)"#, &[]), "52");
    }

    #[test]
    fn date_range_boundaries_work() {
        assert_eq!(
            eval_str(r#"networkdays("2024-02-28", "2024-03-01")"#, &[]),
            "3"
        );
        assert_eq!(eval_str(r#"weeknum("2024-12-30", 21)"#, &[]), "1");
        assert_eq!(
            eval_str(r#"days360("2024-01-31", "2024-02-29")"#, &[]),
            "29"
        );
        assert_eq!(
            eval_str(r#"networkdays("2024-01-06", "2024-01-07")"#, &[]),
            "0"
        );
        assert_eq!(eval_str(r#"eomonth("2024-03-15", -1)"#, &[]), "2024-02-29");
    }

    #[test]
    fn date_helpers_handle_the_1900_system_boundary() {
        assert_eq!(eval_str(r#"eomonth("1900-02-01", 0)"#, &[]), "1900-02-28");
        assert_eq!(
            eval_str(r#"weekday("1900-01-01")"#, &[]),
            "2",
            "Excel's serial 1 is a Monday"
        );
        assert_eq!(eval_str(r#"weekday("1900-03-01")"#, &[]), "5");
        assert_eq!(eval_str(r#"datevalue("1900-02-29")"#, &[]), "60");
    }

    #[test]
    fn days360_supports_us_and_european_methods() {
        assert_eq!(
            eval_str(r#"days360("2024-01-01", "2024-02-01")"#, &[]),
            "30"
        );
        assert_eq!(
            eval_str(r#"days360("2024-01-01", "2024-02-01", 1)"#, &[]),
            "30"
        );
        assert_eq!(
            eval_str(r#"days360("2024-02-01", "2024-01-01")"#, &[]),
            "#VALUE!"
        );
    }

    #[test]
    fn timevalue_datedif_and_clock_cells_work() {
        assert_eq!(eval_str(r#"timevalue("13:04:05")"#, &[]), "0.544502");
        assert_eq!(
            eval_str(r#"datedif("2020-01-01", "2024-01-01", "Y")"#, &[]),
            "4"
        );
        assert_eq!(
            eval_str(r#"datedif("2020-01-01", "2024-01-01", "M")"#, &[]),
            "48"
        );
        assert_eq!(
            eval_str(r#"datedif("2020-01-01", "2024-01-01", "D")"#, &[]),
            "1461"
        );
        assert_eq!(
            eval_str(r#"datedif("2024-01-02", "2024-01-01", "D")"#, &[]),
            "#VALUE!"
        );
        let today = eval_str("today()", &[]);
        assert_eq!(today.len(), 10, "{today}");
        let now = eval_str("now()", &[]);
        assert_eq!(now.len(), 19, "{now}");
        assert!(now.contains(' '), "{now}");
    }

    #[test]
    fn text_and_time_helpers_match_spreadsheet_expectations() {
        assert_eq!(eval_str(r##"text(1234.5, "#,##0.00")"##, &[]), "1,234.50");
        assert_eq!(eval_str(r#"text(0.123, "0.0%")"#, &[]), "12.3%");
        assert_eq!(
            eval_str(r#"text("2024-01-01", "yyyy-mm-dd")"#, &[]),
            "2024-01-01"
        );
        assert_eq!(eval_str(r#"text(1, "banana")"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"text(1234.5, "0.00E+00")"#, &[]), "1.23E+03");
        assert_eq!(
            eval_str(r#"text("-1234.5", "$#,##0.00;($#,##0.00)")"#, &[]),
            "($1,234.50)"
        );
        assert_eq!(eval_str(r#"text("007", "@")"#, &[]), "007");
        assert_eq!(eval_str(r#"textbefore("a-b-c", "-", 2)"#, &[]), "a-b");
        assert_eq!(eval_str(r#"textafter("a-b-c", "-", 2)"#, &[]), "c");
        assert_eq!(eval_str(r#"textbefore("a-b", "-", 2)"#, &[]), "#N/A");
        assert_eq!(eval_str(r#"textafter("a-b", "", 1)"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"datevalue("2024-01-01")"#, &[]), "45292");
        assert_eq!(eval_str(r#"datevalue("1900-01-01")"#, &[]), "1");
        assert_eq!(eval_str(r#"datevalue("1900-02-28")"#, &[]), "59");
        assert_eq!(eval_str(r#"datevalue("1900-02-29")"#, &[]), "60");
        assert_eq!(eval_str(r#"datevalue("1900-03-01")"#, &[]), "61");
        assert_eq!(eval_str(r#"hour("2024-01-01 13:04:05")"#, &[]), "13");
        assert_eq!(eval_str(r#"minute("2024-01-01T13:04:05")"#, &[]), "4");
        assert_eq!(eval_str(r#"second("13:04:05")"#, &[]), "5");
        assert_eq!(eval_str(r#"hour("0.5")"#, &[]), "12");
        assert_eq!(eval_str(r#"minute("bad")"#, &[]), "#VALUE!");
    }

    #[test]
    fn logical_fallbacks_and_switch_are_lazy() {
        assert_eq!(eval_str(r#"ifs(1>2, "a", 2>1, "b")"#, &[]), "b");
        assert_eq!(eval_str(r#"ifs(1>2, "a")"#, &[]), "#N/A");
        assert_eq!(eval_str(r#"switch("b", "a", 1, "b", 2)"#, &[]), "2");
        assert_eq!(eval_str(r#"switch("c", "a", 1, "b", 2, 99)"#, &[]), "99");
        assert_eq!(eval_str(r#"switch("c", "a", 1, "b", 2)"#, &[]), "#N/A");
        assert_eq!(eval_str(r#"choose(2, "a", "b", "c")"#, &[]), "b");
        assert_eq!(eval_str(r#"choose(0, "a")"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"choose(4, "a", "b", "c")"#, &[]), "#VALUE!");
        assert_eq!(eval_str("na()", &[]), "#N/A");
        assert_eq!(eval_str(r#"ifna(na(), "fallback")"#, &[]), "fallback");
        assert_eq!(eval_str(r#"ifna(1/0, "fallback")"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"iferror(na(), "fallback")"#, &[]), "fallback");
        assert_eq!(eval_str("isna(na())", &[]), "TRUE");
        assert_eq!(eval_str("iserror(na())", &[]), "TRUE");
        assert_eq!(eval_str("xor(true(), false(), true())", &[]), "FALSE");
        assert_eq!(eval_str("xor(true(), false())", &[]), "TRUE");
    }

    #[test]
    fn textbefore_after_support_if_not_found() {
        for (source, expected) in [
            (r#"textbefore("a-b", "-", 1, "missing")"#, "a"),
            (r#"textafter("a-b", "-", 1, "missing")"#, "b"),
            (r#"textbefore("abc", "-", 1, "missing")"#, "missing"),
            (r#"textafter("abc", "-", 1, "missing")"#, "missing"),
            (r#"textbefore("abc", "-", 1, 0)"#, "#N/A"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"textbefore("abc", "-")"#, r#"textafter("abc", "-")"#] {
            assert_eq!(eval_str(source, &[]), "#N/A", "{source}");
        }
        assert_eq!(eval_str(r#"textbefore("a-b", "-", 2, "x")"#, &[]), "x");
    }

    #[test]
    fn textbefore_after_honor_match_mode_and_match_end() {
        for (source, expected) in [
            (r#"textbefore("A-B", "-", 1, 1)"#, "A"),
            (r#"textafter("A-B", "-", 1, 1)"#, "B"),
            (r#"textbefore("aXb", "x", 1, 1)"#, "a"),
            (r#"textafter("aXb", "x", 1, 1)"#, "b"),
            (r#"textbefore("aXb", "x", 1, 0, 1)"#, "aXb"),
            (r#"textafter("aXb", "x", 1, 0, 1)"#, ""),
            (r#"textbefore("abc", "-", 1, 0, 1, "none")"#, "abc"),
            (r#"textafter("aÄb", "ä", 1, 1)"#, "b"),
            (r#"textbefore("Ä-B", "ä", 1, 1)"#, ""),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"textbefore("a-b", "-", 1, 2)"#,
            r#"textafter("a-b", "-", 1, 0, 2)"#,
            r#"textbefore("a-b", "-", 1, 1.5)"#,
            r#"textbefore("a-b", "-", 0)"#,
            r#"textafter("a-b", "-", 1.5)"#,
            r#"textbefore("a-b", "-", "x")"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn valuetotext_converts_values_and_quotes_strict_text() {
        for (source, expected) in [
            ("valuetotext(1.5)", "1.5"),
            ("valuetotext(true())", "TRUE"),
            ("valuetotext(na())", "#N/A"),
            (r#"valuetotext("a")"#, "a"),
            (r#"valuetotext("a", 1)"#, "\"a\""),
            ("valuetotext(1, 1)", "1"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "valuetotext()",
            "valuetotext(1, 2)",
            "valuetotext(1, 1.5)",
            r#"valuetotext(1, "x")"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn text_renders_sectioned_formats() {
        for (source, expected) in [
            (r#"text(1234.5, "0.00;0.000;0.0")"#, "1234.50"),
            (r#"text(-1234.5, "0.00;0.000;0.0")"#, "1234.500"),
            (r#"text(0, "0.00;0.000;0.0")"#, "0.0"),
            (r#"text(-1.25, "0.0;-0.00")"#, "-1.25"),
            (r#"text(0, "0.0;-0.00")"#, "0.0"),
            (r#"text(1.2, "0.0;-0.0")"#, "1.2"),
            (r#"text("abc", "0.0;0.0;0.0;@")"#, "abc"),
            (r#"text("abc", "0.0;0.0;0.0;pre@post")"#, "preabcpost"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"text(1, "0.0;0.0;0.0;0.0;0.0")"#,
            r#"text("x", "0.0;0.0")"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn text_renders_scientific_and_accounting_formats() {
        for (source, expected) in [
            (r#"text(1234.5, "0.00E+00")"#, "1.23E+03"),
            (r#"text(1234.5, "0.00e+00")"#, "1.23e+03"),
            (r#"text(-1234.5, "$#,##0.00;($#,##0.00)")"#, "($1,234.50)"),
            (r#"text(1234.5, "$#,##0.00;($#,##0.00)")"#, "$1,234.50"),
            (r#"text(0.125, "0.0%")"#, "12.5%"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"text(1234.5, "bogus")"#, "text()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn left_right_validate_length() {
        for (source, expected) in [
            (r#"left("abc", 2)"#, "ab"),
            (r#"right("abc", 2)"#, "bc"),
            (r#"left("abc")"#, "a"),
            (r#"right("abc")"#, "c"),
            (r#"left("abc", 0)"#, ""),
            (r#"right("abc", 0)"#, ""),
            (r#"left("abc", 9)"#, "abc"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"left("abc", -1)"#,
            r#"right("abc", -1)"#,
            r#"left("abc", 1.5)"#,
            r#"right("abc", "x")"#,
            r#"left("abc", 1, 2)"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn mid_validates_start_and_length() {
        for (source, expected) in [
            (r#"mid("abc", 2, 2)"#, "bc"),
            (r#"mid("abc", 1)"#, "abc"),
            (r#"mid("abc", 9, 2)"#, ""),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"mid("abc", 0, 1)"#,
            r#"mid("abc", 1.5, 1)"#,
            r#"mid("abc", "x", 1)"#,
            r#"mid("abc", 1, -1)"#,
            r#"mid("abc", 1, 1.5)"#,
            r#"mid("abc", 1, 1, 2)"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn rept_requires_integer_count_within_limit() {
        for (source, expected) in [
            (r#"rept("ab", 2)"#, "abab"),
            (r#"rept("ab", 0)"#, ""),
            (r#"rept("ab", 10000)"#, &"ab".repeat(10000)),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"rept("ab", 2.5)"#,
            r#"rept("ab", -1)"#,
            r#"rept("ab", 10001)"#,
            r#"rept("ab", "x")"#,
            "rept()",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
        assert_eq!(
            eval_str(r#"rept("abcdefg", 4681)"#, &[]),
            "abcdefg".repeat(4681)
        );
        assert_eq!(eval_str(r#"rept("abcdefg", 4682)"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"rept("abcd", 10000)"#, &[]), "#VALUE!");
    }

    #[test]
    fn search_folds_unicode_case_and_keeps_positions() {
        for (source, expected) in [
            (r#"search("é", "ÉCLAIR")"#, "1"),
            (r#"search("ä", "BÄR")"#, "2"),
            (r#"search("c", "äbc", 2)"#, "3"),
            (r#"find("É", "ÉCLAIR")"#, "1"),
            (r#"find("é", "ÉCLAIR")"#, "0"),
            (r#"search("x", "abc", 9)"#, "0"),
            (r#"find("a", "abc", 4)"#, "0"),
            (r#"search("a*", "abc")"#, "1"),
            (r#"search("b*", "abc")"#, "2"),
            (r#"search("*c", "abc")"#, "1"),
            (r#"search("?c", "abc")"#, "2"),
            (r#"search("a?c", "abc")"#, "1"),
            (r#"search("*", "abc")"#, "1"),
            (r#"search("a~*", "a*b")"#, "1"),
            (r#"search("a~?", "a?b")"#, "1"),
            (r#"search("É*", "éclair")"#, "1"),
            (r#"search("z*", "abc")"#, "0"),
            (r#"find("a*", "a*b")"#, "1"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"find("a", "abc", 0)"#,
            r#"search("a", "abc", 0)"#,
            r#"find("a", "abc", 1.5)"#,
            r#"search("a", "abc", "x")"#,
            r#"find("a", "abc", 1, 2)"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn clean_strips_control_characters() {
        for (source, expected) in [
            (r#"clean(concat("a", char(9), "b"))"#, "ab"),
            (r#"clean(concat("a", char(10), "b"))"#, "ab"),
            (r#"clean(" a ")"#, " a "),
            (r#"clean("plain")"#, "plain"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["clean()", r#"clean("x", "y")"#] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn textsplit_can_skip_empty_segments() {
        for (source, expected) in [
            (r#"textsplit(",a,b", ",", "", 1)"#, "a"),
            (r#"textsplit(",a,b", ",", "", 0)"#, ""),
            (r#"textsplit("a,,b", ",", "", 1)"#, "a"),
            (r#"textsplit(",", ",", "", 1)"#, ""),
            (r#"textsplit(",a", ",", "", 1, "x")"#, "a"),
            (r#"textsplit("a,b|c,d", ",", "|")"#, "a"),
            (r#"textsplit("x|a,b", ",", "|")"#, "x"),
            (r#"textsplit("a,b|c", ",", "|", 1, "x")"#, "a"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"textsplit(",a,b", ",", "", 2)"#,
            r#"textsplit(",a,b", "")"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
        assert_eq!(eval_str(r#"textsplit("|a,b", ",", "|", 1, "")"#, &[]), "");
        assert_eq!(eval_str(r#"textsplit(",", ",", "", 1, "x")"#, &[]), "x");
        assert_eq!(eval_str(r#"textsplit("", ",", "", 1, "x")"#, &[]), "x");
        assert_eq!(eval_str(r#"textsplit(",", ",", "", 0, "x")"#, &[]), "");
    }

    #[test]
    fn textsplit_returns_first_segment() {
        for (source, expected) in [
            (r#"textsplit("a,b,c", ",")"#, "a"),
            (r#"textsplit("abc", "x")"#, "abc"),
            (r#"textsplit("a-b", "-")"#, "a"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"textsplit("a,b,c", "")"#,
            "textsplit(1, 2)",
            "textsplit(\"a\")",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn chisq_test_returns_goodness_of_fit_pvalue() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
            "f".to_string(),
            "g".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "1"),
            ("b", "10"),
            ("c", "20"),
            ("d", "30"),
            ("e", "12"),
            ("f", "18"),
            ("g", "30"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("chisq.test(B2:D2, E2:G2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0.757465"
        );
        for source in ["chisq.test(B2:D2, E2:F2)", "chisq.test(1, 2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
    }

    #[test]
    fn arraytotext_renders_same_row_range() {
        let columns = vec![
            "id".to_string(),
            "amount".to_string(),
            "qty".to_string(),
            "note".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "7".to_string());
        cells.insert("amount".to_string(), "10".to_string());
        cells.insert("qty".to_string(), "3".to_string());
        cells.insert("note".to_string(), "plain".to_string());

        for (source, expected) in [
            ("arraytotext(B2:D2)", "10, 3, plain"),
            ("arraytotext(B2:D2, 0)", "10, 3, plain"),
            ("arraytotext(B2:D2, 1)", "{10,3,\"plain\"}"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["arraytotext(1)", "arraytotext(B2:D2, 2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
    }

    #[test]
    fn mode_picks_the_most_frequent_value_of_a_same_row_range() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "10"), ("b", "3"), ("c", "10"), ("d", "7")] {
            cells.insert(name.to_string(), value.to_string());
        }
        for source in ["mode(A2:D2)", "mode.sngl(A2:D2)", "mode.mult(A2:D2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "10",
                "{source}"
            );
        }
        // A tie picks the smaller value so the answer is deterministic.
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "1"), ("d", "2")] {
            cells.insert(name.to_string(), value.to_string());
        }
        let tie = Formula::parse("mode.sngl(A2:D2)").unwrap();
        assert_eq!(
            tie.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "1"
        );
        // No repeated value has no mode.
        for (name, value) in [("a", "10"), ("b", "3"), ("c", "7"), ("d", "5")] {
            cells.insert(name.to_string(), value.to_string());
        }
        let no_mode = Formula::parse("mode.sngl(A2:D2)").unwrap();
        assert_eq!(
            no_mode
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        cells.insert("d".to_string(), "n/a".to_string());
        let text = Formula::parse("mode.sngl(A2:D2)").unwrap();
        assert_eq!(
            text.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "#VALUE!",
            "text is ignored and leaves three distinct numbers"
        );
    }

    #[test]
    fn rank_orders_descending_by_default_and_ascending_on_request() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("rank(3, A2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2"
        );
        let formula = Formula::parse("rank(3, A2:D2, 1)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "3"
        );
        let formula = Formula::parse("rank(5, A2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1",
            "a value larger than every cell still ranks first"
        );
    }

    #[test]
    fn dotted_aliases_match_undotted_and_unknown_dotted_is_an_error() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (dotted_source, plain_source) in [
            ("stdev.s(A2:D2)", "stdev(A2:D2)"),
            ("var.s(A2:D2)", "var(A2:D2)"),
            ("stdev.p(A2:D2)", "stdevp(A2:D2)"),
            ("var.p(A2:D2)", "varp(A2:D2)"),
        ] {
            let dotted = Formula::parse(dotted_source).unwrap();
            let plain = Formula::parse(plain_source).unwrap();
            assert_eq!(
                dotted
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                plain
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "{dotted_source} vs {plain_source}"
            );
        }
        let unknown = Formula::parse("foo.bar(A2:D2)").unwrap();
        assert_eq!(
            unknown
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn dotted_statistical_aliases_parse_and_evaluate() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")] {
            cells.insert(name.to_string(), value.to_string());
        }
        let rank = Formula::parse("rank.eq(3, A2:D2)").unwrap();
        assert_eq!(
            rank.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "2"
        );
        let sample = Formula::parse("stdev.s(A2:D2)").unwrap();
        assert_eq!(
            sample
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1.290994"
        );
        let population = Formula::parse("stdev.p(A2:D2)").unwrap();
        assert_eq!(
            population
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1.118034"
        );
        let variance = Formula::parse("var.s(A2:D2)").unwrap();
        assert_eq!(
            variance
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1.666667"
        );
    }

    #[test]
    fn percentile_family_rejects_empty_or_text_only_ranges() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for name in ["a", "b", "c", "d"] {
            cells.insert(name.to_string(), "n/a".to_string());
        }
        for formula in [
            "percentile(A2:D2, 0.5)",
            "quartile(A2:D2, 2)",
            "percentrank(A2:D2, 2)",
            "geomean(A2:D2)",
        ] {
            let parsed = Formula::parse(formula).unwrap();
            assert_eq!(
                parsed
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{formula}"
            );
        }
    }

    #[test]
    fn iserr_ignores_na_but_sees_other_errors() {
        for (source, expected) in [
            ("iserr(1)", "FALSE"),
            (r#"iserr("x")"#, "FALSE"),
            ("iserr(na())", "FALSE"),
            ("iserr(1/0)", "TRUE"),
            ("iserr(sqrt(-1))", "TRUE"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        assert_eq!(eval_str("iserr()", &[]), "#VALUE!");
    }

    #[test]
    fn logical_and_nontext_tests_work() {
        for (source, expected) in [
            ("islogical(true())", "TRUE"),
            ("islogical(1)", "FALSE"),
            (r#"islogical("TRUE")"#, "FALSE"),
            ("isnontext(1)", "TRUE"),
            (r#"isnontext("x")"#, "FALSE"),
            ("isnontext(true())", "TRUE"),
            ("isnontext(na())", "TRUE"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["islogical()", "isnontext()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn precise_error_functions_work() {
        for (source, expected) in [
            ("erf.precise(0)", "0"),
            ("erf.precise(1)", "0.842701"),
            ("erfc.precise(0)", "1"),
            ("erfc.precise(1)", "0.157299"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"erf.precise("x")"#,
            "erf.precise(1, 2)",
            "erf.precise()",
            r#"erfc.precise("x")"#,
            "erfc.precise(1, 2)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn isref_recognizes_references_without_evaluating() {
        for (source, expected) in [
            ("isref(A1)", "TRUE"),
            ("isref(A1:B1)", "TRUE"),
            ("isref(1)", "FALSE"),
            (r#"isref("A1")"#, "FALSE"),
            ("isref(sum(A1:B1))", "FALSE"),
            ("isref(na())", "FALSE"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        assert_eq!(eval_str("isref()", &[]), "#VALUE!");
    }

    #[test]
    fn hypgeom_inv_finds_cutoff() {
        for (source, expected) in [
            ("hypgeom.inv(4, 8, 20, 0.5)", "2"),
            ("hypgeom.inv(4, 8, 20, 0.2)", "1"),
            ("hypgeom.inv(4, 8, 20, 0.1)", "0"),
            ("hypgeom.inv(4, 8, 20, 1)", "4"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "hypgeom.inv(-1, 8, 20, 0.5)",
            "hypgeom.inv(4, 21, 20, 0.5)",
            "hypgeom.inv(4, 8, 20, -0.1)",
            "hypgeom.inv(4, 8, 20, 1.1)",
            r#"hypgeom.inv("x", 8, 20, 0.5)"#,
            "hypgeom.inv(4, 8, 20)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn besselk_returns_second_kind_values() {
        for (source, expected) in [
            ("besselk(1, 0)", "0.421024"),
            ("besselk(1, 1)", "0.601907"),
            ("besselk(2, 0)", "0.113894"),
            ("besselk(2, 1)", "0.139866"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "besselk(0, 0)",
            "besselk(-1, 0)",
            "besselk(1, -1)",
            "besselk(1, 1.5)",
            "besselk(1, 21)",
            r#"besselk("x", 0)"#,
            "besselk(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn bessely_returns_second_kind_values() {
        for (source, expected) in [
            ("bessely(1, 0)", "0.088257"),
            ("bessely(1, 1)", "-0.781213"),
            ("bessely(2, 0)", "0.510376"),
            ("bessely(2, 1)", "-0.107032"),
            ("bessely(0.5, 0)", "-0.444519"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "bessely(0, 0)",
            "bessely(-1, 0)",
            "bessely(1, -1)",
            "bessely(1, 1.5)",
            "bessely(1, 21)",
            r#"bessely("x", 0)"#,
            "bessely(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn hypgeom_dist_returns_mass_or_cumulative() {
        for (source, expected) in [
            ("hypgeom.dist(1, 4, 8, 20, 0)", "0.363261"),
            ("hypgeom.dist(2, 4, 8, 20, 0)", "0.381424"),
            ("hypgeom.dist(1, 4, 8, 20, 1)", "0.465428"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "hypgeom.dist(5, 4, 8, 20, 0)",
            "hypgeom.dist(-1, 4, 8, 20, 0)",
            "hypgeom.dist(1, 21, 8, 20, 0)",
            "hypgeom.dist(1, 4, 21, 20, 0)",
            r#"hypgeom.dist("x", 4, 8, 20, 0)"#,
            "hypgeom.dist(1, 4, 8, 20)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn besseli_returns_modified_values() {
        for (source, expected) in [
            ("besseli(0, 0)", "1"),
            ("besseli(0, 1)", "0"),
            ("besseli(1, 0)", "1.266066"),
            ("besseli(1, 1)", "0.565159"),
            ("besseli(2, 0)", "2.279585"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "besseli(1, -1)",
            "besseli(1, 1.5)",
            "besseli(1, 101)",
            r#"besseli("x", 0)"#,
            "besseli(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn besselj_returns_first_kind_values() {
        for (source, expected) in [
            ("besselj(0, 0)", "1"),
            ("besselj(0, 1)", "0"),
            ("besselj(1, 0)", "0.765198"),
            ("besselj(1, 1)", "0.440051"),
            ("besselj(2, 0)", "0.223891"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "besselj(1, -1)",
            "besselj(1, 1.5)",
            "besselj(1, 101)",
            r#"besselj("x", 0)"#,
            "besselj(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn t_test_returns_pvalues() {
        let columns = vec![
            "id".to_string(),
            "amount".to_string(),
            "qty".to_string(),
            "note".to_string(),
            "x".to_string(),
            "y".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "7".to_string());
        cells.insert("amount".to_string(), "10".to_string());
        cells.insert("qty".to_string(), "3".to_string());
        cells.insert("note".to_string(), "plain".to_string());
        cells.insert("x".to_string(), "5".to_string());
        cells.insert("y".to_string(), "9".to_string());

        for (source, expected) in [
            ("t.test(B2:C2, B2:C2, 2, 2)", "1"),
            ("t.test(B2:C2, B2:C2, 1, 2)", "0.5"),
            ("t.test(B2:C2, E2:F2, 2, 1)", "0.942284"),
            ("t.test(B2:C2, E2:F2, 1, 1)", "0.471142"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["t.test(B2:C2, E2:F2, 3, 1)", "t.test(B2:C2, E2:F2, 2, 0)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
    }

    #[test]
    fn confidence_t_returns_half_width() {
        for (source, expected) in [
            ("confidence.t(0.5, 1, 2)", "0.707107"),
            ("confidence.t(0.5, 2, 2)", "1.414214"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "confidence.t(0, 1, 2)",
            "confidence.t(1, 1, 2)",
            "confidence.t(0.5, 0, 2)",
            "confidence.t(0.5, 1, 1)",
            "confidence.t(0.5, 1, 2.5)",
            r#"confidence.t("x", 1, 2)"#,
            "confidence.t(0.5, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn confidence_norm_returns_half_width() {
        for (source, expected) in [
            ("confidence.norm(0.05, 1, 100)", "0.195996"),
            ("confidence.norm(0.05, 1, 25)", "0.391993"),
            ("confidence.norm(0.05, 2, 16)", "0.979982"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "confidence.norm(0, 1, 100)",
            "confidence.norm(1, 1, 100)",
            "confidence.norm(0.05, 0, 100)",
            "confidence.norm(0.05, 1, 0)",
            "confidence.norm(0.05, 1, 2.5)",
            r#"confidence.norm("x", 1, 100)"#,
            "confidence.norm(0.05, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn f_test_returns_two_tailed_pvalue() {
        let columns = vec![
            "id".to_string(),
            "amount".to_string(),
            "qty".to_string(),
            "note".to_string(),
            "x".to_string(),
            "y".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "7".to_string());
        cells.insert("amount".to_string(), "10".to_string());
        cells.insert("qty".to_string(), "3".to_string());
        cells.insert("note".to_string(), "plain".to_string());
        cells.insert("x".to_string(), "5".to_string());
        cells.insert("y".to_string(), "9".to_string());

        for (source, expected) in [
            ("f.test(B2:C2, B2:C2)", "1"),
            ("f.test(B2:C2, E2:F2)", "0.660997"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn z_test_returns_one_tailed_probability() {
        let columns = vec!["id".to_string(), "amount".to_string(), "qty".to_string()];
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "7".to_string());
        cells.insert("amount".to_string(), "10".to_string());
        cells.insert("qty".to_string(), "3".to_string());

        for (source, expected) in [
            ("z.test(B2:C2, 6.5)", "0.5"),
            ("z.test(B2:C2, 6)", "0.443201"),
            ("z.test(B2:C2, 6.5, 1)", "0.5"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["z.test(B2:C2, 6.5, 0)", "z.test(B2:C2, 6.5, -1)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
    }

    #[test]
    fn gamma_inv_returns_quantiles() {
        for (source, expected) in [
            ("gamma.inv(0.5, 1, 1)", "0.693147"),
            ("gamma.inv(0.5, 1, 2)", "1.386294"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "gamma.inv(0, 1, 1)",
            "gamma.inv(1, 1, 1)",
            "gamma.inv(0.5, 0, 1)",
            "gamma.inv(0.5, 1, 0)",
            r#"gamma.inv("x", 1, 1)"#,
            "gamma.inv(0.5, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn beta_inv_returns_quantiles() {
        for (source, expected) in [
            ("beta.inv(0.5, 2, 2, 0, 1)", "0.5"),
            ("beta.inv(0.5, 2, 2, 10, 20)", "15"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "beta.inv(0, 2, 2, 0, 1)",
            "beta.inv(1, 2, 2, 0, 1)",
            "beta.inv(0.5, 0, 2, 0, 1)",
            "beta.inv(0.5, 2, 0, 0, 1)",
            "beta.inv(0.5, 2, 2, 1, 0)",
            r#"beta.inv("x", 2, 2, 0, 1)"#,
            "beta.inv(0.5, 2)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn chisq_inv_right_tail_returns_quantiles() {
        for (source, expected) in [
            ("chisq.inv.rt(0.5, 1)", "0.454936"),
            ("chisq.inv.rt(0.5, 2)", "1.386294"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "chisq.inv.rt(0, 1)",
            "chisq.inv.rt(1, 1)",
            "chisq.inv.rt(0.5, 0)",
            r#"chisq.inv.rt("x", 1)"#,
            "chisq.inv.rt(0.5)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn chisq_inv_returns_quantiles() {
        for (source, expected) in [
            ("chisq.inv(0.5, 1)", "0.454936"),
            ("chisq.inv(0.5, 2)", "1.386294"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "chisq.inv(0, 1)",
            "chisq.inv(1, 1)",
            "chisq.inv(0.5, 0)",
            r#"chisq.inv("x", 1)"#,
            "chisq.inv(0.5)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn f_inv_returns_left_tail_quantiles() {
        for (source, expected) in [("f.inv(0.5, 1, 1)", "1"), ("f.inv(0.5, 2, 2)", "1")] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "f.inv(0, 1, 1)",
            "f.inv(1, 1, 1)",
            "f.inv(0.5, 0, 1)",
            "f.inv(0.5, 1, 0)",
            r#"f.inv("x", 1, 1)"#,
            "f.inv(0.5, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn f_inv_right_tail_returns_quantiles() {
        for (source, expected) in [("f.inv.rt(0.5, 1, 1)", "1"), ("f.inv.rt(0.5, 2, 2)", "1")] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "f.inv.rt(0, 1, 1)",
            "f.inv.rt(1, 1, 1)",
            "f.inv.rt(0.5, 0, 1)",
            "f.inv.rt(0.5, 1, 0)",
            r#"f.inv.rt("x", 1, 1)"#,
            "f.inv.rt(0.5, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn inverse_reciprocal_hyperbolic_work() {
        for (source, expected) in [
            ("asech(1)", "0"),
            ("asech(0.5)", "1.316958"),
            ("acsch(1)", "0.881374"),
            ("acsch(-1)", "-0.881374"),
            ("acoth(2)", "0.549306"),
            ("acoth(-2)", "-0.549306"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "asech(0)",
            "asech(2)",
            "acsch(0)",
            "acoth(1)",
            "acoth(0.5)",
            r#"asech("x")"#,
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn chisq_dist_right_tail_works() {
        for (source, expected) in [
            ("chisq.dist.rt(0, 1)", "1"),
            ("chisq.dist.rt(1, 1)", "0.317311"),
            ("chisq.dist.rt(2, 2)", "0.367879"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "chisq.dist.rt(-1, 1)",
            "chisq.dist.rt(1, 0)",
            r#"chisq.dist.rt("x", 1)"#,
            "chisq.dist.rt(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn norm_inv_returns_quantiles() {
        for (source, expected) in [
            ("norm.inv(0.5, 0, 1)", "0"),
            ("norm.inv(0.841344746, 0, 1)", "1"),
            ("norm.inv(0.975, 0, 1)", "1.959964"),
            ("norm.inv(0.5, 10, 2)", "10"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "norm.inv(0, 0, 1)",
            "norm.inv(1, 0, 1)",
            "norm.inv(0.5, 0, 0)",
            r#"norm.inv("x", 0, 1)"#,
            "norm.inv(0.5, 0)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn t_inv_two_tail_returns_positive_quantile() {
        for (source, expected) in [("t.inv.2t(0.5, 1)", "1"), ("t.inv.2t(0.8, 1)", "0.32492")] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "t.inv.2t(0, 1)",
            "t.inv.2t(1, 1)",
            "t.inv.2t(0.5, 0)",
            r#"t.inv.2t("x", 1)"#,
            "t.inv.2t(0.5)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn binom_dist_range_sums_interval() {
        for (source, expected) in [
            ("binom.dist.range(4, 0.5, 2, 3)", "0.625"),
            ("binom.dist.range(4, 0.5, 0, 0)", "0.0625"),
            ("binom.dist.range(4, 0.5, 4, 4)", "0.0625"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "binom.dist.range(-1, 0.5, 0, 1)",
            "binom.dist.range(4, 1.5, 0, 1)",
            "binom.dist.range(4, 0.5, 3, 2)",
            "binom.dist.range(4, 0.5, 0, 5)",
            r#"binom.dist.range("x", 0.5, 0, 1)"#,
            "binom.dist.range(4, 0.5, 0)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn t_inv_returns_quantiles() {
        for (source, expected) in [
            ("t.inv(0.5, 1)", "0"),
            ("t.inv(0.75, 1)", "1"),
            ("t.inv(0.25, 1)", "-1"),
            ("t.inv(0.5, 10)", "0"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "t.inv(0, 1)",
            "t.inv(1, 1)",
            "t.inv(0.5, 0)",
            r#"t.inv("x", 1)"#,
            "t.inv(0.5)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn t_dist_two_tail_works() {
        for (source, expected) in [
            ("t.dist.2t(0, 1)", "1"),
            ("t.dist.2t(1, 1)", "0.5"),
            ("t.dist.2t(1, 2)", "0.42265"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "t.dist.2t(-1, 1)",
            "t.dist.2t(1, 0)",
            r#"t.dist.2t("x", 1)"#,
            "t.dist.2t(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn binom_inv_finds_cutoff() {
        for (source, expected) in [
            ("binom.inv(1, 0.5, 0.5)", "0"),
            ("binom.inv(1, 0.5, 0.6)", "1"),
            ("binom.inv(4, 0.5, 0.6875)", "2"),
            ("binom.inv(4, 0.5, 1)", "4"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "binom.inv(-1, 0.5, 0.5)",
            "binom.inv(2.5, 0.5, 0.5)",
            "binom.inv(4, 1.5, 0.5)",
            "binom.inv(4, 0.5, -0.1)",
            r#"binom.inv("x", 0.5, 0.5)"#,
            "binom.inv(4, 0.5)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn t_dist_right_tail_works() {
        for (source, expected) in [
            ("t.dist.rt(0, 1)", "0.5"),
            ("t.dist.rt(1, 1)", "0.25"),
            ("t.dist.rt(1, 2)", "0.211325"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "t.dist.rt(-1, 1)",
            "t.dist.rt(1, 0)",
            r#"t.dist.rt("x", 1)"#,
            "t.dist.rt(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn f_dist_right_tail_works() {
        for (source, expected) in [
            ("f.dist.rt(1, 1, 1)", "0.5"),
            ("f.dist.rt(1, 2, 2)", "0.5"),
            ("f.dist.rt(0, 1, 1)", "1"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "f.dist.rt(-1, 1, 1)",
            "f.dist.rt(1, 0, 1)",
            "f.dist.rt(1, 1, 0)",
            r#"f.dist.rt("x", 1, 1)"#,
            "f.dist.rt(1, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn lognorm_inv_returns_quantiles() {
        for (source, expected) in [
            ("lognorm.inv(0.5, 0, 1)", "1"),
            ("lognorm.inv(0.841344746, 0, 1)", "2.718282"),
            ("lognorm.inv(0.975, 0, 1)", "7.099071"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "lognorm.inv(0, 0, 1)",
            "lognorm.inv(1, 0, 1)",
            "lognorm.inv(0.5, 0, 0)",
            r#"lognorm.inv("x", 0, 1)"#,
            "lognorm.inv(0.5, 0)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn t_dist_returns_density_or_cumulative() {
        for (source, expected) in [
            ("t.dist(0, 1, 1)", "0.5"),
            ("t.dist(0, 1, 0)", "0.31831"),
            ("t.dist(1, 1, 1)", "0.75"),
            ("t.dist(-1, 1, 1)", "0.25"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["t.dist(0, 0, 1)", r#"t.dist("x", 1, 1)"#, "t.dist(0, 1)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn f_dist_returns_density_or_cumulative() {
        for (source, expected) in [
            ("f.dist(1, 1, 1, 1)", "0.5"),
            ("f.dist(1, 2, 2, 1)", "0.5"),
            ("f.dist(1, 1, 1, 0)", "0.159155"),
            ("f.dist(1, 2, 2, 0)", "0.25"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "f.dist(-1, 1, 1, 1)",
            "f.dist(1, 0, 1, 1)",
            "f.dist(1, 1, 0, 1)",
            r#"f.dist("x", 1, 1, 1)"#,
            "f.dist(1, 1, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn norm_dist_returns_density_or_cumulative() {
        for (source, expected) in [
            ("norm.dist(0, 0, 1, 1)", "0.5"),
            ("norm.dist(0, 0, 1, 0)", "0.398942"),
            ("norm.dist(1, 1, 1, 1)", "0.5"),
            ("norm.dist(2, 1, 1, 1)", "0.841345"),
            ("norm.dist(1, 0, 1, 1)", "0.841345"),
            ("norm.dist(0, 1, 1, 1)", "0.158655"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "norm.dist(0, 0, 0, 1)",
            r#"norm.dist("x", 0, 1, 1)"#,
            "norm.dist(0, 0, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn chisq_dist_returns_density_or_cumulative() {
        for (source, expected) in [
            ("chisq.dist(0, 1, 1)", "0"),
            ("chisq.dist(1, 1, 1)", "0.682689"),
            ("chisq.dist(2, 2, 1)", "0.632121"),
            ("chisq.dist(1, 1, 0)", "0.241971"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "chisq.dist(-1, 1, 1)",
            "chisq.dist(1, 0, 1)",
            r#"chisq.dist("x", 1, 1)"#,
            "chisq.dist(1, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn gamma_dist_returns_density_or_cumulative() {
        for (source, expected) in [
            ("gamma.dist(1, 1, 1, 1)", "0.632121"),
            ("gamma.dist(1, 1, 1, 0)", "0.367879"),
            ("gamma.dist(2, 2, 1, 1)", "0.593994"),
            ("gamma.dist(2, 2, 1, 0)", "0.270671"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "gamma.dist(-1, 1, 1, 1)",
            "gamma.dist(1, 0, 1, 1)",
            "gamma.dist(1, 1, 0, 1)",
            r#"gamma.dist("x", 1, 1, 1)"#,
            "gamma.dist(1, 1, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn beta_dist_returns_density_or_cumulative() {
        for (source, expected) in [
            ("beta.dist(0.5, 1, 1, 1)", "0.5"),
            ("beta.dist(0.5, 1, 1, 0)", "1"),
            ("beta.dist(0.5, 2, 2, 1)", "0.5"),
            ("beta.dist(0.5, 2, 2, 0)", "1.5"),
            ("beta.dist(0.25, 2, 2, 1)", "0.15625"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "beta.dist(-0.1, 1, 1, 1)",
            "beta.dist(1.1, 1, 1, 1)",
            "beta.dist(0.5, 0, 1, 1)",
            "beta.dist(0.5, 1, 0, 1)",
            r#"beta.dist("x", 1, 1, 1)"#,
            "beta.dist(0.5, 1, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn weibull_dist_returns_density_or_cumulative() {
        for (source, expected) in [
            ("weibull.dist(1, 1, 1, 1)", "0.632121"),
            ("weibull.dist(1, 1, 1, 0)", "0.367879"),
            ("weibull.dist(2, 2, 1, 1)", "0.981684"),
            ("weibull.dist(0, 1, 1, 0)", "1"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "weibull.dist(-1, 1, 1, 1)",
            "weibull.dist(1, 0, 1, 1)",
            "weibull.dist(1, 1, 0, 1)",
            r#"weibull.dist("x", 1, 1, 1)"#,
            "weibull.dist(1, 1, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn lognorm_dist_returns_density_or_cumulative() {
        for (source, expected) in [
            ("lognorm.dist(1, 0, 1, 1)", "0.5"),
            ("lognorm.dist(1, 0, 1, 0)", "0.398942"),
            ("lognorm.dist(2, 0, 1, 1)", "0.755891"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "lognorm.dist(0, 0, 1, 1)",
            "lognorm.dist(1, 0, 0, 1)",
            r#"lognorm.dist("x", 0, 1, 1)"#,
            "lognorm.dist(1, 0, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn binom_dist_returns_mass_or_cumulative() {
        for (source, expected) in [
            ("binom.dist(0, 1, 0.5, 0)", "0.5"),
            ("binom.dist(1, 1, 0.5, 0)", "0.5"),
            ("binom.dist(0, 1, 0.5, 1)", "0.5"),
            ("binom.dist(1, 1, 0.5, 1)", "1"),
            ("binom.dist(2, 4, 0.5, 0)", "0.375"),
            ("binom.dist(2, 4, 0.5, 1)", "0.6875"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "binom.dist(-1, 4, 0.5, 1)",
            "binom.dist(5, 4, 0.5, 1)",
            "binom.dist(1, 1.5, 0.5, 1)",
            "binom.dist(1, 4, 1.5, 1)",
            r#"binom.dist("x", 4, 0.5, 1)"#,
            "binom.dist(1, 4, 0.5)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn expon_dist_returns_density_or_cumulative() {
        for (source, expected) in [
            ("expon.dist(0, 1, 0)", "1"),
            ("expon.dist(1, 1, 0)", "0.367879"),
            ("expon.dist(1, 1, 1)", "0.632121"),
            ("expon.dist(2, 0.5, 1)", "0.632121"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "expon.dist(-1, 1, 1)",
            "expon.dist(1, 0, 1)",
            r#"expon.dist("x", 1, 1)"#,
            "expon.dist(1, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn poisson_dist_returns_mass_or_cumulative() {
        for (source, expected) in [
            ("poisson.dist(0, 1, 0)", "0.367879"),
            ("poisson.dist(0, 1, 1)", "0.367879"),
            ("poisson.dist(1, 1, 0)", "0.367879"),
            ("poisson.dist(1, 1, 1)", "0.735759"),
            ("poisson.dist(2, 1, 0)", "0.18394"),
            ("poisson.dist(2, 1, 1)", "0.919699"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "poisson.dist(-1, 1, 1)",
            "poisson.dist(1.5, 1, 1)",
            "poisson.dist(1, 0, 1)",
            r#"poisson.dist("x", 1, 1)"#,
            "poisson.dist(1, 1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn norm_s_dist_returns_cumulative_or_density() {
        for (source, expected) in [
            ("norm.s.dist(0, 1)", "0.5"),
            ("norm.s.dist(0, 0)", "0.398942"),
            ("norm.s.dist(1, 1)", "0.841345"),
            ("norm.s.dist(1, 0)", "0.241971"),
            ("norm.s.dist(-1, 1)", "0.158655"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "norm.s.dist(0)",
            r#"norm.s.dist("x", 1)"#,
            "norm.s.dist(0, 1, 2)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn phi_returns_normal_density() {
        for (source, expected) in [
            ("phi(0)", "0.398942"),
            ("phi(1)", "0.241971"),
            ("phi(-1)", "0.241971"),
            ("phi(2)", "0.053991"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"phi("x")"#, "phi()", "phi(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn normal_inverse_matches_standard_quantiles() {
        for (source, expected) in [
            ("norm.s.inv(0.5)", "0"),
            ("norm.s.inv(0.841344746)", "1"),
            ("norm.s.inv(0.975)", "1.959964"),
            ("norm.s.inv(0.025)", "-1.959964"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["norm.s.inv(0)", "norm.s.inv(1)", r#"norm.s.inv("x")"#] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn n_and_t_coerce_values() {
        for (source, expected) in [
            ("n(1)", "1"),
            ("n(1=1)", "1"),
            ("n(1=0)", "0"),
            (r#"n("x")"#, "0"),
            (r#"n("")"#, "0"),
            (r#"t("x")"#, "x"),
            ("t(1)", ""),
            ("t(1=1)", ""),
            (r#"t("")"#, ""),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["n(1, 2)", r#"t("x", "y")"#, "n(1/0)", "t(1/0)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn gauss_returns_normal_half_probability() {
        for (source, expected) in [
            ("gauss(0)", "0"),
            ("gauss(1)", "0.341345"),
            ("gauss(-1)", "-0.341345"),
            ("gauss(2)", "0.47725"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"gauss("x")"#, "gauss()", "gauss(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn sec_and_csc_are_reciprocals() {
        for (source, expected) in [
            ("sec(0)", "1"),
            ("sec(1)", "1.850816"),
            ("csc(1)", "1.188395"),
            ("csc(-1)", "-1.188395"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["csc(0)", r#"sec("x")"#, "sec(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn permutationa_counts_repetition_permutations() {
        for (source, expected) in [
            ("permutationa(3, 2)", "9"),
            ("permutationa(2, 3)", "8"),
            ("permutationa(0, 0)", "1"),
            ("permutationa(0, 2)", "0"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "permutationa(-1, 2)",
            "permutationa(2.5, 2)",
            r#"permutationa("x", 2)"#,
            "permutationa(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn sqrtpi_scales_by_pi() {
        for (source, expected) in [
            ("sqrtpi(0)", "0"),
            ("sqrtpi(1)", "1.772454"),
            ("sqrtpi(4)", "3.544908"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["sqrtpi(-1)", r#"sqrtpi("x")"#, "sqrtpi()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_cotangent_works() {
        for (source, expected) in [
            (r#"imcot("1+0i")"#, "0.6420926159343308"),
            (r#"imcot("0+1i")"#, "-1.3130352854993315i"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"imcot("0+0i")"#, r#"imcot("abc")"#, "imcot(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_cosecant_and_secant_work() {
        for (source, expected) in [
            (r#"imcsc("1+0i")"#, "1.1883951057781212"),
            (r#"imsec("1+0i")"#, "1.8508157176809252"),
            (r#"imsec("0+0i")"#, "1"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"imcsc("0+0i")"#, r#"imcsc("abc")"#, "imsec(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_trigonometric_functions_work() {
        for (source, expected) in [
            (r#"imsin("0+0i")"#, "0"),
            (r#"imcos("0+0i")"#, "1"),
            (r#"imtan("0+0i")"#, "0"),
            (r#"imsin("1+0i")"#, "0.8414709848078965"),
            (r#"imcos("1+0i")"#, "0.5403023058681398"),
            (r#"imtan("1+0i")"#, "1.557407724654902"),
            (r#"imsin("0+1i")"#, "1.1752011936438014i"),
            (r#"imcos("0+1i")"#, "1.5430806348152437"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"imsin("abc")"#, "imcos()", "imtan(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn reciprocal_hyperbolic_functions_work() {
        for (source, expected) in [
            ("csch(1)", "0.850918"),
            ("sech(1)", "0.648054"),
            ("coth(1)", "1.313035"),
            ("sech(0)", "1"),
            ("csch(-1)", "-0.850918"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["csch(0)", "coth(0)", r#"sech("x")"#, "csch(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_hyperbolic_functions_work() {
        for (source, expected) in [
            (r#"imsinh("0+0i")"#, "0"),
            (r#"imcosh("0+0i")"#, "1"),
            (r#"imsinh("1+0i")"#, "1.1752011936438014"),
            (r#"imcosh("1+0i")"#, "1.5430806348152437"),
            (r#"imsinh("0+1i")"#, "0.8414709848078965i"),
            (r#"imcosh("0+1i")"#, "0.5403023058681398"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"imsinh("abc")"#, "imcosh()", "imsinh(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn degrees_and_radians_convert() {
        for (source, expected) in [
            ("degrees(0)", "0"),
            ("degrees(1)", "57.29578"),
            ("radians(0)", "0"),
            ("radians(90)", "1.570796"),
            ("radians(180)", "3.141593"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"degrees("x")"#, "radians()", "degrees(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn iso_ceiling_rounds_up() {
        for (source, expected) in [
            ("iso.ceiling(4.3)", "5"),
            ("iso.ceiling(-4.3)", "-4"),
            ("iso.ceiling(4.3, 2)", "6"),
            ("iso.ceiling(-4.3, 2)", "-4"),
            ("iso.ceiling(4.3, 0)", "0"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "iso.ceiling(1, 2, 3)",
            r#"iso.ceiling("x")"#,
            "iso.ceiling()",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn inverse_hyperbolic_functions_work() {
        for (source, expected) in [
            ("asinh(0)", "0"),
            ("asinh(1)", "0.881374"),
            ("acosh(1)", "0"),
            ("acosh(2)", "1.316958"),
            ("atanh(0)", "0"),
            ("atanh(0.5)", "0.549306"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["acosh(0.5)", "atanh(1)", "atanh(-1)", r#"asinh("x")"#] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn hyperbolic_functions_work() {
        for (source, expected) in [
            ("sinh(0)", "0"),
            ("cosh(0)", "1"),
            ("tanh(0)", "0"),
            ("sinh(1)", "1.175201"),
            ("cosh(1)", "1.543081"),
            ("tanh(1)", "0.761594"),
            ("sinh(-1)", "-1.175201"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"sinh("x")"#, "cosh()", "tanh(1, 2)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn cot_and_acot_work() {
        for (source, expected) in [
            ("cot(1)", "0.642093"),
            ("acot(0)", "1.570796"),
            ("acot(1)", "0.785398"),
            ("acot(-1)", "2.356194"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["cot(0)", r#"cot("x")"#, "acot()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn combina_counts_repetition_combinations() {
        for (source, expected) in [
            ("combina(4, 3)", "20"),
            ("combina(3, 2)", "6"),
            ("combina(0, 0)", "1"),
            ("combina(0, 1)", "0"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "combina(-1, 2)",
            "combina(2.5, 2)",
            "combina(86, 86)",
            r#"combina("x", 2)"#,
            "combina(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn gamma_matches_factorials() {
        for (source, expected) in [
            ("gamma(1)", "1"),
            ("gamma(2)", "1"),
            ("gamma(5)", "24"),
            ("gamma(0.5)", "1.772454"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "gamma(0)",
            "gamma(-1)",
            "gamma(-0.5)",
            r#"gamma("x")"#,
            "gamma()",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn erf_and_erfc_approximate_error_functions() {
        for (source, expected) in [
            ("erf(0)", 0.0),
            ("erf(1)", 0.8427007929497148),
            ("erfc(1)", 0.15729920705028516),
            ("erf(1, 2)", 0.15262147206923793),
            ("erfc(0)", 1.0),
        ] {
            let formula = Formula::parse(source).unwrap();
            let value = formula
                .eval_at(&HashMap::new(), &Vec::new(), &Refs::default(), 1)
                .number()
                .unwrap();
            assert!((value - expected).abs() < 1e-6, "{source}: {value}");
        }
        for source in ["erfc(1, 2)", r#"erf("x")"#, "erf()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn gestep_compares_threshold() {
        for (source, expected) in [
            ("gestep(5, 4)", "1"),
            ("gestep(4, 5)", "0"),
            ("gestep(0)", "1"),
            ("gestep(-1)", "0"),
            ("gestep(4, 4)", "1"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["gestep(1, 2, 3)", r#"gestep("x")"#, "gestep()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn delta_compares_numbers() {
        for (source, expected) in [
            ("delta(5, 5)", "1"),
            ("delta(5, 4)", "0"),
            ("delta(0)", "1"),
            ("delta(0, 1)", "0"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["delta(1, 2, 3)", r#"delta("x")"#, "delta()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_base_two_log_works() {
        for (source, expected) in [
            (r#"imlog2("3+4i")"#, "2.321928094887362+1.3378042124509761i"),
            (r#"imlog2("1+0i")"#, "0"),
            (r#"imlog2("0+1i")"#, "2.266180070913597i"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"imlog2("0+0i")"#, r#"imlog2("abc")"#, "imlog2()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_natural_log_works() {
        for (source, expected) in [
            (r#"imln("3+4i")"#, "1.6094379124341003+0.9272952180016122i"),
            (r#"imln("1+0i")"#, "0"),
            (r#"imln("0+1i")"#, "1.5707963267948966i"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"imln("0+0i")"#, r#"imln("abc")"#, "imln()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_power_works() {
        for (source, expected) in [
            (r#"impower("0+1i", 2)"#, "-1"),
            (r#"impower("1+1i", 2)"#, "2i"),
            (r#"impower("2+0i", 0.5)"#, "1.4142135623730951"),
            (r#"impower("0+0i", 0)"#, "1"),
            (r#"impower("0+0i", 2)"#, "0"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"impower("0+0i", -1)"#,
            r#"impower("abc", 2)"#,
            "impower(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_square_root_work() {
        for (source, expected) in [
            (r#"imsqrt("4+0i")"#, "2"),
            (r#"imsqrt("9+0i")"#, "3"),
            (r#"imsqrt("0+0i")"#, "0"),
            (r#"imsqrt("-4+0i")"#, "2i"),
            (r#"imsqrt("0+4i")"#, "1.4142135623730951+1.414213562373095i"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"imsqrt("abc")"#, "imsqrt()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_exponential_and_log_work() {
        for (source, expected) in [
            (r#"imexp("0")"#, "1"),
            (r#"imexp("1+0i")"#, "2.718281828459045"),
            (r#"imexp("0+1i")"#, "0.5403023058681398+0.8414709848078965i"),
            (
                r#"imlog10("3+4i")"#,
                "0.6989700043360189+0.4027191962733731i",
            ),
            (r#"imlog10("1+0i")"#, "0"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"imlog10("0+0i")"#, r#"imexp("abc")"#, "imlog10()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_conjugate_and_argument_work() {
        for (source, expected) in [
            (r#"imconjugate("3+4i")"#, "3-4i"),
            (r#"imconjugate("3-4i")"#, "3+4i"),
            (r#"imconjugate("4i")"#, "-4i"),
            (r#"imargument("1+1i")"#, "0.785398"),
            (r#"imargument("3+4i")"#, "0.927295"),
            (r#"imargument("0+1i")"#, "1.570796"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"imargument("0+0i")"#,
            r#"imconjugate("abc")"#,
            "imargument()",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_product_and_division_work() {
        for (source, expected) in [
            (r#"improduct("3+4i", "1+2i")"#, "-5+10i"),
            (r#"imdiv("3+4i", "1+2i")"#, "2.2-0.4i"),
            (r#"imdiv("3+4i", "1-2i")"#, "-1+2i"),
            (r#"improduct("3+4j", "1+2j")"#, "-5+10j"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            r#"imdiv("3+4i", "0+0i")"#,
            r#"improduct("3+4i", "abc")"#,
            "imdiv(1)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_sum_and_difference_work() {
        for (source, expected) in [
            (r#"imsum("3+4i", "1+2i")"#, "4+6i"),
            (r#"imsub("3+4i", "1+2i")"#, "2+2i"),
            (r#"imsum("3+4i", "3-4i")"#, "6"),
            (r#"imsum("3+4i", "-3+4i")"#, "8i"),
            (r#"imsum("3+4j", "1+2j")"#, "4+6j"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"imsum("3+4i", "abc")"#, "imsub(1)"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn steyx_measures_regression_error() {
        let columns = vec![
            "y1".to_string(),
            "y2".to_string(),
            "y3".to_string(),
            "x1".to_string(),
            "x2".to_string(),
            "x3".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("y1", "2"),
            ("y2", "4"),
            ("y3", "6"),
            ("x1", "1"),
            ("x2", "2"),
            ("x3", "3"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("steyx(A2:C2, D2:F2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0"
        );
        cells.insert("y2".to_string(), "3".to_string());
        let formula = Formula::parse("steyx(A2:C2, D2:F2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0.816497"
        );
        for source in ["steyx(A2:C2, D2:E2)", "steyx(A2:B2, D2:E2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
        for name in ["x1", "x2", "x3"] {
            cells.insert(name.to_string(), "1".to_string());
        }
        let formula = Formula::parse("steyx(A2:C2, D2:F2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn gammaln_matches_log_gamma() {
        for (source, expected) in [
            ("gammaln(1)", "0"),
            ("gammaln(2)", "0"),
            ("gammaln(5)", "3.178054"),
            ("gammaln(0.5)", "0.572365"),
            ("gammaln.precise(1)", "0"),
            ("gammaln.precise(5)", "3.178054"),
            ("gammaln.precise(0.5)", "0.572365"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "gammaln(0)",
            "gammaln(-1)",
            r#"gammaln("x")"#,
            "gammaln()",
            "gammaln.precise(0)",
            "gammaln.precise(-1)",
            "gammaln.precise()",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn complex_numbers_round_trip() {
        for (source, expected) in [
            ("complex(3, 4)", "3+4i"),
            ("complex(3, -4)", "3-4i"),
            ("complex(0, 4)", "4i"),
            ("complex(3, 1)", "3+i"),
            ("complex(3, -1)", "3-i"),
            ("complex(0, 0)", "0"),
            (r#"complex(3, 4, "j")"#, "3+4j"),
            (r#"imreal("3+4i")"#, "3"),
            (r#"imaginary("3+4i")"#, "4"),
            (r#"imabs("3+4i")"#, "5"),
            (r#"imaginary("4i")"#, "4"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [r#"complex(3, 4, "x")"#, r#"imreal("abc")"#, "imabs()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn multinomial_counts_arrangements() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3")] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("multinomial(A2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "60"
        );
        cells.insert("a".to_string(), "2".to_string());
        cells.insert("b".to_string(), "2".to_string());
        cells.insert("c".to_string(), String::new());
        let formula = Formula::parse("multinomial(A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "6"
        );
        cells.insert("a".to_string(), "-1".to_string());
        let formula = Formula::parse("multinomial(A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        cells.insert("a".to_string(), "171".to_string());
        cells.insert("b".to_string(), String::new());
        let formula = Formula::parse("multinomial(A2:A2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        let columns = vec!["a".to_string(), "b".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), String::new());
        cells.insert("b".to_string(), String::new());
        let formula = Formula::parse("multinomial(A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn factdouble_computes_double_factorials() {
        for (source, expected) in [
            ("factdouble(0)", "1"),
            ("factdouble(1)", "1"),
            ("factdouble(4)", "8"),
            ("factdouble(5)", "15"),
            ("factdouble(6)", "48"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "factdouble(-2)",
            "factdouble(2.5)",
            "factdouble(301)",
            "factdouble()",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn roman_and_arabic_round_trip() {
        for (source, expected) in [
            ("roman(4)", "IV"),
            ("roman(1999)", "MCMXCIX"),
            ("roman(0)", "0"),
            ("roman(4, 4)", "IV"),
            (r#"arabic("MCMXCIX")"#, "1999"),
            (r#"arabic("iv")"#, "4"),
            (r#"arabic("")"#, "0"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "roman(4000)",
            "roman(4, 5)",
            "roman(4.5)",
            r#"arabic("ABC")"#,
            "arabic()",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn encodeurl_percent_encodes_text() {
        for (source, expected) in [
            (r#"encodeurl("Hello World")"#, "Hello%20World"),
            (r#"encodeurl("a/b?c=d&e")"#, "a%2Fb%3Fc%3Dd%26e"),
            (r#"encodeurl("a-_.~")"#, "a-_.~"),
            (r#"encodeurl("中文")"#, "%E4%B8%AD%E6%96%87"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        assert_eq!(eval_str("encodeurl()", &[]), "#VALUE!");
        assert_eq!(eval_str("encodeurl(1, 2)", &[]), "#VALUE!");
    }

    #[test]
    fn dollar_fraction_round_trips() {
        for (source, expected) in [
            ("dollarde(1.02, 16)", "1.125"),
            ("dollarfr(1.125, 16)", "1.02"),
            ("dollarde(-1.02, 16)", "-1.125"),
            ("dollarfr(-1.125, 16)", "-1.02"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in [
            "dollarde(1.02, 0)",
            "dollarfr(1.125, -16)",
            "dollarde(1.02, 2.5)",
            "dollarde(1.02)",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn maxa_and_mina_treat_text_as_zero() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), "1".to_string());
        cells.insert("b".to_string(), "2".to_string());
        cells.insert("c".to_string(), "text".to_string());
        for (source, expected) in [("maxa(A2:C2)", "2"), ("mina(A2:C2)", "0")] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        cells.insert("a".to_string(), "TRUE".to_string());
        cells.insert("b".to_string(), "FALSE".to_string());
        cells.insert("c".to_string(), String::new());
        for (source, expected) in [("maxa(A2:C2)", "1"), ("mina(A2:C2)", "0")] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        let columns = vec!["a".to_string(), "b".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), String::new());
        cells.insert("b".to_string(), String::new());
        let formula = Formula::parse("maxa(A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn averagea_treats_text_and_booleans_as_numbers() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), "1".to_string());
        cells.insert("b".to_string(), "2".to_string());
        cells.insert("c".to_string(), "3".to_string());
        cells.insert("d".to_string(), "text".to_string());
        let formula = Formula::parse("averagea(A2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1.5"
        );
        cells.insert("a".to_string(), "1".to_string());
        cells.insert("b".to_string(), "TRUE".to_string());
        cells.insert("c".to_string(), "FALSE".to_string());
        cells.insert("d".to_string(), String::new());
        let formula = Formula::parse("averagea(A2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0.666667"
        );
        let formula = Formula::parse("averagea(A2:A2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1"
        );
        let columns = vec!["a".to_string(), "b".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), String::new());
        cells.insert("b".to_string(), String::new());
        let formula = Formula::parse("averagea(A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn population_text_statistics_work() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), "1".to_string());
        cells.insert("b".to_string(), "2".to_string());
        cells.insert("c".to_string(), "3".to_string());
        cells.insert("d".to_string(), "text".to_string());
        for (source, expected) in [("stdevpa(A2:D2)", "1.118034"), ("varpa(A2:D2)", "1.25")] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        let formula = Formula::parse("stdevpa(A2:A2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0"
        );
        let columns = vec!["a".to_string(), "b".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), String::new());
        cells.insert("b".to_string(), String::new());
        let formula = Formula::parse("varpa(A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn paired_sum_functions_work() {
        let columns = vec![
            "x1".to_string(),
            "x2".to_string(),
            "x3".to_string(),
            "y1".to_string(),
            "y2".to_string(),
            "y3".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("x1", "1"),
            ("x2", "2"),
            ("x3", "3"),
            ("y1", "4"),
            ("y2", "5"),
            ("y3", "6"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("sumxmy2(A2:C2, D2:F2)", "27"),
            ("sumx2my2(A2:C2, D2:F2)", "-63"),
            ("sumx2py2(A2:C2, D2:F2)", "91"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in ["sumxmy2(A2:C2, D2:E2)", "sumx2py2(A2:C2)"] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
    }

    #[test]
    fn stdeva_and_vara_treat_text_as_zero() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), "1".to_string());
        cells.insert("b".to_string(), "2".to_string());
        cells.insert("c".to_string(), "3".to_string());
        cells.insert("d".to_string(), "text".to_string());
        for (source, expected) in [("stdeva(A2:D2)", "1.290994"), ("vara(A2:D2)", "1.666667")] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        let formula = Formula::parse("stdeva(A2:A2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        let formula = Formula::parse("stdeva(A2:D2, 1)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn seriessum_sums_powers() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), "1".to_string());
        cells.insert("b".to_string(), "1".to_string());
        cells.insert("c".to_string(), "1".to_string());
        for (source, expected) in [
            ("seriessum(2, 1, 1, A2:C2)", "14"),
            ("seriessum(2, 0, 1, A2:C2)", "7"),
            ("seriessum(1, 1, 1, A2:C2)", "3"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        cells.insert("b".to_string(), "x".to_string());
        let formula = Formula::parse("seriessum(2, 1, 1, A2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        let columns = vec!["a".to_string(), "b".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), String::new());
        cells.insert("b".to_string(), String::new());
        let formula = Formula::parse("seriessum(2, 1, 1, A2:B2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        assert_eq!(eval_str("seriessum(2, 1, 1)", &[]), "#VALUE!");
    }

    #[test]
    fn fisher_transform_round_trips() {
        assert_eq!(eval_str("fisher(0)", &[]), "0");
        assert_eq!(eval_str("fisher(0.5)", &[]), "0.549306");
        assert_eq!(eval_str("fisherinv(0)", &[]), "0");
        assert_eq!(eval_str("fisherinv(0.549306)", &[]), "0.5");
        assert_eq!(eval_str("fisherinv(fisher(0.5))", &[]), "0.5");
        for source in ["fisher(1)", "fisher(-1)", "fisher(2)", r#"fisher("x")"#] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn mirr_discounts_and_reinvests_cash_flows() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), "-100".to_string());
        cells.insert("b".to_string(), "50".to_string());
        cells.insert("c".to_string(), "60".to_string());
        let formula = Formula::parse("mirr(A2:C2, 0.1, 0.05)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0.06066"
        );
        cells.insert("b".to_string(), "110".to_string());
        cells.insert("c".to_string(), String::new());
        let columns = vec!["a".to_string(), "b".to_string()];
        let formula = Formula::parse("mirr(A2:B2, 0.1, 0.1)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "0.1"
        );
        cells.insert("a".to_string(), "100".to_string());
        cells.insert("b".to_string(), "20".to_string());
        for source in [
            "mirr(A2:B2, 0.1, 0.1)",
            "mirr(A2:B2, -1, 0.1)",
            "mirr(A2:B2, 0.1, -1)",
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "{source}"
            );
        }
    }

    #[test]
    fn math_rounding_functions_work() {
        for (source, expected) in [
            ("ceiling.math(4.3)", "5"),
            ("floor.math(4.3)", "4"),
            ("ceiling.math(4.3, 2)", "6"),
            ("floor.math(4.3, 2)", "4"),
            ("ceiling.math(-4.3, 2)", "-4"),
            ("ceiling.math(-4.3, 2, 1)", "-6"),
            ("floor.math(-4.3, 2)", "-6"),
            ("floor.math(-4.3, 2, 1)", "-4"),
            ("ceiling.math(5, 0)", "0"),
            ("floor.math(5, 0)", "0"),
        ] {
            assert_eq!(eval_str(source, &[]), expected, "{source}");
        }
        for source in ["ceiling.math(4.3, 2, \"x\")", "floor.math()"] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn iseven_and_isodd_work() {
        assert_eq!(eval_str("iseven(2)", &[]), "TRUE");
        assert_eq!(eval_str("iseven(3)", &[]), "FALSE");
        assert_eq!(eval_str("isodd(3)", &[]), "TRUE");
        assert_eq!(eval_str("isodd(-1)", &[]), "TRUE");
        assert_eq!(eval_str("iseven(-2)", &[]), "TRUE");
        for source in [
            r#"iseven(2.5)"#,
            r#"isodd(2.5)"#,
            r#"iseven("x")"#,
            "isodd()",
        ] {
            assert_eq!(eval_str(source, &[]), "#VALUE!", "{source}");
        }
    }

    #[test]
    fn avg_and_exclusive_ranking_variants_work() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "2"), ("d", "4")] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("rank.avg(2, A2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2.5"
        );
        cells.insert("c".to_string(), "3".to_string());
        let exc = Formula::parse("percentrank.exc(A2:D2, 2)").unwrap();
        assert_eq!(
            exc.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "0.4"
        );
        let quart = Formula::parse("quartile.exc(A2:D2, 1)").unwrap();
        assert_eq!(
            quart
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1.25"
        );
    }

    #[test]
    fn percentrank_and_percentile_aliases_read_same_row_ranges() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("percentrank(A2:D2, 2)", "0.333333"),
            ("percentrank(A2:D2, 2.5)", "0.5"),
            ("percentrank(A2:D2, 2, 2)", "0.33"),
            ("percentrank.inc(A2:D2, 2)", "0.333333"),
            ("percentrank.inc(A2:D2, 2.5)", "0.5"),
            ("percentrank.inc(A2:D2, 2, 2)", "0.33"),
            ("percentile.inc(A2:D2, 0.5)", "2.5"),
            ("percentile.exc(A2:D2, 0.5)", "2.5"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        let boundary = Formula::parse("percentile.exc(A2:D2, 0)").unwrap();
        assert_eq!(
            boundary
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        let outside = Formula::parse("percentrank(A2:D2, 0)").unwrap();
        assert_eq!(
            outside
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#N/A"
        );
    }

    #[test]
    fn geomean_and_harmmean_read_same_row_ranges() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "4"), ("d", "8")] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("geomean(A2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2.828427"
        );
        let formula = Formula::parse("harmmean(A2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2.133333"
        );
        cells.insert("a".to_string(), "0".to_string());
        let formula = Formula::parse("geomean(A2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn quartile_and_trimmean_read_same_row_ranges() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (quart, expected) in [("0", "1"), ("2", "2.5"), ("4", "4")] {
            for name in ["quartile", "quartile.inc"] {
                let formula = Formula::parse(&format!("{name}(A2:D2, {quart})")).unwrap();
                assert_eq!(
                    formula
                        .eval_at(&cells, &columns, &Refs::default(), 1)
                        .render(),
                    expected,
                    "{name} quart={quart}"
                );
            }
        }
        let formula = Formula::parse("trimmean(A2:D2, 0.5)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2.5"
        );
        let formula = Formula::parse("quartile(A2:D2, 5)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn percentile_interpolates_a_same_row_range() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (p, expected) in [("0", "1"), ("0.5", "2.5"), ("0.25", "1.75"), ("1", "4")] {
            let formula = Formula::parse(&format!("percentile(A2:D2, {p})")).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "p={p}"
            );
        }
        for p in ["-0.1", "1.1"] {
            let formula = Formula::parse(&format!("percentile(A2:D2, {p})")).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                "#VALUE!",
                "p={p} is outside 0..1"
            );
        }
        cells.insert("c".to_string(), "n/a".to_string());
        let formula = Formula::parse("percentile(A2:D2, 0.5)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2",
            "one text cell leaves 1, 2, 4"
        );
    }

    #[test]
    fn sumsq_and_product_read_same_row_ranges() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut cells = HashMap::new();
        for (name, value) in [("a", "10"), ("b", "3"), ("c", "7")] {
            cells.insert(name.to_string(), value.to_string());
        }
        assert_eq!(
            Formula::parse("sumsq(A2:C2)")
                .unwrap()
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "158"
        );
        assert_eq!(
            Formula::parse("product(A2:C2)")
                .unwrap()
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "210"
        );
        cells.insert("b".to_string(), "n/a".to_string());
        assert_eq!(
            Formula::parse("sumsq(A2:C2)")
                .unwrap()
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "149",
            "text is skipped"
        );
        assert_eq!(
            Formula::parse("product(A2:C2)")
                .unwrap()
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "70"
        );
        cells.insert("a".to_string(), "n/a".to_string());
        assert_eq!(
            Formula::parse("sumsq(A2:B2)")
                .unwrap()
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!",
            "no numeric cell has no sum of squares"
        );
    }

    #[test]
    fn large_and_small_read_a_same_row_range() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), "10".to_string());
        cells.insert("b".to_string(), "3".to_string());
        cells.insert("c".to_string(), "7".to_string());
        for (formula, expected) in [
            ("large(A2:C2, 1)", "10"),
            ("large(A2:C2, 2)", "7"),
            ("small(A2:C2, 1)", "3"),
            ("small(A2:C2, 2)", "7"),
        ] {
            assert_eq!(
                Formula::parse(formula)
                    .unwrap()
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{formula}"
            );
        }
        assert_eq!(
            Formula::parse("large(A2:C2, 0)")
                .unwrap()
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!",
            "k is 1-based"
        );
        assert_eq!(
            Formula::parse("small(A2:C2, 4)")
                .unwrap()
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!",
            "k past the end"
        );
        cells.insert("c".to_string(), "n/a".to_string());
        assert_eq!(
            Formula::parse("large(A2:C2, 2)")
                .unwrap()
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "3",
            "text is ignored, leaving two numbers"
        );
    }

    #[test]
    fn median_reads_a_same_row_range_and_refuses_a_whole_column() {
        let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut cells = HashMap::new();
        cells.insert("a".to_string(), "10".to_string());
        cells.insert("b".to_string(), "3".to_string());
        cells.insert("c".to_string(), "7".to_string());
        let even = Formula::parse("median(A2:B2)").unwrap();
        assert_eq!(
            even.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "6.5"
        );
        let odd = Formula::parse("median(A2:C2)").unwrap();
        assert_eq!(
            odd.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "7"
        );
        cells.insert("c".to_string(), "n/a".to_string());
        assert_eq!(
            odd.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "6.5",
            "text is ignored, like the other aggregate range functions"
        );
        let column = Formula::parse("median(a)").unwrap();
        assert_eq!(
            column
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!",
            "a whole-column median would have to keep every value"
        );
    }

    #[test]
    fn sumproduct_multiplies_two_same_row_ranges() {
        let columns = vec![
            "id".to_string(),
            "qty".to_string(),
            "price".to_string(),
            "weight".to_string(),
            "fee".to_string(),
            "f".to_string(),
            "g".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "7".to_string());
        cells.insert("qty".to_string(), "10".to_string());
        cells.insert("price".to_string(), "3".to_string());
        cells.insert("weight".to_string(), "2".to_string());
        cells.insert("fee".to_string(), "4".to_string());
        cells.insert("f".to_string(), "0.5".to_string());
        cells.insert("g".to_string(), "1".to_string());
        let formula = Formula::parse("sumproduct(B2:C2, D2:E2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "32",
            "10*2 + 3*4"
        );
        // Text is treated as 0, like a spreadsheet, not as an error.
        cells.insert("price".to_string(), "n/a".to_string());
        let formula = Formula::parse("sumproduct(B2:C2, D2:E2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "20",
            "10*2 + 0*4"
        );
        cells.insert("price".to_string(), "3".to_string());
        let three = Formula::parse("sumproduct(B2:C2, D2:E2, F2:G2)").unwrap();
        assert_eq!(
            three
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "22",
            "10*2*0.5 + 3*4*1"
        );
        let ragged = Formula::parse("sumproduct(B2:C2, F2:F2)").unwrap();
        assert_eq!(
            ragged
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!",
            "ranges of different widths have no pairwise meaning"
        );
    }

    #[test]
    fn workday_skips_weekends_and_accepts_negative_days() {
        assert_eq!(eval_str(r#"workday("2026-01-02", 1)"#, &[]), "2026-01-05");
        assert_eq!(eval_str(r#"workday("2026-01-05", -1)"#, &[]), "2026-01-02");
        assert_eq!(
            eval_str(r#"workday("2026-01-01", 0)"#, &[]),
            "2026-01-01",
            "zero days keeps the same date"
        );
        assert_eq!(eval_str(r#"workday("not-a-date", 1)"#, &[]), "#VALUE!");
    }

    #[test]
    fn networkdays_and_workday_accept_a_holiday_range() {
        let columns = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("a", "2024-01-01"),
            ("b", "2024-01-02"),
            ("c", "2024-01-03"),
            ("d", "2024-01-05"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        let formula = Formula::parse("networkdays(A2, D2, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "3"
        );
        let formula = Formula::parse("workday(A2, 3, B2:C2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2024-01-08"
        );
    }

    #[test]
    fn networkdays_and_workday_skip_explicit_holidays() {
        assert_eq!(
            eval_str(
                r#"networkdays("2024-01-01", "2024-01-05", "2024-01-03")"#,
                &[]
            ),
            "4"
        );
        assert_eq!(
            eval_str(r#"workday("2024-01-01", 3, "2024-01-03")"#, &[]),
            "2024-01-05"
        );
        assert_eq!(
            eval_str(r#"networkdays("2024-01-01", "2024-01-05", "bad")"#, &[]),
            "#VALUE!"
        );
        assert_eq!(
            eval_str(r#"workday("2024-01-05", -2, "2024-01-03")"#, &[]),
            "2024-01-02"
        );
    }

    #[test]
    fn networkdays_counts_weekdays_both_ends_included() {
        assert_eq!(
            eval_str(r#"networkdays("2026-01-01", "2026-01-04")"#, &[]),
            "2"
        );
        assert_eq!(
            eval_str(r#"networkdays("2026-01-04", "2026-01-05")"#, &[]),
            "1",
            "Sunday is skipped"
        );
        assert_eq!(
            eval_str(r#"networkdays("2026-01-01", "2026-01-08")"#, &[]),
            "6",
            "five weekday plus the following Thursday"
        );
        assert_eq!(
            eval_str(r#"networkdays("2026-01-05", "2026-01-01")"#, &[]),
            "#VALUE!",
            "an end before the start has no business-day count"
        );
    }

    #[test]
    fn weekday_and_weeknum_return_types_work() {
        assert_eq!(eval_str(r#"weekday("2024-01-01")"#, &[]), "2");
        assert_eq!(eval_str(r#"weekday("2024-01-01", 2)"#, &[]), "1");
        assert_eq!(eval_str(r#"weekday("2024-01-01", 3)"#, &[]), "0");
        assert_eq!(eval_str(r#"weeknum("2024-01-01")"#, &[]), "1");
        assert_eq!(eval_str(r#"weeknum("2024-01-01", 2)"#, &[]), "1");
        assert_eq!(eval_str(r#"weeknum("2024-01-01", 21)"#, &[]), "1");
        assert_eq!(eval_str(r#"weekday("2024-01-01", 9)"#, &[]), "#VALUE!");
        assert_eq!(eval_str(r#"weeknum("2024-01-01", 9)"#, &[]), "#VALUE!");
    }

    #[test]
    fn weeknum_uses_sunday_as_the_first_day() {
        assert_eq!(eval_str(r#"weeknum("2026-01-01")"#, &[]), "1");
        assert_eq!(
            eval_str(r#"weeknum("2026-01-03")"#, &[]),
            "1",
            "Saturday Jan 3 closes the week that contains Jan 1"
        );
        assert_eq!(
            eval_str(r#"weeknum("2026-01-04")"#, &[]),
            "2",
            "the Sunday after Jan 1 starts a new week"
        );
        assert_eq!(
            eval_str(r#"weeknum("2026-01-05")"#, &[]),
            "2",
            "Monday stays in the week the Sunday opened"
        );
        assert_eq!(eval_str(r#"weeknum("not-a-date")"#, &[]), "#VALUE!");
    }

    #[test]
    fn date_arithmetic_stays_in_iso_text() {
        assert_eq!(
            eval_str(r#"edate("2024-01-31", 1)"#, &[]),
            "2024-02-29",
            "month-end clamps to the shorter leap February"
        );
        assert_eq!(eval_str(r#"edate("2024-01-31", -2)"#, &[]), "2023-11-30");
        assert_eq!(eval_str(r#"eomonth("2024-02-10", 0)"#, &[]), "2024-02-29");
        assert_eq!(eval_str(r#"weekday("2026-11-23")"#, &[]), "2", "Monday = 2");
        assert_eq!(eval_str(r#"weekday("2026-11-22")"#, &[]), "1", "Sunday = 1");
        assert_eq!(eval_str(r#"days("2026-11-23", "2026-11-20")"#, &[]), "3");
        assert_eq!(
            eval_str(r#"days("2023-01-01", "2022-12-31")"#, &[]),
            "1",
            "spans a year boundary"
        );
        assert_eq!(eval_str(r#"edate("not-a-date", 1)"#, &[]), "#VALUE!");
    }

    #[test]
    fn countblank_reads_a_same_row_range() {
        let columns = vec!["id".to_string(), "amount".to_string(), "qty".to_string()];
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "7".to_string());
        cells.insert("amount".to_string(), "10".to_string());
        cells.insert("qty".to_string(), String::new());
        let blank = Formula::parse("countblank(B2:C2)").unwrap();
        assert_eq!(
            blank
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1"
        );
    }

    #[test]
    fn hlookup_and_lookup_read_single_row_tables() {
        let columns = vec![
            "id".to_string(),
            "amount".to_string(),
            "qty".to_string(),
            "note".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("id", "7"),
            ("amount", "10"),
            ("qty", "3"),
            ("note", "plain"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("hlookup(10, B2:D2, 1, false())", "10"),
            ("hlookup(\"plain\", B2:D2, 1, false())", "plain"),
            ("hlookup(\"pl*\", B2:D2, 1, false())", "plain"),
            ("hlookup(\"?lain\", B2:D2, 1, false())", "plain"),
            ("hlookup(5, B2:D2, 1)", "3"),
            ("lookup(10, B2:B2, D2:D2)", "plain"),
            ("lookup(3, C2:C2, D2:D2)", "plain"),
            ("lookup(5, C2:C2, D2:D2)", "plain"),
            ("lookup(10, B2:B2)", "10"),
            ("lookup(\"plao\", B2:D2)", "plain"),
            ("lookup(\"plao\", B2:D2, B2:D2)", "plain"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in [
            "hlookup(1, B2:D2, 1)",
            "hlookup(10, B2:D2, 2, false())",
            "hlookup(\"nope*\", B2:D2, 1, false())",
            "lookup(1, B2:B2, D2:D2)",
            "lookup(3, B2:C2, D2:D2)",
        ] {
            let formula = Formula::parse(source).unwrap();
            let expected =
                if source.starts_with("hlookup(10") || source == "lookup(3, B2:C2, D2:D2)" {
                    "#VALUE!"
                } else {
                    "#N/A"
                };
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn index_match_and_xmatch_read_a_same_row_range() {
        let columns = vec![
            "id".to_string(),
            "amount".to_string(),
            "qty".to_string(),
            "note".to_string(),
        ];
        let mut cells = HashMap::new();
        for (name, value) in [
            ("id", "7"),
            ("amount", "10"),
            ("qty", "3"),
            ("note", "plain"),
        ] {
            cells.insert(name.to_string(), value.to_string());
        }
        for (source, expected) in [
            ("index(B2:D2, 2)", "3"),
            ("index(B2:D2, 1, 3)", "plain"),
            ("match(10, B2:D2, 0)", "1"),
            ("match(\"plain\", B2:D2, 0)", "3"),
            ("match(\"PLAIN\", B2:D2, 0)", "3"),
            ("match(\"pl*\", B2:D2, 0)", "3"),
            ("match(\"plao\", B2:D2, 1)", "3"),
            ("match(\"plai\", B2:D2, -1)", "3"),
            ("match(\"zzz\", B2:D2, 1)", "3"),
            ("match(\"*\", B2:D2, 0)", "1"),
            ("match(\"?lain\", B2:D2, 0)", "3"),
            ("xmatch(3, B2:D2, 0)", "2"),
            ("xmatch(3, B2:D2, 0, 1)", "2"),
            ("xmatch(\"pl*\", B2:D2, 2)", "3"),
            ("match(5, B2:D2, 1)", "2"),
            ("xmatch(5, B2:D2, 1)", "1"),
        ] {
            let formula = Formula::parse(source).unwrap();
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
        for source in [
            "index(B2:D2, 4)",
            "match(99, B2:D2, 0)",
            "match(10, A3:A3, 0)",
            "match(10, B2:D2, 2)",
            "match(\"nope*\", B2:D2, 0)",
            "xmatch(\"nope*\", B2:D2, 2)",
        ] {
            let formula = Formula::parse(source).unwrap();
            let expected = if source.starts_with("match(99") || source.contains("nope") {
                "#N/A"
            } else {
                "#VALUE!"
            };
            assert_eq!(
                formula
                    .eval_at(&cells, &columns, &Refs::default(), 1)
                    .render(),
                expected,
                "{source}"
            );
        }
    }

    #[test]
    fn countif_and_sumif_read_a_same_row_range() {
        let columns = vec![
            "id".to_string(),
            "amount".to_string(),
            "qty".to_string(),
            "note".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "7".to_string());
        cells.insert("amount".to_string(), "10".to_string());
        cells.insert("qty".to_string(), "3".to_string());
        cells.insert("note".to_string(), "plain".to_string());

        let count = Formula::parse(r#"countif(B2:C2, ">5")"#).unwrap();
        assert_eq!(
            count
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1"
        );
        let sum = Formula::parse(r#"sumif(B2:C2, ">5")"#).unwrap();
        assert_eq!(
            sum.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "10"
        );
        let avg = Formula::parse(r#"averageif(B2:C2, ">5")"#).unwrap();
        assert_eq!(
            avg.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "10"
        );
        // Text cells are not counted by an ordering criterion.
        let wider = Formula::parse(r#"countif(B2:D2, ">5")"#).unwrap();
        assert_eq!(
            wider
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1"
        );
        // Another row is still not resolvable.
        let next = Formula::parse(r#"countif(B3:C3, ">5")"#).unwrap();
        assert_eq!(
            next.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "#VALUE!"
        );
    }

    #[test]
    fn a_same_row_range_aggregates_between_columns() {
        let columns = vec![
            "id".to_string(),
            "amount".to_string(),
            "qty".to_string(),
            "note".to_string(),
        ];
        let mut cells = HashMap::new();
        cells.insert("id".to_string(), "7".to_string());
        cells.insert("amount".to_string(), "10".to_string());
        cells.insert("qty".to_string(), "3".to_string());
        cells.insert("note".to_string(), "plain".to_string());

        let sum = Formula::parse("sum(B2:C2)").unwrap();
        assert_eq!(
            sum.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "13"
        );
        let count = Formula::parse("count(B2:C2)").unwrap();
        assert_eq!(
            count
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "2"
        );
        let avg = Formula::parse("average(B2:C2)").unwrap();
        assert_eq!(
            avg.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "6.5"
        );

        // A non-numeric cell in the range is skipped, exactly like the column aggregates.
        let wider = Formula::parse("sum(B2:D2)").unwrap();
        assert_eq!(
            wider
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "13"
        );

        // Sample variance of 10 and 3 is 24.5; stdev is its square root.
        let range_var = Formula::parse("var(B2:C2)").unwrap();
        assert_eq!(
            range_var
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "24.5"
        );
        let range_sd = Formula::parse("stdev(B2:C2)").unwrap();
        let rendered = range_sd
            .eval_at(&cells, &columns, &Refs::default(), 1)
            .render();
        assert!(
            (rendered.parse::<f64>().unwrap() - 24.5_f64.sqrt()).abs() < 0.01,
            "stdev(range) = {rendered}"
        );

        let pop_var = Formula::parse("varp(B2:C2)").unwrap();
        assert_eq!(
            pop_var
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "12.25"
        );
        let pop_sd = Formula::parse("stdevp(B2:C2)").unwrap();
        assert_eq!(
            pop_sd
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "3.5"
        );

        // A range that needs another row cannot be served while streaming.
        let next = Formula::parse("sum(B3:C3)").unwrap();
        assert_eq!(
            next.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "#VALUE!"
        );

        // A bare range is not a value.
        let bare = Formula::parse("B2:C2").unwrap();
        assert_eq!(
            bare.eval_at(&cells, &columns, &Refs::default(), 1).render(),
            "#VALUE!"
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

    #[test]
    fn the_everyday_functions_behave_like_a_spreadsheet() {
        // Numeric
        assert_eq!(eval_str("sign(-3)", &[]), "-1");
        assert_eq!(eval_str("power(2, 10)", &[]), "1024");
        assert_eq!(eval_str("roundup(1.41, 1)", &[]), "1.5");
        assert_eq!(eval_str("rounddown(1.49, 1)", &[]), "1.4");
        assert_eq!(eval_str("roundup(-1.41, 1)", &[]), "-1.5");
        assert_eq!(eval_str("round(2.5)", &[]), "3");
        assert_eq!(eval_str("ln(1)", &[]), "0");
        assert_eq!(eval_str("log(100)", &[]), "2");
        assert_eq!(eval_str("log(8, 2)", &[]), "3");

        // Text
        assert_eq!(eval_str("exact('a', 'A')", &[]), "FALSE");
        assert_eq!(eval_str("exact('x', 'x')", &[]), "TRUE");
        // `city2` bare is parsed as an A1-style reference (column CITY, row 2), so a
        // column whose name ends in digits has to be bracketed.
        assert_eq!(
            eval_str(
                "exact([city], [city2])",
                &[("city", "CDMX"), ("city2", "CDMX")]
            ),
            "TRUE"
        );
        assert_eq!(eval_str("proper('hello world')", &[]), "Hello World");
        assert_eq!(eval_str(r#"proper("sally's shop")"#, &[]), "Sally'S Shop");
        assert_eq!(eval_str("proper('2-way street')", &[]), "2-Way Street");
        assert_eq!(eval_str("proper('76budget')", &[]), "76Budget");
        assert_eq!(eval_str("proper('')", &[]), "");
        assert_eq!(eval_str("proper('a', 2)", &[]), "#VALUE!");
        assert_eq!(eval_str("trim('  a   b  ')", &[]), "a b");
        assert_eq!(eval_str("trim('a  b  c')", &[]), "a b c");
        assert_eq!(eval_str("trim('   ')", &[]), "");
        assert_eq!(eval_str("trim('a', 'b')", &[]), "#VALUE!");
        assert_eq!(eval_str("trim(concat('a', char(9), 'b'))", &[]), "a\tb");
        assert_eq!(eval_str("rept('ab', 3)", &[]), "ababab");
        assert_eq!(eval_str("substitute('a-b-c', '-', '+')", &[]), "a+b+c");
        assert_eq!(eval_str("substitute('a-b-c', '-', '+', 2)", &[]), "a-b+c");
        assert_eq!(
            eval_str("substitute('a-b-c', '-', '+', 4)", &[]),
            "a-b-c",
            "an instance past the hit count leaves the text unchanged"
        );
        assert_eq!(eval_str("substitute('a-b-c', '-', '+', 0)", &[]), "#VALUE!");
        assert_eq!(
            eval_str("substitute('a-b-c', '-', '+', 1.5)", &[]),
            "#VALUE!"
        );
        assert_eq!(
            eval_str("substitute('a-b-c', '-', '+', 'x')", &[]),
            "#VALUE!"
        );
        assert_eq!(eval_str("replace('a-b-c', 3, 1, '+')", &[]), "a-+-c");
        assert_eq!(eval_str("replace('abcdef', 2, 3, 'XY')", &[]), "aXYef");
        assert_eq!(eval_str("replace('abc', 0, 1, 'x')", &[]), "#VALUE!");
        assert_eq!(eval_str("replace('abc', 2, -1, 'x')", &[]), "#VALUE!");
        assert_eq!(eval_str("replace('abc', 5, 1, 'x')", &[]), "abc");
        assert_eq!(eval_str("replace('abc', 1.5, 1, 'x')", &[]), "#VALUE!");
        assert_eq!(eval_str("replace('abc', 2, 1.5, 'x')", &[]), "#VALUE!");
        assert_eq!(eval_str("replace('abc', 'x', 1, 'y')", &[]), "#VALUE!");
        assert_eq!(eval_str("replace('abc', 2, 1, 'x', 2)", &[]), "#VALUE!");
        assert_eq!(eval_str("find('B', 'aBc')", &[]), "2");
        assert_eq!(eval_str("search('b', 'aBc')", &[]), "2");
        assert_eq!(eval_str("find('z', 'aBc')", &[]), "0");
        assert_eq!(eval_str("concatenate('a', 1)", &[]), "a1");
        assert_eq!(eval_str("value(' 42 ')", &[]), "42");

        // Dates compose: date() produces what year()/month()/day() read back.
        assert_eq!(eval_str("date(2026, 11, 20)", &[]), "2026-11-20");
        assert_eq!(eval_str("year(date(2026, 11, 20))", &[]), "2026");
        assert_eq!(eval_str("month(date(2026, 11, 20))", &[]), "11");
        assert_eq!(eval_str("day(date(2026, 11, 20))", &[]), "20");
        // A nonsense date is an error, not a made-up string.
        assert_eq!(eval_str("date(2026, 13, 1)", &[]), "#VALUE!");
    }

    #[test]
    fn multi_criteria_aggregates_read_the_prepass_sets() {
        let formula = Formula::parse(r#"countifs(amount, ">5", qty, "<3")"#).unwrap();
        let needs = formula.row_needs();
        let key = criteria_key(
            crate::formula::state::SetOp::Count,
            None,
            &needs.sets[0].pairs,
        );
        assert_eq!(
            needs.sets[0].pairs,
            vec![
                ("amount".to_string(), ">5".to_string()),
                ("qty".to_string(), "<3".to_string())
            ]
        );

        let mut state = RowState::new(needs.clone());
        let mut sets = HashMap::new();
        sets.insert(
            key,
            crate::formula::state::SetTotals {
                sum: 0.0,
                count: 2,
                min: 0.0,
                max: 0.0,
            },
        );
        state.set_sets(sets);
        let cells = row(&[("amount", "10"), ("qty", "1")]);
        let columns = vec!["amount".to_string(), "qty".to_string()];
        assert_eq!(
            formula
                .eval_stateful(&cells, &columns, &Refs::default(), 2, Some(&state))
                .render(),
            "2"
        );

        let summed = Formula::parse(r#"sumifs(amount, city, "CDMX")"#).unwrap();
        let needs = summed.row_needs();
        assert_eq!(needs.sets[0].value.as_deref(), Some("amount"));
        assert_eq!(needs.sets[0].op, crate::formula::state::SetOp::Sum);
        let key = criteria_key(
            crate::formula::state::SetOp::Sum,
            Some("amount"),
            &needs.sets[0].pairs,
        );
        let mut state = RowState::new(needs);
        let mut sets = HashMap::new();
        sets.insert(
            key,
            crate::formula::state::SetTotals {
                sum: 100.0,
                count: 4,
                min: 0.0,
                max: 0.0,
            },
        );
        state.set_sets(sets);
        let cells = row(&[("amount", "10"), ("city", "CDMX")]);
        let columns = vec!["amount".to_string(), "city".to_string()];
        assert_eq!(
            summed
                .eval_stateful(&cells, &columns, &Refs::default(), 2, Some(&state))
                .render(),
            "100"
        );

        let averaged = Formula::parse(r#"averageifs(amount, city, "CDMX")"#).unwrap();
        let needs = averaged.row_needs();
        assert_eq!(needs.sets[0].op, crate::formula::state::SetOp::Average);
        let key = criteria_key(
            crate::formula::state::SetOp::Average,
            Some("amount"),
            &needs.sets[0].pairs,
        );
        let mut state = RowState::new(needs);
        let mut sets = HashMap::new();
        sets.insert(
            key,
            crate::formula::state::SetTotals {
                sum: 40.0,
                count: 4,
                min: 0.0,
                max: 0.0,
            },
        );
        state.set_sets(sets);
        assert_eq!(
            averaged
                .eval_stateful(&cells, &columns, &Refs::default(), 2, Some(&state))
                .render(),
            "10"
        );

        // minifs/maxifs read the same set, but ask for the bounds instead of the sum.
        let lower = Formula::parse(r#"minifs(amount, city, "CDMX")"#).unwrap();
        let needs = lower.row_needs();
        let key = criteria_key(
            crate::formula::state::SetOp::Min,
            Some("amount"),
            &needs.sets[0].pairs,
        );
        let mut state = RowState::new(needs);
        let mut sets = HashMap::new();
        sets.insert(
            key,
            crate::formula::state::SetTotals {
                sum: 40.0,
                count: 4,
                min: 2.0,
                max: 20.0,
            },
        );
        state.set_sets(sets);
        assert_eq!(
            lower
                .eval_stateful(&cells, &columns, &Refs::default(), 2, Some(&state))
                .render(),
            "2"
        );
        let upper = Formula::parse(r#"maxifs(amount, city, "CDMX")"#).unwrap();
        let needs = upper.row_needs();
        let key = criteria_key(
            crate::formula::state::SetOp::Max,
            Some("amount"),
            &needs.sets[0].pairs,
        );
        let mut state = RowState::new(needs);
        let mut sets = HashMap::new();
        sets.insert(
            key,
            crate::formula::state::SetTotals {
                sum: 40.0,
                count: 4,
                min: 2.0,
                max: 20.0,
            },
        );
        state.set_sets(sets);
        assert_eq!(
            upper
                .eval_stateful(&cells, &columns, &Refs::default(), 2, Some(&state))
                .render(),
            "20"
        );
    }

    /// Whole-column aggregates are constant per row, so the formula layer must ask for
    /// the column up front and read the pre-pass totals instead of recomputing them.
    #[test]
    fn aggregate_functions_read_the_prepass_totals() {
        let formula = Formula::parse("sum(amount) + count(amount)").unwrap();
        assert!(formula.references().contains(&"amount".to_string()));
        assert_eq!(formula.row_needs().aggregates, vec!["amount".to_string()]);

        let mut state = RowState::new(formula.row_needs());
        let mut totals = HashMap::new();
        totals.insert("amount".to_string(), (100.0, 4));
        state.set_totals(totals);
        let mut bounds = HashMap::new();
        bounds.insert("amount".to_string(), (1.0, 10.0));
        state.set_bounds(bounds);
        let mut blanks = HashMap::new();
        blanks.insert("amount".to_string(), 0);
        state.set_blanks(blanks);
        let mut squares = HashMap::new();
        squares.insert("amount".to_string(), 3100.0);
        state.set_squares(squares);
        let mut filtered = HashMap::new();
        filtered.insert(("amount".to_string(), ">5".to_string()), (100.0, 4));
        state.set_filtered(filtered);

        let cells = row(&[("amount", "10")]);
        let columns = vec!["amount".to_string()];
        let refs = Refs::default();
        assert_eq!(
            formula
                .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
                .render(),
            "104"
        );
        assert_eq!(
            Formula::parse("average(amount)")
                .unwrap()
                .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
                .render(),
            "25"
        );
        assert_eq!(
            Formula::parse("min(amount)")
                .unwrap()
                .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
                .render(),
            "1"
        );
        assert_eq!(
            Formula::parse("max(amount)")
                .unwrap()
                .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
                .render(),
            "10"
        );
        assert_eq!(
            Formula::parse("countblank(amount)")
                .unwrap()
                .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
                .render(),
            "0"
        );
        assert_eq!(
            Formula::parse(r#"averageif(amount, ">5")"#)
                .unwrap()
                .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
                .render(),
            "25"
        );

        let variance = Formula::parse("var(amount)").unwrap();
        let rendered = variance
            .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
            .render();
        assert!(
            (rendered.parse::<f64>().unwrap() - 200.0).abs() < 0.01,
            "var = {rendered}"
        );
        let deviation = Formula::parse("stdev(amount)").unwrap();
        let rendered = deviation
            .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
            .render();
        assert!(
            (rendered.parse::<f64>().unwrap() - 200.0_f64.sqrt()).abs() < 0.01,
            "stdev = {rendered}"
        );
        let population = Formula::parse("varp(amount)").unwrap();
        let rendered = population
            .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
            .render();
        assert!(
            (rendered.parse::<f64>().unwrap() - 150.0).abs() < 0.01,
            "varp = {rendered}"
        );
        let population_sd = Formula::parse("stdevp(amount)").unwrap();
        let rendered = population_sd
            .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
            .render();
        assert!(
            (rendered.parse::<f64>().unwrap() - 150.0_f64.sqrt()).abs() < 0.01,
            "stdevp = {rendered}"
        );
        // No pre-pass (the view path) is an error, never a wrong zero.
        assert_eq!(
            Formula::parse("sum(amount)")
                .unwrap()
                .eval_stateful(&cells, &columns, &refs, 2, None)
                .render(),
            "#VALUE!"
        );
    }

    #[test]
    fn countif_and_sumif_read_the_filtered_prepass() {
        let formula =
            Formula::parse(r#"countif(amount, ">1000") + sumif(amount, ">1000")"#).unwrap();
        assert_eq!(
            formula.row_needs().filtered,
            vec![("amount".to_string(), ">1000".to_string())]
        );
        let mut state = RowState::new(formula.row_needs());
        let mut filtered = HashMap::new();
        filtered.insert(("amount".to_string(), ">1000".to_string()), (5000.0, 2));
        state.set_filtered(filtered);
        let cells = row(&[("amount", "10")]);
        let columns = vec!["amount".to_string()];
        let refs = Refs::default();
        assert_eq!(
            formula
                .eval_stateful(&cells, &columns, &refs, 2, Some(&state))
                .render(),
            "5002"
        );
    }
}
