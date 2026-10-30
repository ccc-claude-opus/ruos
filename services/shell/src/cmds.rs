//! The built-in commands. Each takes its arguments and the previous
//! command's output (a pipe or `<`) and gives back its own output, which
//! the shell prints, writes to a file or hands to the next command.
//! Commands that run other programs (run, a /bin program, dock) print
//! straight to the console.
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libsys::console::Console;
use libsys::dock::Dock;
use libsys::ns::{self, Namespace};
use libsys::{Channel, Handle, Memory, ObjType, Rights, cprintln};
use objstore::{Id, Index, Kind, Manifest};

use crate::Sh;

type Out = Result<Vec<u8>, String>;

const HELP: &str = "\
檔案（在 root 映像上，這次開機的可寫層裡）
  ls [-l] [PATH]   cd [DIR]   pwd   cat [FILE...]   tree [DIR]   stat PATH   find [DIR] [-name PAT] [-type f|d]
  mkdir [-p] DIR   rmdir DIR   rm [-r] PATH   cp FROM TO   mv FROM TO   touch FILE   basename   dirname
  save             把這次的變更 commit 進 root 映像（下次開機還在）
文字（可以接管線）
  echo [-n]   printf FMT ARGS   head/tail [-n N]   wc   grep [-i -v -c] WORD   sort [-r -n]   uniq [-c]
  cut -d D -f LIST   tr SET1 SET2   sed 's/A/B/g; /W/d'   seq   tee FILE   rev   tac   nl
shell 語言
  a | b   a > F   a >> F   a < F   a ; b   a && b   a || b   # 註解
  X=1   $X  ${X}  $?  $1 $#  $(指令)  $((1+2))   *.txt   '不展開'  \"展開 $X\"
  if 指令; then …; elif …; else …; fi   for X in a b c; do …; done   while 指令; do …; done
  test / [ -f F ] [ -d D ] [ A = B ] [ N -lt M ]   true   false   read X   export   unset   env
  sh 腳本 參數…   . 腳本   which 名稱
系統
  date   uptime   clear   history   !N   sleep 秒   help   exit [CODE]   ↑ ↓ 翻歷史
  ps（= dock ps）   kill POD（= dock stop）   其他名稱：執行 /bin 裡的程式（例如 hello）
能力（capability）：程式只能用別人交給它的 handle
  handles   ns   run IMAGE [NAME...]（只給列出的名稱，例如 run snoop console store）   mem
映像（內容定址，物件以 BLAKE3 雜湊命名）
  images   image NAME   ls IMAGE:DIR   cat IMAGE:FILE
pod（映像 + quota + 名稱空間 + 可寫層）
  dock run [-d] [--name N] [--mem SIZE] [--tasks N] IMAGE [ARGS]
  dock ps | stop POD | logs POD | rm POD | ls POD [DIR] | cat POD FILE
  dock write POD FILE TEXT | commit POD IMAGE | build POD PODFILE | push IMAGE
kube（宣告想要的狀態，kube 一直讓它成真；範例在 /home/kube）
  kube apply FILE...   kube get [pods|deploy|spaces]   kube describe NAME
  kube scale NAME N    kube delete NAME   kube logs POD
";

/// Flags (`-x`, `-n N`) and the other arguments.
struct Args<'a> {
    flags: Vec<char>,
    n: Option<usize>,
    rest: Vec<&'a str>,
}

fn args(argv: &[String]) -> Args<'_> {
    let mut a = Args { flags: Vec::new(), n: None, rest: Vec::new() };
    let mut i = 1;
    while i < argv.len() {
        let w = argv[i].as_str();
        if w == "-n" && i + 1 < argv.len() {
            a.n = argv[i + 1].parse().ok();
            i += 2;
            continue;
        }
        match w.strip_prefix('-') {
            Some(f) if !f.is_empty() && a.rest.is_empty() => a.flags.extend(f.chars()),
            _ => a.rest.push(w),
        }
        i += 1;
    }
    a
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn lines_out(ls: impl IntoIterator<Item = String>) -> Vec<u8> {
    let mut o = String::new();
    for l in ls {
        o += &l;
        o.push('\n');
    }
    o.into_bytes()
}

fn err<E: core::fmt::Debug>(what: &str) -> impl Fn(E) -> String + '_ {
    move |e| format!("{what}: {e:?}")
}

impl Sh {
    /// The input of a text command: its files, else what came in.
    fn input(&self, files: &[&str], input: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
        if files.is_empty() {
            return Ok(input.unwrap_or_default());
        }
        let fs = self.fs()?;
        let mut all = Vec::new();
        for f in files {
            all.extend(fs.read(&self.path(f)).map_err(err(f))?);
        }
        Ok(all)
    }

    fn is_dir(&self, p: &str) -> bool {
        self.fs.is_some_and(|fs| matches!(fs.stat(p), Ok((true, _))))
    }

    fn ls(&self, argv: &[String]) -> Out {
        let a = args(argv);
        let long = a.flags.contains(&'l');
        let paths = if a.rest.is_empty() { alloc::vec![self.cwd.as_str()] } else { a.rest.clone() };
        let mut out = String::new();
        for (k, p) in paths.iter().enumerate() {
            if p.contains(':') {
                out += &self.image_ls(p)?;
                continue;
            }
            let fs = self.fs()?;
            let full = self.path(p);
            let (dir, size) = fs.stat(&full).map_err(err(p))?;
            if !dir {
                out += &if long { format!("-  {:>8}  {}\n", size, p) } else { format!("{p}\n") };
                continue;
            }
            if paths.len() > 1 {
                out += &format!("{}{}:\n", if k > 0 { "\n" } else { "" }, p);
            }
            for e in fs.list(&full).map_err(err(p))? {
                if e.name.starts_with(".ruos") {
                    continue;
                }
                let name = if e.dir { format!("{}/", e.name) } else { e.name.clone() };
                out += &if long { format!("{}  {:>8}  {}\n", if e.dir { 'd' } else { '-' }, e.size, name) } else { format!("{name}\n") };
            }
        }
        Ok(out.into_bytes())
    }

