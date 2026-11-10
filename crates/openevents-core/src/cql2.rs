//! A bounded profile of OGC CQL2 (ADR-0006).
//!
//! Both encodings parse to the same tree: CQL2-text (used in profiles) and CQL2-JSON
//! (used on the wire). Supported in this first version: `=`, `<>`, `<`, `<=`, `>`, `>=`,
//! `in`, `isNull`, `and`, `or`, `not`, and the function `actor()` / `actor('claim')`.
//! Properties are dotted paths into the subject's state; a path through a list matches
//! when any element matches.

use serde_json::{json, Value};

pub const MAX_DEPTH: usize = 8;
pub const MAX_ARGS: usize = 10;
pub const MAX_IN: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Property(String),
    Literal(Value),
    /// `actor()` is the actor id, `actor('rol')` a claim from the token.
    Actor(Option<String>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Cmp(CmpOp, Operand, Operand),
    In(Operand, Vec<Operand>),
    IsNull(Operand),
    Bool(bool),
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("invalid filter: {0}")]
pub struct FilterError(pub String);

fn ferr<T>(msg: impl Into<String>) -> Result<T, FilterError> {
    Err(FilterError(msg.into()))
}

/// Who is asking, for `actor()`.
#[derive(Debug, Clone, Default)]
pub struct ActorContext {
    pub id: String,
    pub claims: Value,
}

impl Expr {
    /// Does the filter use `actor()`? Client filters may not; profile rules may.
    pub fn uses_actor(&self) -> bool {
        fn op(o: &Operand) -> bool {
            matches!(o, Operand::Actor(_))
        }
        match self {
            Expr::And(v) | Expr::Or(v) => v.iter().any(|e| e.uses_actor()),
            Expr::Not(e) => e.uses_actor(),
            Expr::Cmp(_, a, b) => op(a) || op(b),
            Expr::In(a, list) => op(a) || list.iter().any(op),
            Expr::IsNull(a) => op(a),
            Expr::Bool(_) => false,
        }
    }

    /// All property paths the filter reads.
    pub fn properties(&self) -> Vec<String> {
        let mut out = Vec::new();
        fn op(o: &Operand, out: &mut Vec<String>) {
            if let Operand::Property(p) = o {
                out.push(p.clone());
            }
        }
        fn walk(e: &Expr, out: &mut Vec<String>) {
            match e {
                Expr::And(v) | Expr::Or(v) => v.iter().for_each(|e| walk(e, out)),
                Expr::Not(e) => walk(e, out),
                Expr::Cmp(_, a, b) => {
                    op(a, out);
                    op(b, out)
                }
                Expr::In(a, list) => {
                    op(a, out);
                    list.iter().for_each(|o| op(o, out))
                }
                Expr::IsNull(a) => op(a, out),
                Expr::Bool(_) => {}
            }
        }
        walk(self, &mut out);
        out
    }

    pub fn eval(&self, doc: &Value, actor: &ActorContext) -> bool {
        match self {
            Expr::Bool(b) => *b,
            Expr::And(v) => v.iter().all(|e| e.eval(doc, actor)),
            Expr::Or(v) => v.iter().any(|e| e.eval(doc, actor)),
            Expr::Not(e) => !e.eval(doc, actor),
            Expr::IsNull(a) => resolve(a, doc, actor).iter().all(|v| v.is_null()),
            Expr::Cmp(op, a, b) => {
                let left = resolve(a, doc, actor);
                let right = resolve(b, doc, actor);
                left.iter()
                    .any(|l| right.iter().any(|r| compare(*op, l, r)))
            }
            Expr::In(a, list) => {
                let left = resolve(a, doc, actor);
                list.iter().any(|item| {
                    let right = resolve(item, doc, actor);
                    left.iter()
                        .any(|l| right.iter().any(|r| compare(CmpOp::Eq, l, r)))
                })
            }
        }
    }

