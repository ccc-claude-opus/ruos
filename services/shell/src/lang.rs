//! The shell language: a small sh. Words with '…' (literal), "…" (with $
//! expansions) and \ escapes; `$NAME`, `${NAME}`, `$?`, `$#`, `$@`, `$0`–`$9`,
//! `$(command)`, `$((arithmetic))`; `*` and `?` in unquoted words match
//! file names; pipelines with `<`, `>`, `>>`; lists joined by `;`, `&&`,
//! `||` or newlines; `if … then … [elif …] [else …] fi`, `for NAME in …; do …
//! done`, `while … do … done`; `NAME=value`; `#` comments.
//!
//! Commands are the shell's built-ins (cmds.rs): each gives back its
//! output, which goes to the next command, a file, a `$(…)` or the console.
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::Sh;
use crate::cmds;

/// A piece of a word: how it was quoted decides how it is expanded.
#[derive(Clone, Debug, PartialEq)]
enum Part {
    /// '…' or a \ escape: as it is.
    Lit(String),
    /// Unquoted: $ expansions, then split into fields and matched as file names.
    Bare(String),
    /// "…": $ expansions only.
    Dq(String),
}

#[derive(Clone, Debug, PartialEq)]
struct Word(Vec<Part>);

impl Word {
    /// The word as plain text if it is one unquoted piece (keywords, names).
    fn plain(&self) -> Option<&str> {
        match self.0.as_slice() {
            [Part::Bare(s)] => Some(s),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    W(Word),
    Pipe,
    Out,
    Append,
    In,
    Semi,
    And,
    Or,
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    /// The input ends inside a quote, an `if`, a loop…: read another line.
    Incomplete,
    Bad(String),
}

/// Copies `$(…)` / `$((…))` through as it is (it is parsed when expanded).
fn dollar_paren(cs: &mut core::iter::Peekable<core::str::Chars>, cur: &mut String) -> Result<(), ParseError> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    while let Some(c) = cs.next() {
        cur.push(c);
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    Err(ParseError::Incomplete)
}

fn lex(src: &str) -> Result<Vec<Tok>, ParseError> {
    let mut toks = Vec::new();
    let mut parts: Vec<Part> = Vec::new();
    let mut cur = String::new();
    let mut cs = src.chars().peekable();
    let flush_bare = |cur: &mut String, parts: &mut Vec<Part>| {
        if !cur.is_empty() {
            parts.push(Part::Bare(core::mem::take(cur)));
        }
    };
    let end_word = |cur: &mut String, parts: &mut Vec<Part>, toks: &mut Vec<Tok>| {
        if !cur.is_empty() {
            parts.push(Part::Bare(core::mem::take(cur)));
        }
        if !parts.is_empty() {
            toks.push(Tok::W(Word(core::mem::take(parts))));
        }
    };
    while let Some(c) = cs.next() {
        match c {
            '#' if cur.is_empty() && parts.is_empty() => {
                while cs.peek().is_some_and(|&c| c != '\n') {
                    cs.next();
                }
            }
            '\'' => {
                flush_bare(&mut cur, &mut parts);
                let mut s = String::new();
                loop {
                    match cs.next() {
                        Some('\'') => break,
                        Some(c) => s.push(c),
                        None => return Err(ParseError::Incomplete),
                    }
                }
                parts.push(Part::Lit(s));
            }
            '"' => {
                flush_bare(&mut cur, &mut parts);
                let mut s = String::new();
                loop {
                    match cs.next() {
                        Some('"') => break,
                        Some('\\') => match cs.next() {
                            Some(c @ ('"' | '\\' | '$')) => {
                                s.push('\\');
                                s.push(c);
                            }
                            Some(c) => {
                                s.push('\\');
                                s.push(c);
                            }
                            None => return Err(ParseError::Incomplete),
                        },
                        Some('$') if cs.peek() == Some(&'(') => {
                            s.push('$');
                            dollar_paren(&mut cs, &mut s)?;
                        }
                        Some(c) => s.push(c),
                        None => return Err(ParseError::Incomplete),
                    }
                }
                parts.push(Part::Dq(s));
            }
            '\\' => match cs.next() {
                Some('\n') => {}
                Some(c) => {
                    flush_bare(&mut cur, &mut parts);
                    parts.push(Part::Lit(c.to_string()));
                }
                None => return Err(ParseError::Incomplete),
            },
            '$' if cs.peek() == Some(&'(') => {
                cur.push('$');
                dollar_paren(&mut cs, &mut cur)?;
            }
            c if c.is_whitespace() || "|<>;&".contains(c) => {
                end_word(&mut cur, &mut parts, &mut toks);
                match c {
                    '\n' | ';' => toks.push(Tok::Semi),
                    '|' if cs.peek() == Some(&'|') => {
                        cs.next();
                        toks.push(Tok::Or);
                    }
                    '|' => toks.push(Tok::Pipe),
                    '&' if cs.peek() == Some(&'&') => {
                        cs.next();
                        toks.push(Tok::And);
                    }
                    '&' => return Err(ParseError::Bad(String::from("background jobs (&) are not supported; use dock run -d"))),
                    '<' => toks.push(Tok::In),
                    '>' if cs.peek() == Some(&'>') => {
                        cs.next();
                        toks.push(Tok::Append);
                    }
                    '>' => toks.push(Tok::Out),
                    _ => {}
                }
            }
            c => cur.push(c),
        }
    }
    end_word(&mut cur, &mut parts, &mut toks);
    Ok(toks)
}

#[derive(Debug)]
struct Simple {
    words: Vec<Word>,
    input: Option<Word>,
    /// (file, append)
    output: Option<(Word, bool)>,
}

#[derive(Debug)]
enum Command {
    Simple(Simple),
    If(Vec<(List, List)>, Option<List>),
    For(String, Option<Vec<Word>>, List),
    While(List, List),
}

/// Commands joined by pipes.
type Pipeline = Vec<Command>;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Join {
    Then,
    And,
    Or,
}

/// Pipelines, each run after the one before (always, on success, on failure).
#[derive(Debug, Default)]
pub struct List(Vec<(Join, Pipeline)>);

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

const KEYWORDS: [&str; 9] = ["then", "elif", "else", "fi", "do", "done", "in", "if", "while"];

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn keyword(&self) -> Option<&str> {
        match self.peek() {
            Some(Tok::W(w)) => w.plain(),
            _ => None,
        }
    }

    fn expect(&mut self, kw: &str) -> Result<(), ParseError> {
        match self.keyword() {
            Some(k) if k == kw => {
                self.at += 1;
                Ok(())
            }
            Some(k) => Err(ParseError::Bad(format!("expected {kw}, found {k}"))),
            None if self.peek().is_none() => Err(ParseError::Incomplete),
            None => Err(ParseError::Bad(format!("expected {kw}"))),
        }
    }

    fn skip_semis(&mut self) {
        while self.peek() == Some(&Tok::Semi) {
            self.at += 1;
        }
    }

    /// A list, up to one of the `stop` keywords (or the end, if `stop` is empty).
    fn list(&mut self, stop: &[&str]) -> Result<List, ParseError> {
        let mut l = List::default();
        let mut join = Join::Then;
        loop {
            self.skip_semis();
            match self.keyword() {
                Some(k) if stop.contains(&k) => break,
                _ => {}
            }
            if self.peek().is_none() {
                if stop.is_empty() {
                    break;
                }
                return Err(ParseError::Incomplete);
            }
            l.0.push((join, self.pipeline()?));
            join = match self.peek() {
                Some(Tok::And) => Join::And,
                Some(Tok::Or) => Join::Or,
                Some(Tok::Semi) | None => Join::Then,
                Some(_) if !stop.is_empty() => Join::Then,
                Some(t) => return Err(ParseError::Bad(format!("unexpected {t:?}"))),
            };
            if matches!(self.peek(), Some(Tok::And | Tok::Or)) {
                self.at += 1;
                self.skip_semis();
                if self.peek().is_none() {
                    return Err(ParseError::Incomplete);
                }
            }
        }
        Ok(l)
    }

    fn pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let mut p = alloc::vec![self.command()?];
        while self.peek() == Some(&Tok::Pipe) {
            self.at += 1;
            self.skip_semis();
            if self.peek().is_none() {
                return Err(ParseError::Incomplete);
            }
            p.push(self.command()?);
        }
        Ok(p)
    }