    fn cd(&mut self, argv: &[String]) -> Out {
        let to = argv.get(1).map(String::as_str).unwrap_or("/home");
        let p = self.path(to);
        if !self.is_dir(&p) {
            return Err(format!("{to}: no such directory"));
        }
        self.cwd = p;
        Ok(Vec::new())
    }

    fn cat(&self, argv: &[String], input: Option<Vec<u8>>) -> Out {
        let files: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        if let [spec] = files.as_slice() {
            if spec.contains(':') {
                return self.image_cat(spec);
            }
        }
        self.input(&files, input)
    }

    fn tree(&self, argv: &[String]) -> Out {
        let fs = self.fs()?;
        let root = self.path(argv.get(1).map(String::as_str).unwrap_or("."));
        fn walk(fs: &libsys::fs::Fs, dir: &str, depth: usize, out: &mut String) -> Result<(), String> {
            for e in fs.list(dir).map_err(err(dir))? {
                if e.name.starts_with(".ruos") {
                    continue;
                }
                *out += &format!("{}{}{}\n", "  ".repeat(depth), e.name, if e.dir { "/" } else { "" });
                if e.dir {
                    walk(fs, &crate::join(dir, &e.name), depth + 1, out)?;
                }
            }
            Ok(())
        }
        let mut out = format!("{root}\n");
        walk(&fs, &root, 1, &mut out)?;
        Ok(out.into_bytes())
    }

    fn stat(&self, argv: &[String]) -> Out {
        let p = argv.get(1).ok_or("PATH")?;
        let (dir, size) = self.fs()?.stat(&self.path(p)).map_err(err(p))?;
        Ok(format!("{}: {}, {} bytes\n", self.path(p), if dir { "directory" } else { "file" }, size).into_bytes())
    }

    fn mkdir(&self, argv: &[String]) -> Out {
        let a = args(argv);
        let fs = self.fs()?;
        for d in &a.rest {
            let full = self.path(d);
            if a.flags.contains(&'p') {
                let mut at = String::new();
                for part in full.split('/').filter(|x| !x.is_empty()) {
                    at = format!("{at}/{part}");
                    if !self.is_dir(&at) {
                        fs.mkdir(&at).map_err(err(d))?;
                    }
                }
            } else {
                fs.mkdir(&full).map_err(err(d))?;
            }
        }
        Ok(Vec::new())
    }

    fn rm(&self, argv: &[String]) -> Out {
        let a = args(argv);
        let fs = self.fs()?;
        fn remove(fs: &libsys::fs::Fs, p: &str, recursive: bool) -> Result<(), String> {
            match fs.stat(p).map_err(err(p))? {
                (true, _) if !recursive => Err(format!("{p}: is a directory (rm -r)")),
                (true, _) => {
                    for e in fs.list(p).map_err(err(p))? {
                        if !e.name.starts_with(".ruos") {
                            remove(fs, &crate::join(p, &e.name), true)?;
                        }
                    }
                    fs.rmdir(p).map_err(err(p))
                }
                (false, _) => fs.unlink(p).map_err(err(p)),
            }
        }
        for p in &a.rest {
            remove(&fs, &self.path(p), a.flags.contains(&'r'))?;
        }
        Ok(Vec::new())
    }

    /// `cp` / `mv`: into a directory keeps the name.
    fn target(&self, from: &str, to: &str) -> String {
        let t = self.path(to);
        if self.is_dir(&t) { crate::join(&t, from.rsplit('/').next().unwrap_or(from)) } else { t }
    }

    fn cp(&self, argv: &[String], mv: bool) -> Out {
        let (Some(from), Some(to)) = (argv.get(1), argv.get(2)) else { return Err(String::from("FROM TO")) };
        let fs = self.fs()?;
        let (src, dst) = (self.path(from), self.target(from, to));
        if mv {
            fs.rename(&src, &dst).map_err(err(from))?;
        } else {
            if self.is_dir(&src) {
                return Err(format!("{from}: is a directory"));
            }
            fs.write(&dst, &fs.read(&src).map_err(err(from))?).map_err(err(to))?;
        }
        Ok(Vec::new())
    }

    fn touch(&self, argv: &[String]) -> Out {
        let fs = self.fs()?;
        for f in &argv[1..] {
            let p = self.path(f);
            if fs.stat(&p).is_err() {
                fs.write(&p, b"").map_err(err(f))?;
            }
        }
        Ok(Vec::new())
    }

    fn save(&self) -> Out {
        let id = self.fs()?.commit(crate::ROOT).map_err(err("save"))?;
        Ok(format!("saved into {}:latest {} (the next boot starts with these files)\n", crate::ROOT, &id[..12.min(id.len())]).into_bytes())
    }