    /// CQL2-JSON encoding.
    pub fn to_json(&self) -> Value {
        fn op(o: &Operand) -> Value {
            match o {
                Operand::Property(p) => json!({ "property": p }),
                Operand::Literal(v) => v.clone(),
                Operand::Actor(None) => json!({ "op": "actor", "args": [] }),
                Operand::Actor(Some(c)) => json!({ "op": "actor", "args": [c] }),
            }
        }
        match self {
            Expr::Bool(b) => json!(b),
            Expr::And(v) => {
                json!({ "op": "and", "args": v.iter().map(|e| e.to_json()).collect::<Vec<_>>() })
            }
            Expr::Or(v) => {
                json!({ "op": "or", "args": v.iter().map(|e| e.to_json()).collect::<Vec<_>>() })
            }
            Expr::Not(e) => json!({ "op": "not", "args": [e.to_json()] }),
            Expr::IsNull(a) => json!({ "op": "isNull", "args": [op(a)] }),
            Expr::In(a, list) => {
                json!({ "op": "in", "args": [op(a), list.iter().map(op).collect::<Vec<_>>()] })
            }
            Expr::Cmp(c, a, b) => {
                let name = match c {
                    CmpOp::Eq => "=",
                    CmpOp::Ne => "<>",
                    CmpOp::Lt => "<",
                    CmpOp::Le => "<=",
                    CmpOp::Gt => ">",
                    CmpOp::Ge => ">=",
                };
                json!({ "op": name, "args": [op(a), op(b)] })
            }
        }
    }
}

/// Values a path points at, flattening lists along the way.
pub fn lookup<'a>(doc: &'a Value, path: &str) -> Vec<&'a Value> {
    let mut current = vec![doc];
    for part in path.split('.') {
        let mut next = Vec::new();
        for v in current {
            match v {
                Value::Object(map) => {
                    if let Some(x) = map.get(part) {
                        next.push(x);
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        if let Some(x) = item.get(part) {
                            next.push(x);
                        }
                    }
                }
                _ => {}
            }
        }
        current = next;
    }
    let mut out = Vec::new();
    for v in current {
        match v {
            Value::Array(items) => out.extend(items.iter()),
            other => out.push(other),
        }
    }
    out
}

fn resolve(o: &Operand, doc: &Value, actor: &ActorContext) -> Vec<Value> {
    match o {
        Operand::Literal(v) => vec![v.clone()],
        Operand::Property(p) => lookup(doc, p).into_iter().cloned().collect(),
        Operand::Actor(None) => vec![Value::String(actor.id.clone())],
        Operand::Actor(Some(claim)) => lookup(&actor.claims, claim).into_iter().cloned().collect(),
    }
}

fn compare(op: CmpOp, a: &Value, b: &Value) -> bool {
    use std::cmp::Ordering;
    let ord = match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64().partial_cmp(&y.as_f64()),
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    };
    match (op, ord) {
        (CmpOp::Eq, Some(o)) => o == Ordering::Equal,
        (CmpOp::Ne, Some(o)) => o != Ordering::Equal,
        (CmpOp::Lt, Some(o)) => o == Ordering::Less,
        (CmpOp::Le, Some(o)) => o != Ordering::Greater,
        (CmpOp::Gt, Some(o)) => o == Ordering::Greater,
        (CmpOp::Ge, Some(o)) => o != Ordering::Less,
        (_, None) => false,
    }
}

// ---------- CQL2-JSON ----------

pub fn parse_json(v: &Value) -> Result<Expr, FilterError> {
    parse_json_expr(v, 0)
}