    fn command(&mut self) -> Result<Command, ParseError> {
        match self.keyword() {
            Some("if") => {
                self.at += 1;
                let mut arms = Vec::new();
                let mut otherwise = None;
                loop {
                    let cond = self.list(&["then"])?;
                    self.expect("then")?;
                    let body = self.list(&["elif", "else", "fi"])?;
                    arms.push((cond, body));
                    match self.keyword() {
                        Some("elif") => self.at += 1,
                        Some("else") => {
                            self.at += 1;
                            otherwise = Some(self.list(&["fi"])?);
                            self.expect("fi")?;
                            break;
                        }
                        _ => {
                            self.expect("fi")?;
                            break;
                        }
                    }
                }
                Ok(Command::If(arms, otherwise))
            }
            Some("for") => {
                self.at += 1;
                let name = match self.peek() {
                    Some(Tok::W(w)) => w.plain().map(String::from).ok_or(ParseError::Bad(String::from("for NAME")))?,
                    None => return Err(ParseError::Incomplete),
                    _ => return Err(ParseError::Bad(String::from("for NAME"))),
                };
                self.at += 1;
                let mut words = None;
                if self.keyword() == Some("in") {
                    self.at += 1;
                    let mut ws = Vec::new();
                    while let Some(Tok::W(w)) = self.peek() {
                        ws.push(w.clone());
                        self.at += 1;
                    }
                    words = Some(ws);
                }
                self.skip_semis();
                self.expect("do")?;
                let body = self.list(&["done"])?;
                self.expect("done")?;
                Ok(Command::For(name, words, body))
            }
            Some("while") => {
                self.at += 1;
                let cond = self.list(&["do"])?;
                self.expect("do")?;
                let body = self.list(&["done"])?;
                self.expect("done")?;
                Ok(Command::While(cond, body))
            }
            Some(k) if KEYWORDS.contains(&k) => Err(ParseError::Bad(format!("unexpected {k}"))),
            _ => self.simple(),
        }
    }