    /// A program from /bin (or a path): its own console client and the
    /// arguments, nothing else.
    fn program(&self, argv: &[String]) -> Result<Option<i64>, String> {
        let fs = self.fs()?;
        let path = if argv[0].contains('/') { self.path(&argv[0]) } else { format!("/bin/{}", argv[0]) };
        if !matches!(fs.stat(&path), Ok((false, _))) {
            return Ok(None);
        }
        let (m, size) = fs.read_mem(&path).map_err(err("read"))?;
        let (mine, theirs) = Channel::create().map_err(err("channel"))?;
        self.con.connect(theirs.0).map_err(err("console"))?;
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        let ns = libsys::args::add(Namespace::new().add("console", mine.0), &args, &[]).map_err(err("args"))?;
        let name = path.rsplit('/').next().unwrap_or(&path);
        let r = self.store.spawn_module(m, size, name, &ns, None);
        let _ = m.0.close();
        let task = r.map_err(err("start"))?;
        let code = task.wait().map_err(err("wait"))?;
        let _ = task.close();
        Ok(Some(code))
    }
}

pub fn run(sh: &mut Sh, argv: &[String], input: Option<Vec<u8>>) -> Out {
    let a = args(argv);
    let con = sh.con;
    match argv[0].as_str() {
        "help" => Ok(HELP.as_bytes().to_vec()),
        "echo" => {
            let nl = !a.flags.contains(&'n');
            Ok(format!("{}{}", a.rest.join(" "), if nl { "\n" } else { "" }).into_bytes())
        }
        "pwd" => Ok(format!("{}\n", sh.cwd).into_bytes()),
        "cd" => sh.cd(argv),
        "ls" => sh.ls(argv),
        "cat" => sh.cat(argv, input),
        "tree" => sh.tree(argv),
        "stat" => sh.stat(argv),
        "mkdir" => sh.mkdir(argv),
        "rmdir" => {
            let fs = sh.fs()?;
            for d in &a.rest {
                fs.rmdir(&sh.path(d)).map_err(err(d))?;
            }
            Ok(Vec::new())
        }
        "rm" => sh.rm(argv),
        "cp" => sh.cp(argv, false),
        "mv" => sh.cp(argv, true),
        "touch" => sh.touch(argv),
        "save" => sh.save(),
        "head" | "tail" => {
            let t = text(&sh.input(&a.rest, input)?);
            let ls: Vec<&str> = t.lines().collect();
            let n = a.n.unwrap_or(10).min(ls.len());
            let pick = if argv[0] == "head" { &ls[..n] } else { &ls[ls.len() - n..] };
            Ok(lines_out(pick.iter().map(|l| l.to_string())))
        }
        "wc" => {
            let b = sh.input(&a.rest, input)?;
            let t = text(&b);
            let name = if a.rest.len() == 1 { format!(" {}", a.rest[0]) } else { String::new() };
            let counts = [('l', t.lines().count()), ('w', t.split_whitespace().count()), ('c', b.len())];
            let only: Vec<usize> = counts.iter().filter(|(f, _)| a.flags.contains(f)).map(|&(_, n)| n).collect();
            // wc -l alone: just the number (handy in $(…)).
            if only.len() == 1 {
                return Ok(format!("{}{}\n", only[0], name).into_bytes());
            }
            let show: Vec<usize> = if only.is_empty() { counts.iter().map(|&(_, n)| n).collect() } else { only };
            Ok(format!("{}{}\n", show.iter().map(|n| format!("{n:>7}")).collect::<Vec<_>>().join(" "), name).into_bytes())
        }
        "grep" => {
            let Some((pat, files)) = a.rest.split_first() else { return Err(String::from("[-i] [-v] [-c] WORD [FILE...]")) };
            let t = text(&sh.input(files, input)?);
            let fold = a.flags.contains(&'i');
            let pat = if fold { pat.to_lowercase() } else { pat.to_string() };
            // A word, with ^ (at the start) and $ (at the end); no other patterns.
            let (start, rest) = match pat.strip_prefix('^') { Some(r) => (true, r), None => (false, pat.as_str()) };
            let (end, word) = match rest.strip_suffix('$') { Some(r) => (true, r), None => (false, rest) };
            let matches = |l: &str| match (start, end) {
                (true, true) => l == word,
                (true, false) => l.starts_with(word),
                (false, true) => l.ends_with(word),
                _ => l.contains(word),
            };
            let hit = |l: &str| (if fold { matches(&l.to_lowercase()) } else { matches(l) }) != a.flags.contains(&'v');
            let found: Vec<String> = t.lines().filter(|l| hit(l)).map(String::from).collect();
            if found.is_empty() {
                sh.status = 1;
            }
            if a.flags.contains(&'c') { Ok(format!("{}\n", found.len()).into_bytes()) } else { Ok(lines_out(found)) }
        }
        "sort" => {
            let t = text(&sh.input(&a.rest, input)?);
            let mut ls: Vec<String> = t.lines().map(String::from).collect();
            if a.flags.contains(&'n') {
                let num = |l: &String| l.trim().split_whitespace().next().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
                ls.sort_by_key(num);
            } else {
                ls.sort();
            }
            if a.flags.contains(&'r') {
                ls.reverse();
            }
            Ok(lines_out(ls))
        }
        "uniq" => {
            let t = text(&sh.input(&a.rest, input)?);
            let mut out: Vec<(usize, String)> = Vec::new();
            for l in t.lines() {
                match out.last_mut() {
                    Some((n, last)) if last == l => *n += 1,
                    _ => out.push((1, String::from(l))),
                }
            }
            let count = a.flags.contains(&'c');
            Ok(lines_out(out.into_iter().map(|(n, l)| if count { format!("{n:>7} {l}") } else { l })))
        }
        "date" => Ok(format!("{}\n", date(libsys::realtime_ns() / 1_000_000_000)).into_bytes()),
        "uptime" => {
            let s = libsys::clock_ns() / 1_000_000_000;
            Ok(format!("up {}h {}m {}s\n", s / 3600, s / 60 % 60, s % 60).into_bytes())
        }
        "clear" => Ok(b"\x1b[2J\x1b[H".to_vec()),
        "history" => Ok(lines_out(sh.history.iter().enumerate().map(|(i, l)| format!("{:>4}  {}", i + 1, l)))),
        "sleep" => {
            // Seconds, like Linux (0.5 works too).
            let secs: f64 = a.rest.first().and_then(|x| x.parse().ok()).ok_or("SECONDS")?;
            libsys::sleep_ms((secs * 1000.0) as u64);
            Ok(Vec::new())
        }
        "true" => Ok(Vec::new()),
        "false" => {
            sh.status = 1;
            Ok(Vec::new())
        }
        "test" | "[" => {
            let mut v: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
            if argv[0] == "[" {
                if v.last() != Some(&"]") {
                    return Err(String::from("missing ]"));
                }
                v.pop();
            }
            if !test(sh, &v)? {
                sh.status = 1;
            }
            Ok(Vec::new())
        }
        "cut" => cut(sh, argv, input),
        "tr" => {
            let del = argv.get(1).is_some_and(|x| x == "-d");
            let sets: Vec<Vec<char>> = argv[1 + del as usize..].iter().map(|x| set(x)).collect();
            let t = text(&input.unwrap_or_default());
            let out: String = match (del, sets.as_slice()) {
                (true, [d]) => t.chars().filter(|c| !d.contains(c)).collect(),
                (false, [from, to]) if !to.is_empty() => {
                    t.chars().map(|c| from.iter().position(|&f| f == c).map_or(c, |i| to[i.min(to.len() - 1)])).collect()
                }
                _ => return Err(String::from("SET1 SET2 | -d SET")),
            };
            Ok(out.into_bytes())
        }
        "sed" => sed(sh, argv, input),
        "seq" => {
            let n: Vec<i64> = a.rest.iter().map(|x| x.parse().map_err(|_| format!("{x}: not a number"))).collect::<Result<_, _>>()?;
            let (first, step, last) = match n.as_slice() {
                [l] => (1, 1, *l),
                [f, l] => (*f, 1, *l),
                [f, s, l] if *s != 0 => (*f, *s, *l),
                _ => return Err(String::from("[FIRST [STEP]] LAST")),
            };
            let mut out = Vec::new();
            let mut x = first;
            while (step > 0 && x <= last) || (step < 0 && x >= last) {
                out.push(x.to_string());
                x += step;
                if out.len() > 100_000 {
                    break;
                }
            }
            Ok(lines_out(out))
        }
        "tee" => {
            let data = input.unwrap_or_default();
            let fs = sh.fs()?;
            for f in &a.rest {
                let p = sh.path(f);
                let mut all = if a.flags.contains(&'a') { fs.read(&p).unwrap_or_default() } else { Vec::new() };
                all.extend_from_slice(&data);
                fs.write(&p, &all).map_err(err(f))?;
            }
            Ok(data)
        }
        "basename" => {
            let p = a.rest.first().ok_or("PATH [SUFFIX]")?.trim_end_matches('/');
            let b = p.rsplit('/').next().unwrap_or(p);
            let b = a.rest.get(1).and_then(|s| b.strip_suffix(s)).filter(|x| !x.is_empty()).unwrap_or(b);
            Ok(format!("{b}\n").into_bytes())
        }
        "dirname" => {
            let p = a.rest.first().ok_or("PATH")?.trim_end_matches('/');
            let d = match p.rfind('/') {
                Some(0) => "/",
                Some(i) => &p[..i],
                None => ".",
            };
            Ok(format!("{d}\n").into_bytes())
        }
        "printf" => Ok(printf(&argv[1..]).into_bytes()),
        "env" | "set" => Ok(lines_out(sh.vars.iter().map(|(k, v)| format!("{k}={v}")))),
        "export" => {
            for x in &argv[1..] {
                if let Some((k, v)) = x.split_once('=') {
                    sh.vars.insert(String::from(k), String::from(v));
                }
            }
            Ok(Vec::new())
        }
        "unset" => {
            for x in &argv[1..] {
                sh.vars.remove(x);
            }
            Ok(Vec::new())
        }
        "read" => {
            let line = match input {
                Some(b) => text(&b).lines().next().map(String::from),
                None => {
                    let mut buf = [0u8; libsys::console::MAX_LINE];
                    let n = sh.con.read_line(&mut buf).map_err(err("console"))?;
                    Some(text(&buf[..n]))
                }
            };
            let Some(line) = line else {
                sh.status = 1;
                return Ok(Vec::new());
            };
            let names = &argv[1..];
            let mut words = line.split_whitespace();
            for (i, n) in names.iter().enumerate() {
                let v = if i + 1 == names.len() { words.by_ref().collect::<Vec<_>>().join(" ") } else { String::from(words.next().unwrap_or("")) };
                sh.vars.insert(n.clone(), v);
            }
            Ok(Vec::new())
        }
        "find" => find(sh, argv),
        "rev" => Ok(lines_out(text(&sh.input(&a.rest, input)?).lines().map(|l| l.chars().rev().collect()))),
        "tac" => Ok(lines_out(text(&sh.input(&a.rest, input)?).lines().rev().map(String::from))),
        "nl" => Ok(lines_out(text(&sh.input(&a.rest, input)?).lines().enumerate().map(|(i, l)| format!("{:>6}\t{}", i + 1, l)))),
        "which" | "type" => {
            let mut out = String::new();
            for n in &a.rest {
                if BUILTINS.contains(n) {
                    out += &format!("{n}: shell built-in\n");
                } else if sh.fs.is_some_and(|fs| matches!(fs.stat(&format!("/bin/{n}")), Ok((false, _)))) {
                    out += &format!("/bin/{n}\n");
                } else {
                    sh.status = 1;
                    out += &format!("{n}: not found\n");
                }
            }
            Ok(out.into_bytes())
        }
        "sh" | "." | "source" => {
            let f = a.rest.first().ok_or("FILE [ARGS]")?;
            let src = text(&sh.fs()?.read(&sh.path(f)).map_err(err(f))?);
            let here = argv[0] != "sh";
            let args = (!here).then(|| argv[1..].to_vec());
            let code = sh.run_script(&src, args)?;
            sh.status = code;
            Ok(Vec::new())
        }
        "ps" => {
            sh.dock("ps");
            Ok(Vec::new())
        }
        "kill" => {
            for p in &a.rest {
                sh.dock(&format!("stop {p}"));
            }
            Ok(Vec::new())
        }
        "kube" => sh.kube(&argv[1..]),
        "handles" => Ok(handles().into_bytes()),
        "ns" => Ok(lines_out(ns::names().map(|n| format!("{:<10} handle {}", n, ns::get(n).map(|h| h.0).unwrap_or(0))))),
        "run" => {
            run_image(sh, argv);
            Ok(Vec::new())
        }
        "mem" => Ok(mem().into_bytes()),
        "images" => sh.store.list().map_err(err("images")),
        "image" => sh.image(a.rest.first().copied().unwrap_or("")),
        _ => match sh.program(argv)? {
            Some(0) => Ok(Vec::new()),
            Some(code) => {
                cprintln!(con, "[{} exited with {}]", argv[0], code);
                sh.status = code;
                Ok(Vec::new())
            }
            None => Err(String::from("unknown command (type help)")),
        },
    }
}