fn parse_json_expr(v: &Value, depth: usize) -> Result<Expr, FilterError> {
    if depth > MAX_DEPTH {
        return ferr("filter is nested too deeply");
    }
    if let Value::Bool(b) = v {
        return Ok(Expr::Bool(*b));
    }
    let obj = v
        .as_object()
        .ok_or_else(|| FilterError("expected an object with op and args".into()))?;
    let op = obj
        .get("op")
        .and_then(|o| o.as_str())
        .ok_or_else(|| FilterError("missing op".into()))?;
    let args = obj
        .get("args")
        .and_then(|a| a.as_array())
        .ok_or_else(|| FilterError(format!("{op}: missing args")))?;
    if args.len() > MAX_ARGS {
        return ferr(format!("{op}: at most {MAX_ARGS} args"));
    }
    match op {
        "and" | "or" => {
            let list = args
                .iter()
                .map(|a| parse_json_expr(a, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            if list.is_empty() {
                return ferr(format!("{op}: needs args"));
            }
            Ok(if op == "and" {
                Expr::And(list)
            } else {
                Expr::Or(list)
            })
        }
        "not" => {
            if args.len() != 1 {
                return ferr("not: needs one arg");
            }
            Ok(Expr::Not(Box::new(parse_json_expr(&args[0], depth + 1)?)))
        }
        "isNull" => {
            if args.len() != 1 {
                return ferr("isNull: needs one arg");
            }
            Ok(Expr::IsNull(parse_json_operand(&args[0])?))
        }
        "in" => {
            if args.len() != 2 {
                return ferr("in: needs two args");
            }
            let list = args[1]
                .as_array()
                .ok_or_else(|| FilterError("in: second arg must be a list".into()))?;
            if list.len() > MAX_IN {
                return ferr(format!("in: at most {MAX_IN} values"));
            }
            Ok(Expr::In(
                parse_json_operand(&args[0])?,
                list.iter()
                    .map(parse_json_operand)
                    .collect::<Result<_, _>>()?,
            ))
        }
        "=" | "<>" | "<" | "<=" | ">" | ">=" => {
            if args.len() != 2 {
                return ferr(format!("{op}: needs two args"));
            }
            let cmp = match op {
                "=" => CmpOp::Eq,
                "<>" => CmpOp::Ne,
                "<" => CmpOp::Lt,
                "<=" => CmpOp::Le,
                ">" => CmpOp::Gt,
                _ => CmpOp::Ge,
            };
            Ok(Expr::Cmp(
                cmp,
                parse_json_operand(&args[0])?,
                parse_json_operand(&args[1])?,
            ))
        }
        other => ferr(format!("operator {other} is not supported")),
    }
}

fn parse_json_operand(v: &Value) -> Result<Operand, FilterError> {
    if let Some(obj) = v.as_object() {
        if let Some(p) = obj.get("property").and_then(|p| p.as_str()) {
            check_path(p)?;
            return Ok(Operand::Property(p.to_string()));
        }
        if obj.get("op").and_then(|o| o.as_str()) == Some("actor") {
            let args = obj
                .get("args")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default();
            return match args.as_slice() {
                [] => Ok(Operand::Actor(None)),
                [Value::String(c)] => Ok(Operand::Actor(Some(c.clone()))),
                _ => ferr("actor: takes no args or one claim name"),
            };
        }
        return ferr("objects are only allowed as property or function");
    }
    match v {
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {
            Ok(Operand::Literal(v.clone()))
        }
        _ => ferr("unsupported operand"),
    }
}

fn check_path(p: &str) -> Result<(), FilterError> {
    if p.is_empty() || p.split('.').any(|s| s.is_empty()) || p.split('.').count() > 6 {
        return ferr(format!("invalid property path {p}"));
    }
    Ok(())
}

// ---------- CQL2-text ----------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(f64),
    Op(String),
    LParen,
    RParen,
    Comma,
}

fn lex(s: &str) -> Result<Vec<Tok>, FilterError> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            out.push(Tok::LParen);
            i += 1;
        } else if c == ')' {
            out.push(Tok::RParen);
            i += 1;
        } else if c == ',' {
            out.push(Tok::Comma);
            i += 1;
        } else if c == '\'' {
            let mut v = String::new();
            i += 1;
            loop {
                if i >= chars.len() {
                    return ferr("unterminated string");
                }
                if chars[i] == '\'' {
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        v.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                v.push(chars[i]);
                i += 1;
            }
            out.push(Tok::Str(v));
        } else if "=<>".contains(c) {
            let mut op = c.to_string();
            if i + 1 < chars.len() && (chars[i + 1] == '=' || (c == '<' && chars[i + 1] == '>')) {
                op.push(chars[i + 1]);
                i += 1;
            }
            out.push(Tok::Op(op));
            i += 1;
        } else if c.is_ascii_digit()
            || (c == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
        {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let n: String = chars[start..i].iter().collect();
            out.push(Tok::Num(
                n.parse()
                    .map_err(|_| FilterError(format!("bad number {n}")))?,
            ));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            out.push(Tok::Ident(chars[start..i].iter().collect()));
        } else {
            return ferr(format!("unexpected character {c}"));
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(kw))
    }
    fn expect(&mut self, t: Tok) -> Result<(), FilterError> {
        match self.next() {
            Some(x) if x == t => Ok(()),
            other => ferr(format!("expected {t:?}, got {other:?}")),
        }
    }

    fn or(&mut self, depth: usize) -> Result<Expr, FilterError> {
        let mut list = vec![self.and(depth)?];
        while self.keyword("or") {
            self.pos += 1;
            list.push(self.and(depth)?);
        }
        Ok(if list.len() == 1 {
            list.pop().unwrap()
        } else {
            Expr::Or(list)
        })
    }

    fn and(&mut self, depth: usize) -> Result<Expr, FilterError> {
        let mut list = vec![self.not(depth)?];
        while self.keyword("and") {
            self.pos += 1;
            list.push(self.not(depth)?);
        }
        Ok(if list.len() == 1 {
            list.pop().unwrap()
        } else {
            Expr::And(list)
        })
    }

    fn not(&mut self, depth: usize) -> Result<Expr, FilterError> {
        if self.keyword("not") {
            self.pos += 1;
            return Ok(Expr::Not(Box::new(self.not(depth)?)));
        }
        self.primary(depth)
    }

    fn primary(&mut self, depth: usize) -> Result<Expr, FilterError> {
        if depth > MAX_DEPTH {
            return ferr("filter is nested too deeply");
        }
        if self.peek() == Some(&Tok::LParen) {
            self.pos += 1;
            let e = self.or(depth + 1)?;
            self.expect(Tok::RParen)?;
            return Ok(e);
        }
        if self.keyword("true") || self.keyword("false") {
            let b = self.keyword("true");
            self.pos += 1;
            return Ok(Expr::Bool(b));
        }
        let left = self.operand()?;
        if self.keyword("is") {
            self.pos += 1;
            let negate = if self.keyword("not") {
                self.pos += 1;
                true
            } else {
                false
            };
            if !self.keyword("null") {
                return ferr("expected NULL");
            }
            self.pos += 1;
            let e = Expr::IsNull(left);
            return Ok(if negate { Expr::Not(Box::new(e)) } else { e });
        }
        let negate_in = if self.keyword("not") {
            self.pos += 1;
            true
        } else {
            false
        };
        if self.keyword("in") {
            self.pos += 1;
            self.expect(Tok::LParen)?;
            let mut list = vec![self.operand()?];
            while self.peek() == Some(&Tok::Comma) {
                self.pos += 1;
                list.push(self.operand()?);
            }
            self.expect(Tok::RParen)?;
            if list.len() > MAX_IN {
                return ferr(format!("in: at most {MAX_IN} values"));
            }
            let e = Expr::In(left, list);
            return Ok(if negate_in { Expr::Not(Box::new(e)) } else { e });
        }
        if negate_in {
            return ferr("expected IN after NOT");
        }
        let op = match self.next() {
            Some(Tok::Op(o)) => o,
            other => return ferr(format!("expected comparison, got {other:?}")),
        };
        let cmp = match op.as_str() {
            "=" => CmpOp::Eq,
            "<>" => CmpOp::Ne,
            "<" => CmpOp::Lt,
            "<=" => CmpOp::Le,
            ">" => CmpOp::Gt,
            ">=" => CmpOp::Ge,
            o => return ferr(format!("unknown operator {o}")),
        };
        let right = self.operand()?;
        Ok(Expr::Cmp(cmp, left, right))
    }

    fn operand(&mut self) -> Result<Operand, FilterError> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(Operand::Literal(Value::String(s))),
            Some(Tok::Num(n)) => Ok(Operand::Literal(json!(n))),
            Some(Tok::Ident(id))
                if id.eq_ignore_ascii_case("actor") && self.peek() == Some(&Tok::LParen) =>
            {
                self.pos += 1;
                let claim = match self.next() {
                    Some(Tok::RParen) => return Ok(Operand::Actor(None)),
                    Some(Tok::Str(c)) => c,
                    other => return ferr(format!("actor: expected a claim name, got {other:?}")),
                };
                self.expect(Tok::RParen)?;
                Ok(Operand::Actor(Some(claim)))
            }
            Some(Tok::Ident(id)) if id.eq_ignore_ascii_case("true") => {
                Ok(Operand::Literal(json!(true)))
            }
            Some(Tok::Ident(id)) if id.eq_ignore_ascii_case("false") => {
                Ok(Operand::Literal(json!(false)))
            }
            Some(Tok::Ident(id)) => {
                check_path(&id)?;
                Ok(Operand::Property(id))
            }
            other => ferr(format!("expected a value or property, got {other:?}")),
        }
    }
}