    fn simple(&mut self) -> Result<Command, ParseError> {
        let mut s = Simple { words: Vec::new(), input: None, output: None };
        loop {
            match self.peek() {
                Some(Tok::W(w)) => {
                    s.words.push(w.clone());
                    self.at += 1;
                }
                Some(op @ (Tok::In | Tok::Out | Tok::Append)) => {
                    let op = op.clone();
                    self.at += 1;
                    let f = match self.peek() {
                        Some(Tok::W(w)) => w.clone(),
                        None => return Err(ParseError::Incomplete),
                        _ => return Err(ParseError::Bad(String::from("a file name must follow < > >>"))),
                    };
                    self.at += 1;
                    match op {
                        Tok::In => s.input = Some(f),
                        _ => s.output = Some((f, op == Tok::Append)),
                    }
                }
                _ => break,
            }
        }
        if s.words.is_empty() && s.input.is_none() && s.output.is_none() {
            return Err(ParseError::Bad(String::from("missing command")));
        }
        Ok(Command::Simple(s))
    }
}

pub fn parse(src: &str) -> Result<List, ParseError> {
    let mut p = Parser { toks: lex(src)?, at: 0 };
    let l = p.list(&[])?;
    match p.peek() {
        None => Ok(l),
        Some(t) => Err(ParseError::Bad(format!("unexpected {t:?}"))),
    }
}

/// What running something ended with.
pub enum Flow {
    Next,
    Exit(i64),
}

/// `*` and `?` against a name.
pub fn glob_match(pat: &[char], name: &[char]) -> bool {
    match (pat.first(), name.first()) {
        (None, None) => true,
        (Some('*'), _) => glob_match(&pat[1..], name) || (!name.is_empty() && glob_match(pat, &name[1..])),
        (Some('?'), Some(_)) => glob_match(&pat[1..], &name[1..]),
        (Some(p), Some(n)) if p == n => glob_match(&pat[1..], &name[1..]),
        _ => false,
    }
}