/// Unix seconds as a UTC date (days to a civil date: Howard Hinnant's algorithm).
fn date(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, rem / 60 % 60, rem % 60)
}

fn type_name(t: ObjType) -> &'static str {
    match t {
        ObjType::Task => "task",
        ObjType::Thread => "thread",
        ObjType::Channel => "channel",
        ObjType::Memory => "memory",
        ObjType::Port => "port",
        ObjType::Quota => "quota",
        ObjType::Serial => "serial",
        ObjType::Mmio => "mmio",
        ObjType::Irq => "irq",
    }
}

/// Rights as letters: d(up) t(ransfer) w(ait) r(ead) W(rite) m(anage).
fn rights(r: Rights) -> String {
    [(Rights::DUP, 'd'), (Rights::TRANSFER, 't'), (Rights::WAIT, 'w'), (Rights::READ, 'r'), (Rights::WRITE, 'W'), (Rights::MANAGE, 'm')]
        .iter()
        .map(|&(b, c)| if r.has(b) { c } else { '-' })
        .collect()
}

/// The handle table, as far as it goes: a handle is just an index.
fn handles() -> String {
    let mut out = format!("{:<4}{:<9}{:<8}{}\n", "#", "TYPE", "RIGHTS", "NAME");
    let mut misses = 0;
    for i in 0..1024u32 {
        match Handle(i).info() {
            Ok((t, r)) => {
                misses = 0;
                let name = ns::names().find(|n| ns::get(n) == Some(Handle(i))).unwrap_or("");
                out += &format!("{:<4}{:<9}{:<8}{}\n", i, type_name(t), rights(r), name);
            }
            Err(_) => {
                misses += 1;
                if misses > 32 {
                    break;
                }
            }
        }
    }
    out
}