pub fn parse_text(s: &str) -> Result<Expr, FilterError> {
    let mut p = Parser {
        toks: lex(s)?,
        pos: 0,
    };
    let e = p.or(0)?;
    if p.pos != p.toks.len() {
        return ferr(format!("unexpected {:?}", p.peek()));
    }
    Ok(e)
}

/// A filter as given by a client or a profile: CQL2-text in a string, or CQL2-JSON.
pub fn parse_any(v: &Value) -> Result<Expr, FilterError> {
    match v {
        Value::String(s) => parse_text(s),
        other => parse_json(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(id: &str, claims: Value) -> ActorContext {
        ActorContext {
            id: id.into(),
            claims,
        }
    }

    #[test]
    fn text_and_json_give_the_same_tree() {
        let t = parse_text("status = 'open' AND deelnames.partij = actor()").unwrap();
        let j = parse_json(&t.to_json()).unwrap();
        assert_eq!(t, j);
    }

    #[test]
    fn paths_through_lists() {
        let doc = json!({ "deelnames": [ { "partij": "a", "rol": "inwoner" }, { "partij": "b", "rol": "curator" } ] });
        let e = parse_text("deelnames.partij = actor()").unwrap();
        assert!(e.eval(&doc, &actor("b", json!({}))));
        assert!(!e.eval(&doc, &actor("c", json!({}))));
    }

    #[test]
    fn in_not_and_claims() {
        let doc = json!({ "status": "open", "n": 3 });
        let e = parse_text("status IN ('open', 'wacht') AND NOT n > 5 AND actor('rol') = 'kcc'")
            .unwrap();
        assert!(e.eval(&doc, &actor("x", json!({ "rol": "kcc" }))));
        assert!(!e.eval(&doc, &actor("x", json!({ "rol": "inwoner" }))));
        assert!(parse_text("einde IS NULL")
            .unwrap()
            .eval(&doc, &actor("x", json!({}))));
        assert!(parse_text("status IS NOT NULL")
            .unwrap()
            .eval(&doc, &actor("x", json!({}))));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_text("status = ").is_err());
        assert!(parse_text("a.. = 1").is_err());
        assert!(parse_json(&json!({ "op": "like", "args": [] })).is_err());
        assert!(parse_text("x = 1 extra").is_err());
    }

    #[test]
    fn detects_actor() {
        assert!(parse_text("a = actor()").unwrap().uses_actor());
        assert!(!parse_text("a = 'x'").unwrap().uses_actor());
    }
}