/// Integer arithmetic: + - * / % and parentheses, numbers and variable names.
fn arith(sh: &Sh, src: &str) -> Result<i64, String> {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
        sh: &'a Sh,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
                self.i += 1;
            }
        }
        fn expr(&mut self) -> Result<i64, String> {
            let mut v = self.term()?;
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'+') => {
                        self.i += 1;
                        v = v.wrapping_add(self.term()?);
                    }
                    Some(b'-') => {
                        self.i += 1;
                        v = v.wrapping_sub(self.term()?);
                    }
                    _ => return Ok(v),
                }
            }
        }
        fn term(&mut self) -> Result<i64, String> {
            let mut v = self.atom()?;
            loop {
                self.ws();
                let op = self.s.get(self.i).copied();
                if !matches!(op, Some(b'*' | b'/' | b'%')) {
                    return Ok(v);
                }
                self.i += 1;
                let r = self.atom()?;
                v = match op {
                    Some(b'*') => v.wrapping_mul(r),
                    _ if r == 0 => return Err(String::from("division by zero")),
                    Some(b'/') => v / r,
                    _ => v % r,
                };
            }
        }
        fn atom(&mut self) -> Result<i64, String> {
            self.ws();
            match self.s.get(self.i) {
                Some(b'(') => {
                    self.i += 1;
                    let v = self.expr()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b')') {
                        return Err(String::from("missing )"));
                    }
                    self.i += 1;
                    Ok(v)
                }
                Some(b'-') => {
                    self.i += 1;
                    Ok(-self.atom()?)
                }
                Some(c) if c.is_ascii_digit() => {
                    let st = self.i;
                    while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit()) {
                        self.i += 1;
                    }
                    core::str::from_utf8(&self.s[st..self.i]).unwrap().parse().map_err(|_| String::from("number too big"))
                }
                Some(c) if c.is_ascii_alphabetic() || *c == b'_' || *c == b'$' => {
                    if *c == b'$' {
                        self.i += 1;
                    }
                    let st = self.i;
                    while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                        self.i += 1;
                    }
                    let name = core::str::from_utf8(&self.s[st..self.i]).unwrap();
                    Ok(self.sh.var(name).trim().parse().unwrap_or(0))
                }
                _ => Err(String::from("bad arithmetic")),
            }
        }
    }
    let mut p = P { s: src.as_bytes(), i: 0, sh };
    let v = p.expr()?;
    p.ws();
    if p.i != src.len() {
        return Err(format!("bad arithmetic: {src}"));
    }
    Ok(v)
}