/// `run IMAGE [NAME...]`: the program gets the named capabilities and
/// nothing else. A console or store is a new client of the service (the
/// shell keeps its own); `fs` a new client of the shell's files; anything
/// else is a duplicate.
fn run_image(sh: &Sh, argv: &[String]) {
    let Some(image) = argv.get(1) else { return cprintln!(sh.con, "run: IMAGE [NAME...]") };
    let mut give = Namespace::new();
    for n in &argv[2..] {
        let h = match n.as_str() {
            "console" => Channel::create().ok().and_then(|(mine, theirs)| {
                sh.con.connect(theirs.0).ok()?;
                Some(mine.0)
            }),
            "store" => sh.store.connect().ok().map(|s| s.0.0),
            "fs" => sh.fs.and_then(|fs| fs.connect().ok()).map(|f| f.0.0),
            n => ns::get(n).and_then(|h| h.dup(Rights::ALL).ok()),
        };
        match h {
            Some(h) => give = give.add(n, h),
            None => return cprintln!(sh.con, "run: the shell has no {}", n),
        }
    }
    match sh.store.spawn(image, &give, None) {
        Ok(t) => match t.wait() {
            Ok(code) => cprintln!(sh.con, "[{} exited with {}]", image, code),
            Err(e) => cprintln!(sh.con, "run: wait: {:?}", e),
        },
        Err(e) => cprintln!(sh.con, "run: {}: {:?}", image, e),
    }
}

impl Sh {
    fn index(&self, name: &str) -> libsys::Result<(Id, Index)> {
        let id = Id::parse(&self.store.index(name)?).ok_or(libsys::Error::InvalidArgs)?;
        let x = Index::decode(&self.store.object(&format!("{id}"))?).map_err(|_| libsys::Error::InvalidArgs)?;
        Ok((id, x))
    }

