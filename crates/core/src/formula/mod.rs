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
//!          | edate(date, months) | eomonth(date, months) | weekday(date) | days(end, start)
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

        assert_eq!(eval_str("char(65)", &[]), "A");
        assert_eq!(eval_str("code(\"A\")", &[]), "65");
        assert_eq!(eval_str("code(\"\")", &[]), "#VALUE!");
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
        let formula = Formula::parse(r#"vlookup("people.csv", "cust", "name")"#).unwrap();
        assert_eq!(
            formula.sheet_references(),
            vec![(
                "people.csv".to_string(),
                "cust".to_string(),
                "name".to_string()
            )],
            "the alias must feed the loader, not only the evaluator"
        );
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
        let formula = Formula::parse("mode(A2:D2)").unwrap();
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "10"
        );
        // A tie picks the smaller value so the answer is deterministic.
        for (name, value) in [("a", "1"), ("b", "2"), ("c", "1"), ("d", "2")] {
            cells.insert(name.to_string(), value.to_string());
        }
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "1"
        );
        // No repeated value has no mode.
        for (name, value) in [("a", "10"), ("b", "3"), ("c", "7"), ("d", "5")] {
            cells.insert(name.to_string(), value.to_string());
        }
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!"
        );
        cells.insert("d".to_string(), "n/a".to_string());
        assert_eq!(
            formula
                .eval_at(&cells, &columns, &Refs::default(), 1)
                .render(),
            "#VALUE!",
            "text is ignored and leaves three distinct numbers"
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
        assert_eq!(eval_str("rept('ab', 3)", &[]), "ababab");
        assert_eq!(eval_str("substitute('a-b-c', '-', '+')", &[]), "a+b+c");
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