impl Sh {
    /// `$…` expansions in `s`.
    fn expand_text(&mut self, s: &str) -> Result<String, String> {
        let cs: Vec<char> = s.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < cs.len() {
            let c = cs[i];
            if c == '\\' && i + 1 < cs.len() && matches!(cs[i + 1], '"' | '\\' | '$') {
                out.push(cs[i + 1]);
                i += 2;
                continue;
            }
            if c != '$' || i + 1 == cs.len() {
                out.push(c);
                i += 1;
                continue;
            }
            let n = cs[i + 1];
            if n == '(' {
                // $(( … )) or $( … ): to the matching parenthesis.
                let mut depth = 0;
                let mut j = i + 1;
                while j < cs.len() {
                    match cs[j] {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let inner: String = cs[i + 2..j.min(cs.len())].iter().collect();
                if inner.starts_with('(') && inner.ends_with(')') {
                    out += &arith(self, &inner[1..inner.len() - 1])?.to_string();
                } else {
                    let text = self.capture(&inner);
                    out += text.trim_end_matches('\n');
                }
                i = j + 1;
            } else if n == '{' {
                let end = cs[i..].iter().position(|&c| c == '}').map(|p| i + p).ok_or("missing }")?;
                let name: String = cs[i + 2..end].iter().collect();
                out += &self.var(&name);
                i = end + 1;
            } else if n.is_ascii_alphabetic() || n == '_' {
                let mut j = i + 1;
                while j < cs.len() && (cs[j].is_ascii_alphanumeric() || cs[j] == '_') {
                    j += 1;
                }
                let name: String = cs[i + 1..j].iter().collect();
                out += &self.var(&name);
                i = j;
            } else if n.is_ascii_digit() || "?#@*".contains(n) {
                out += &self.var(&n.to_string());
                i += 2;
            } else {
                out.push('$');
                i += 1;
            }
        }
        Ok(out)
    }

    /// A word → its fields: expanded, unquoted expansions split at
    /// whitespace, unquoted `*` `?` matched against file names.
    fn expand_word(&mut self, w: &Word) -> Result<Vec<String>, String> {
        // (text, may split / glob)
        let mut fields: Vec<String> = alloc::vec![String::new()];
        let mut globbable = alloc::vec![false];
        let mut quoted = false;
        for p in &w.0 {
            match p {
                Part::Lit(s) => {
                    fields.last_mut().unwrap().push_str(s);
                    quoted = true;
                }
                Part::Dq(s) => {
                    let e = self.expand_text(s)?;
                    fields.last_mut().unwrap().push_str(&e);
                    quoted = true;
                }
                Part::Bare(s) => {
                    let had_dollar = s.contains('$');
                    let e = self.expand_text(s)?;
                    if had_dollar {
                        // The expansion's own whitespace separates fields.
                        let mut pieces = e.split(char::is_whitespace);
                        if e.starts_with(char::is_whitespace) && !fields.last().unwrap().is_empty() {
                            fields.push(String::new());
                            globbable.push(false);
                        }
                        fields.last_mut().unwrap().push_str(pieces.next().unwrap_or(""));
                        for piece in pieces {
                            fields.push(String::from(piece));
                            globbable.push(false);
                        }
                    } else {
                        fields.last_mut().unwrap().push_str(&e);
                    }
                    if s.contains(['*', '?']) {
                        *globbable.last_mut().unwrap() = true;
                    }
                }
            }
        }
        let only_empty = quoted && fields.iter().all(String::is_empty);
        let mut out = Vec::new();
        for (f, g) in fields.into_iter().zip(globbable) {
            if f.is_empty() {
                if only_empty && out.is_empty() {
                    out.push(f);
                }
                continue;
            }
            if g {
                let m = self.glob(&f);
                if !m.is_empty() {
                    out.extend(m);
                    continue;
                }
            }
            out.push(f);
        }
        Ok(out)
    }

    /// File names matching `pat` (a pattern in the last part only).
    fn glob(&self, pat: &str) -> Vec<String> {
        let Some(fs) = self.fs else { return Vec::new() };
        let (dir, last) = match pat.rfind('/') {
            Some(i) => (&pat[..i + 1], &pat[i + 1..]),
            None => ("", pat),
        };
        if dir.contains(['*', '?']) {
            return Vec::new();
        }
        let Ok(es) = fs.list(&self.path(if dir.is_empty() { "." } else { dir })) else { return Vec::new() };
        let p: Vec<char> = last.chars().collect();
        let mut out: Vec<String> = es
            .iter()
            .filter(|e| !e.name.starts_with('.') || last.starts_with('.'))
            .filter(|e| glob_match(&p, &e.name.chars().collect::<Vec<_>>()))
            .map(|e| format!("{dir}{}", e.name))
            .collect();
        out.sort();
        out
    }

    /// Runs `src` and gives back what it printed (`$(…)`).
    fn capture(&mut self, src: &str) -> String {
        self.captures.push(Vec::new());
        match parse(src) {
            Ok(l) => {
                let _ = self.run_list(&l);
            }
            Err(e) => self.complain("$(…)", &parse_error(e)),
        }
        String::from_utf8_lossy(&self.captures.pop().unwrap_or_default()).into_owned()
    }

    /// Output: to the innermost `$(…)`, else the console.
    pub fn emit(&mut self, b: &[u8]) {
        match self.captures.last_mut() {
            Some(c) => c.extend_from_slice(b),
            None => _ = self.con.write(b),
        }
    }

    fn complain(&mut self, who: &str, msg: &str) {
        let _ = self.con.write(format!("{who}: {msg}\n").as_bytes());
    }

    pub fn run_list(&mut self, l: &List) -> Flow {
        for (join, p) in &l.0 {
            let skip = match join {
                Join::Then => false,
                Join::And => self.status != 0,
                Join::Or => self.status == 0,
            };
            if skip {
                continue;
            }
            if let Flow::Exit(c) = self.run_pipeline(p) {
                return Flow::Exit(c);
            }
        }
        Flow::Next
    }

    fn run_pipeline(&mut self, p: &Pipeline) -> Flow {
        if let [c @ (Command::If(..) | Command::For(..) | Command::While(..))] = p.as_slice() {
            return self.run_compound(c);
        }
        let n = p.len();
        let mut data: Option<Vec<u8>> = None;
        for (i, c) in p.iter().enumerate() {
            let Command::Simple(s) = c else {
                self.complain("shell", "if / for / while cannot be part of a pipeline");
                self.status = 2;
                return Flow::Next;
            };
            let mut argv = Vec::new();
            for w in &s.words {
                match self.expand_word(w) {
                    Ok(f) => argv.extend(f),
                    Err(e) => {
                        self.complain("shell", &e);
                        self.status = 1;
                        return Flow::Next;
                    }
                }
            }
            let file = |sh: &mut Sh, w: &Word| -> Result<String, String> {
                let f = sh.expand_word(w)?;
                match f.as_slice() {
                    [one] => Ok(one.clone()),
                    _ => Err(String::from("one file name expected")),
                }
            };
            let input = match &s.input {
                Some(w) => match file(self, w).and_then(|f| self.fs()?.read(&self.path(&f)).map_err(|e| format!("{f}: {e:?}"))) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        self.complain("shell", &e);
                        self.status = 1;
                        return Flow::Next;
                    }
                },
                None => data.take(),
            };
            // NAME=value words (alone): variables.
            let assigns = argv.iter().take_while(|a| a.split_once('=').is_some_and(|(k, _)| is_name(k))).count();
            if assigns == argv.len() && !argv.is_empty() && s.words.iter().take(assigns).all(|w| matches!(w.0.first(), Some(Part::Bare(_)))) {
                for a in &argv {
                    let (k, v) = a.split_once('=').unwrap();
                    self.vars.insert(String::from(k), String::from(v));
                }
                self.status = 0;
                continue;
            }
            if argv.is_empty() {
                continue;
            }
            if argv[0] == "exit" {
                return Flow::Exit(argv.get(1).and_then(|x| x.parse().ok()).unwrap_or(self.status));
            }
            self.status = 0;
            self.piped_out = i + 1 < n || s.output.is_some() || !self.captures.is_empty();
            let out = match cmds::run(self, &argv, input) {
                Ok(o) => o,
                Err(e) => {
                    self.complain(&argv[0], &e);
                    self.status = if self.status == 0 { 1 } else { self.status };
                    if i + 1 < n {
                        data = Some(Vec::new());
                        continue;
                    }
                    return Flow::Next;
                }
            };
            if let Some(code) = self.exit_request.take() {
                return Flow::Exit(code);
            }
            match &s.output {
                Some((w, append)) => {
                    let r = file(self, w).and_then(|f| {
                        let p = self.path(&f);
                        let fs = self.fs()?;
                        let mut all = if *append { fs.read(&p).unwrap_or_default() } else { Vec::new() };
                        all.extend_from_slice(&out);
                        fs.write(&p, &all).map_err(|e| format!("{f}: {e:?}"))
                    });
                    if let Err(e) = r {
                        self.complain("shell", &e);
                        self.status = 1;
                        return Flow::Next;
                    }
                    data = Some(Vec::new());
                }
                None if i + 1 == n => self.emit(&out),
                None => data = Some(out),
            }
        }
        Flow::Next
    }