    fn image(&self, name: &str) -> Out {
        let (id, x) = self.index(name).map_err(err(name))?;
        let mut out = format!("{} index {}\n", name, id.short());
        for (arch, m) in &x.manifests {
            let Ok(Ok(man)) = self.store.object(&format!("{m}")).map(|b| Manifest::decode(&b)) else { continue };
            out += &format!("  {:<8} manifest {}  entry {}\n", arch, m.short(), man.entry);
            for (i, l) in man.layers.iter().enumerate() {
                out += &format!("           layer {} {}\n", i, l.short());
            }
        }
        Ok(out.into_bytes())
    }

    /// `IMAGE[:PATH]` → the image's layers here, and the path.
    fn layers(&self, spec: &str) -> libsys::Result<(Vec<Id>, String)> {
        let (name, path) = spec.split_once(':').unwrap_or((spec, "/"));
        Ok((libsys::image::manifest(&self.store, name)?.layers, String::from(if path.is_empty() { "/" } else { path })))
    }

    fn image_ls(&self, spec: &str) -> Result<String, String> {
        let es = self.layers(spec).and_then(|(l, p)| libsys::image::list(&self.store, &l, &p)).map_err(err(spec))?;
        let mut out = String::new();
        for e in es.iter().filter(|e| !e.name.starts_with(".ruos")) {
            let kind = match e.kind {
                Kind::Dir => "dir ",
                Kind::Exec => "exec",
                _ => "file",
            };
            out += &format!("{} {:>8} {}  {}\n", kind, e.size, e.id.short(), e.name);
        }
        Ok(out)
    }

    fn image_cat(&self, spec: &str) -> Out {
        let (l, p) = self.layers(spec).map_err(err(spec))?;
        let e = libsys::image::lookup(&self.store, &l, &p).map_err(err(spec))?;
        if e.kind == Kind::Dir {
            return Err(format!("{spec}: is a directory"));
        }
        let b = self.store.object(&format!("{}", e.id)).map_err(err(spec))?;
        if b.iter().take(512).any(|&c| c == 0) {
            return Ok(format!("({} bytes of binary)\n", b.len()).into_bytes());
        }
        Ok(b)
    }
}

impl Sh {
    /// kube: `apply` sends the files' text; the rest goes as it is.
    fn kube(&self, argv: &[String]) -> Out {
        let k = self.kube.ok_or("kube: not available")?;
        let ask = |msg: &str| -> Result<String, String> {
            let r = k.request(msg).map_err(err("kube"))?;
            match r.strip_prefix("error ") {
                Some(m) => Err(format!("kube: {}", m.trim_end())),
                None if r.is_empty() || r.ends_with('\n') => Ok(r),
                None => Ok(r + "\n"),
            }
        };
        if argv.first().map(String::as_str) != Some("apply") {
            let words: Vec<&str> = argv.iter().map(String::as_str).collect();
            return ask(&words.join(" ")).map(String::into_bytes);
        }
        let files: Vec<&String> = argv[1..].iter().filter(|a| *a != "-f").collect();
        if files.is_empty() {
            return Err(String::from("kube apply FILE..."));
        }
        let mut out = String::new();
        for f in files {
            let src = self.fs()?.read(&self.path(f)).map_err(err(f))?;
            out += &ask(&format!("apply\n{}", text(&src))).map_err(|m| format!("{m} ({f})"))?;
        }
        Ok(out.into_bytes())
    }
}

pub fn dock_cmd(con: &Console, d: Dock, cmd: &str) {
    let r = match d.request(cmd) {
        Ok(r) => r,
        Err(e) => return cprintln!(con, "dock: {:?}", e),
    };
    let words: Vec<&str> = r.split_whitespace().collect();
    match (cmd.split_whitespace().next(), words.as_slice()) {
        (_, ["error", ..]) => cprintln!(con, "dock: {}", &r[6..]),
        (Some("run"), ["ok", id, name]) => {
            if cmd.split_whitespace().any(|w| w == "-d" || w == "--detach") {
                cprintln!(con, "started {} (pod {})", name, id);
            } else {
                match d.request(&format!("wait {id}")) {
                    Ok(w) => cprintln!(con, "[{} {}]", name, w.replace("exit", "exited with")),
                    Err(e) => cprintln!(con, "dock: {:?}", e),
                }
            }
        }
        (Some("stop"), ["ok", "stopped", name]) => cprintln!(con, "stopped {}", name),
        (Some("commit"), ["ok", "committed", r, id, files, removed]) => {
            cprintln!(con, "committed {} {} ({} files, {} removed)", r, &id[..12.min(id.len())], files, removed)
        }
        (Some("push"), ["ok", ..]) => cprintln!(con, "{}", &r[3..]),
        (Some("build"), ["ok", "built", r, id, n, from]) => {
            cprintln!(con, "built {} {} ({} file(s) on {})", r, &id[..12.min(id.len())], n, from)
        }
        (Some("rm"), ["ok", "removed", name]) => cprintln!(con, "removed {}", name),
        (Some("write"), ["ok"]) => {}
        _ => _ = con.write(r.as_bytes()),
    }
}

/// A copy-on-write demonstration.
fn mem() -> String {
    let r = (|| -> libsys::Result<([u8; 5], [u8; 5])> {
        let m = Memory::create(4096)?;
        m.write(0, b"hello")?;
        let c = m.clone_cow()?;
        c.write(0, b"HELLO")?;
        let (mut a, mut b) = ([0u8; 5], [0u8; 5]);
        m.read(0, &mut a)?;
        c.read(0, &mut b)?;
        Ok((a, b))
    })();
    match r {
        Ok((a, b)) => format!(
            "mem: original {:?}, clone {:?} (copy on write)\n",
            core::str::from_utf8(&a).unwrap_or("?"),
            core::str::from_utf8(&b).unwrap_or("?")
        ),
        Err(e) => format!("mem: {e:?}\n"),
    }
}

/// The built-in commands (for `which`).
const BUILTINS: &[&str] = &[
    "help", "echo", "pwd", "cd", "ls", "cat", "tree", "stat", "mkdir", "rmdir", "rm", "cp", "mv", "touch", "save", "head", "tail", "wc",
    "grep", "sort", "uniq", "cut", "tr", "sed", "seq", "tee", "basename", "dirname", "printf", "env", "set", "export", "unset", "read",
    "find", "rev", "tac", "nl", "which", "type", "sh", ".", "source", "test", "[", "true", "false", "date", "uptime", "clear", "history",
    "sleep", "ps", "kill", "exit", "handles", "ns", "run", "mem", "images", "image", "dock", "kube",
];

/// test / [ : -e -f -d -s -z -n, = !=, -eq -ne -lt -le -gt -ge, !.
fn test(sh: &Sh, v: &[&str]) -> Result<bool, String> {
    let num = |x: &str| x.trim().parse::<i64>().map_err(|_| format!("{x}: not a number"));
    let stat = |p: &str| sh.fs.and_then(|fs| fs.stat(&sh.path(p)).ok());
    Ok(match v {
        [] => false,
        ["!", rest @ ..] => !test(sh, rest)?,
        [s] => !s.is_empty(),
        ["-e", p] => stat(p).is_some(),
        ["-f", p] => stat(p).is_some_and(|(d, _)| !d),
        ["-d", p] => stat(p).is_some_and(|(d, _)| d),
        ["-s", p] => stat(p).is_some_and(|(d, n)| !d && n > 0),
        ["-z", s] => s.is_empty(),
        ["-n", s] => !s.is_empty(),
        [a, "=" | "==", b] => a == b,
        [a, "!=", b] => a != b,
        [a, "-eq", b] => num(a)? == num(b)?,
        [a, "-ne", b] => num(a)? != num(b)?,
        [a, "-lt", b] => num(a)? < num(b)?,
        [a, "-le", b] => num(a)? <= num(b)?,
        [a, "-gt", b] => num(a)? > num(b)?,
        [a, "-ge", b] => num(a)? >= num(b)?,
        _ => return Err(format!("cannot test {}", v.join(" "))),
    })
}

/// A tr set: characters and ranges (a-z), \n and \t.
fn set(s: &str) -> Vec<char> {
    let cs: Vec<char> = s.replace("\\n", "\n").replace("\\t", "\t").chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if i + 2 < cs.len() && cs[i + 1] == '-' {
            let (a, b) = (cs[i] as u32, cs[i + 2] as u32);
            out.extend((a..=b).filter_map(char::from_u32));
            i += 3;
        } else {
            out.push(cs[i]);
            i += 1;
        }
    }
    out
}

/// Which fields (or characters) a cut list names: 1,3 2-4 -2 3-.
fn cut_list(s: &str) -> Result<Vec<(usize, usize)>, String> {
    s.split(',')
        .map(|r| {
            let n = |x: &str, d: usize| if x.is_empty() { Ok(d) } else { x.parse::<usize>().map_err(|_| format!("{s}: bad list")) };
            match r.split_once('-') {
                Some((a, b)) => Ok((n(a, 1)?, n(b, usize::MAX)?)),
                None => Ok((n(r, 1)?, n(r, 1)?)),
            }
        })
        .collect()
}

/// cut -d D -f LIST | -c LIST [FILE...]
fn cut(sh: &Sh, argv: &[String], input: Option<Vec<u8>>) -> Out {
    let (mut delim, mut fields, mut chars, mut files) = ('\t', None, None, Vec::new());
    let mut i = 1;
    while i < argv.len() {
        let w = argv[i].as_str();
        let mut value = |flag: &str| -> Option<String> {
            match w.strip_prefix(flag) {
                Some("") => {
                    i += 1;
                    argv.get(i).cloned()
                }
                Some(v) => Some(String::from(v)),
                None => None,
            }
        };
        if let Some(d) = value("-d") {
            delim = d.chars().next().unwrap_or('\t');
        } else if let Some(f) = value("-f") {
            fields = Some(cut_list(&f)?);
        } else if let Some(c) = value("-c") {
            chars = Some(cut_list(&c)?);
        } else {
            files.push(w);
        }
        i += 1;
    }
    let t = text(&sh.input(&files, input)?);
    let pick = |n: usize, list: &[(usize, usize)]| list.iter().any(|&(a, b)| n >= a && n <= b);
    let out = match (fields, chars) {
        (Some(l), None) => t
            .lines()
            .map(|line| {
                if !line.contains(delim) {
                    return String::from(line);
                }
                let d = delim.to_string();
                line.split(delim).enumerate().filter(|(i, _)| pick(i + 1, &l)).map(|(_, f)| f).collect::<Vec<_>>().join(&d)
            })
            .collect::<Vec<_>>(),
        (None, Some(l)) => t.lines().map(|line| line.chars().enumerate().filter(|(i, _)| pick(i + 1, &l)).map(|(_, c)| c).collect()).collect(),
        _ => return Err(String::from("-f LIST [-d D] | -c LIST")),
    };
    Ok(lines_out(out))
}