    fn run_compound(&mut self, c: &Command) -> Flow {
        match c {
            Command::If(arms, otherwise) => {
                for (cond, body) in arms {
                    if let Flow::Exit(c) = self.run_list(cond) {
                        return Flow::Exit(c);
                    }
                    if self.status == 0 {
                        return self.run_list(body);
                    }
                }
                match otherwise {
                    Some(b) => self.run_list(b),
                    None => {
                        self.status = 0;
                        Flow::Next
                    }
                }
            }
            Command::For(name, words, body) => {
                let items = match words {
                    Some(ws) => {
                        let mut items = Vec::new();
                        for w in ws {
                            match self.expand_word(w) {
                                Ok(f) => items.extend(f),
                                Err(e) => {
                                    self.complain("for", &e);
                                    self.status = 1;
                                    return Flow::Next;
                                }
                            }
                        }
                        items
                    }
                    None => self.args.get(1..).map(|a| a.to_vec()).unwrap_or_default(),
                };
                self.status = 0;
                for it in items {
                    self.vars.insert(name.clone(), it);
                    if let Flow::Exit(c) = self.run_list(body) {
                        return Flow::Exit(c);
                    }
                }
                Flow::Next
            }
            Command::While(cond, body) => {
                let mut rounds = 0u32;
                loop {
                    if let Flow::Exit(c) = self.run_list(cond) {
                        return Flow::Exit(c);
                    }
                    if self.status != 0 {
                        self.status = 0;
                        return Flow::Next;
                    }
                    if let Flow::Exit(c) = self.run_list(body) {
                        return Flow::Exit(c);
                    }
                    rounds += 1;
                    if rounds == 100_000 {
                        self.complain("while", "stopped after 100000 rounds");
                        self.status = 1;
                        return Flow::Next;
                    }
                }
            }
            Command::Simple(_) => Flow::Next,
        }
    }

    /// Runs a script (`sh FILE ARGS`, `. FILE`): with its own $0, $1… unless `here`.
    pub fn run_script(&mut self, src: &str, args: Option<Vec<String>>) -> Result<i64, String> {
        let l = parse(src).map_err(parse_error)?;
        let saved = args.map(|a| core::mem::replace(&mut self.args, a));
        let f = self.run_list(&l);
        if let Some(a) = saved {
            self.args = a;
        }
        match f {
            Flow::Exit(c) => Ok(c),
            Flow::Next => Ok(self.status),
        }
    }
}

pub fn parse_error(e: ParseError) -> String {
    match e {
        ParseError::Incomplete => String::from("unexpected end (an unfinished quote, if, for or while?)"),
        ParseError::Bad(m) => m,
    }
}

pub fn is_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