/// sed [-n] SCRIPT [FILE...]: commands `s/from/to/[g]`, `d`, `p`, each
/// with an optional address (a line number, `$`, or /word/), joined by `;`.
/// Words are plain text, not regular expressions.
fn sed(sh: &Sh, argv: &[String], input: Option<Vec<u8>>) -> Out {
    let quiet = argv.get(1).is_some_and(|x| x == "-n");
    let rest = &argv[1 + quiet as usize..];
    let (script, files) = rest.split_first().ok_or("[-n] SCRIPT [FILE...]")?;
    enum Addr {
        All,
        Line(usize),
        Last,
        Word(String),
    }
    enum Op {
        Sub(String, String, bool),
        Del,
        Print,
    }
    let mut cmds: Vec<(Addr, Op)> = Vec::new();
    for c in script.split(';').map(str::trim).filter(|c| !c.is_empty()) {
        let (addr, c) = if let Some(r) = c.strip_prefix('/') {
            let end = r.find('/').ok_or("unfinished /word/")?;
            (Addr::Word(String::from(&r[..end])), &r[end + 1..])
        } else if let Some(r) = c.strip_prefix('$') {
            (Addr::Last, r)
        } else {
            let n = c.chars().take_while(|c| c.is_ascii_digit()).count();
            if n > 0 { (Addr::Line(c[..n].parse().unwrap_or(0)), &c[n..]) } else { (Addr::All, c) }
        };
        let op = match c.chars().next() {
            Some('d') => Op::Del,
            Some('p') => Op::Print,
            Some('s') => {
                let sep = c[1..].chars().next().ok_or("s/FROM/TO/")?;
                let parts: Vec<&str> = c[1 + sep.len_utf8()..].split(sep).collect();
                match parts.as_slice() {
                    [from, to, flags] if !from.is_empty() => Op::Sub(String::from(*from), String::from(*to), flags.contains('g')),
                    _ => return Err(String::from("s/FROM/TO/[g]")),
                }
            }
            _ => return Err(format!("{c}: commands are s, d and p")),
        };
        cmds.push((addr, op));
    }
    let t = text(&sh.input(&files.iter().map(String::as_str).collect::<Vec<_>>(), input)?);
    let lines: Vec<&str> = t.lines().collect();
    let mut out = Vec::new();
    'line: for (i, l) in lines.iter().enumerate() {
        let mut l = String::from(*l);
        for (addr, op) in &cmds {
            let at = match addr {
                Addr::All => true,
                Addr::Line(n) => i + 1 == *n,
                Addr::Last => i + 1 == lines.len(),
                Addr::Word(w) => l.contains(w.as_str()),
            };
            if !at {
                continue;
            }
            match op {
                Op::Del => continue 'line,
                Op::Print => out.push(l.clone()),
                Op::Sub(from, to, all) => l = if *all { l.replace(from.as_str(), to) } else { l.replacen(from.as_str(), to, 1) },
            }
        }
        if !quiet {
            out.push(l);
        }
    }
    Ok(lines_out(out))
}

/// printf FORMAT ARGS: %s %d %%, \n \t \\; the format is reused while arguments are left.
fn printf(argv: &[String]) -> String {
    let Some((fmt, mut args)) = argv.split_first() else { return String::new() };
    let mut out = String::new();
    loop {
        let mut used = 0;
        let mut cs = fmt.chars().peekable();
        while let Some(c) = cs.next() {
            match (c, cs.peek().copied()) {
                ('\\', Some(n)) => {
                    cs.next();
                    out.push(match n {
                        'n' => '\n',
                        't' => '\t',
                        c => c,
                    });
                }
                ('%', Some('%')) => {
                    cs.next();
                    out.push('%');
                }
                ('%', Some(f @ ('s' | 'd'))) => {
                    cs.next();
                    let v = args.get(used).map(String::as_str).unwrap_or("");
                    used += 1;
                    if f == 'd' {
                        out += &v.trim().parse::<i64>().unwrap_or(0).to_string();
                    } else {
                        out += v;
                    }
                }
                (c, _) => out.push(c),
            }
        }
        if used == 0 || used >= args.len() {
            return out;
        }
        args = &args[used..];
    }
}

/// find [DIR] [-name PATTERN] [-type f|d]
fn find(sh: &Sh, argv: &[String]) -> Out {
    let fs = sh.fs()?;
    let (mut dir, mut name, mut kind) = (String::from("."), None, None);
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "-name" => {
                i += 1;
                name = argv.get(i).map(|p| p.chars().collect::<Vec<_>>());
            }
            "-type" => {
                i += 1;
                kind = argv.get(i).and_then(|t| t.chars().next());
            }
            d => dir = String::from(d),
        }
        i += 1;
    }
    let shown = dir.clone();
    let mut out = Vec::new();
    fn walk(
        fs: &libsys::fs::Fs,
        full: &str,
        shown: &str,
        name: &Option<Vec<char>>,
        kind: Option<char>,
        out: &mut Vec<String>,
    ) -> Result<(), String> {
        for e in fs.list(full).map_err(err(shown))? {
            if e.name.starts_with(".ruos") {
                continue;
            }
            let path = if shown.ends_with('/') { format!("{shown}{}", e.name) } else { format!("{shown}/{}", e.name) };
            let name_ok = name.as_ref().is_none_or(|p| crate::lang::glob_match(p, &e.name.chars().collect::<Vec<_>>()));
            let kind_ok = match kind {
                Some('f') => !e.dir,
                Some('d') => e.dir,
                _ => true,
            };
            if name_ok && kind_ok {
                out.push(path.clone());
            }
            if e.dir {
                walk(fs, &crate::join(full, &e.name), &path, name, kind, out)?;
            }
        }
        Ok(())
    }
    walk(&fs, &sh.path(&dir), &shown, &name, kind, &mut out)?;
    Ok(lines_out(out))
}
