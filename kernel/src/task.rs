//! Tasks (an address space + a handle table) and threads.
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use abi::{Error, Rights};

use crate::arch::{self, AddrSpace};
use crate::object::{Handle, HandleTable, Object};
use crate::quota::{Charge, Quota, Res};
use crate::sched;
use crate::signal::SignalCell;
use crate::sync::IrqLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Run {
    Ready,
    Running,
    Blocked,
    Dead,
}

/// A thread waiting for an exit code, to be stored at `ptr` in its task.
pub struct Waiter {
    pub thread: Arc<Thread>,
    pub ptr: u64,
}

pub struct Task {
    pub id: u32,
    pub name: String,
    /// What this task (and the tasks it starts, by default) is charged to.
    pub quota: Arc<Quota>,
    /// Refunded when the task ends (handles may keep the `Task` longer).
    charge: IrqLock<Option<Charge>>,
    /// Threads of this task on a CPU right now.
    pub running: AtomicU32,
    /// Ended, memory not released yet (a thread was still on a CPU).
    pending_release: AtomicBool,
    pub aspace: Arc<AddrSpace>,
    pub handles: IrqLock<HandleTable>,
    state: IrqLock<TaskState>,
    pub sig: SignalCell,
}

struct TaskState {
    threads: Vec<Arc<Thread>>,
    exit: Option<i64>,
    waiters: Vec<Waiter>,
}

/// Where a thread starts: `entry` is an address (native) or a function
/// table index (wasm32; `MAIN_ENTRY` = the program's own entry).
#[derive(Clone, Copy)]
pub struct Start {
    pub entry: u64,
    pub stack: u64,
    pub arg: u64,
}

pub const MAIN_ENTRY: u64 = u64::MAX;

pub struct Thread {
    pub id: u32,
    pub task: Arc<Task>,
    pub start: Start,
    pub state: IrqLock<ThreadState>,
    pub sig: SignalCell,
    charge: IrqLock<Option<Charge>>,
}

pub struct ThreadState {
    pub run: Run,
    /// Bumped by every block: tells a pending wakeup whether it is stale.
    pub wgen: u64,
    exit: Option<i64>,
    waiters: Vec<Waiter>,
}

static NEXT_TASK: AtomicU32 = AtomicU32::new(1);
static NEXT_THREAD: AtomicU32 = AtomicU32::new(1);
static TASKS: IrqLock<BTreeMap<u32, Arc<Task>>> = IrqLock::new(BTreeMap::new());
static THREADS: IrqLock<BTreeMap<u32, Arc<Thread>>> = IrqLock::new(BTreeMap::new());

pub fn thread_by_id(id: u32) -> Option<Arc<Thread>> {
    THREADS.lock().get(&id).cloned()
}

/// Starts the program `image` as task `name` with `handles` (numbered
/// 0, 1, ... in the new task).
pub fn spawn(name: &str, image: &[u8], handles: Vec<Handle>, quota: &Arc<Quota>) -> Result<Arc<Task>, Error> {
    let charge = Charge::new(quota, Res::Tasks, 1)?;
    let aspace = Arc::new(AddrSpace::new(quota)?);
    aspace.set_self(Arc::downgrade(&aspace));
    let start = aspace.load(image)?;
    let n = handles.len() as u64;
    let mut table = HandleTable::new();
    for h in handles {
        table.add(h)?;
    }
    let task = Arc::new(Task {
        id: NEXT_TASK.fetch_add(1, Ordering::Relaxed),
        name: String::from(name),
        quota: quota.clone(),
        charge: IrqLock::new(Some(charge)),
        running: AtomicU32::new(0),
        pending_release: AtomicBool::new(false),
        aspace,
        handles: IrqLock::new(table),
        state: IrqLock::new(TaskState { threads: Vec::new(), exit: None, waiters: Vec::new() }),
        sig: SignalCell::new(0),
    });
    TASKS.lock().insert(task.id, task.clone());
    new_thread(&task, Start { arg: n, ..start })?;
    Ok(task)
}

pub fn new_thread(task: &Arc<Task>, start: Start) -> Result<Arc<Thread>, Error> {
    let charge = Charge::new(&task.quota, Res::Threads, 1)?;
    let t = Arc::new(Thread {
        id: NEXT_THREAD.fetch_add(1, Ordering::Relaxed),
        task: task.clone(),
        start,
        state: IrqLock::new(ThreadState { run: Run::Ready, wgen: 0, exit: None, waiters: Vec::new() }),
        sig: SignalCell::new(0),
        charge: IrqLock::new(Some(charge)),
    });
    {
        let mut s = task.state.lock();
        if s.exit.is_some() {
            return Err(Error::Killed);
        }
        s.threads.push(t.clone());
    }
    THREADS.lock().insert(t.id, t.clone());
    sched::start(t.clone());
    Ok(t)
}

impl Thread {
    pub fn run(&self) -> Run {
        self.state.lock().run
    }

    pub fn set_run(&self, r: Run) {
        self.state.lock().run = r;
    }

    /// Exit code, or registers `w` to be woken with it.
    pub fn wait(&self, w: Waiter) -> Option<i64> {
        let mut s = self.state.lock();
        if s.exit.is_none() {
            s.waiters.push(w);
        }
        s.exit
    }
}

impl Task {
    pub fn wait(&self, w: Waiter) -> Option<i64> {
        let mut s = self.state.lock();
        if s.exit.is_none() {
            s.waiters.push(w);
        }
        s.exit
    }

    pub fn add_handle(&self, obj: Object, rights: Rights) -> Result<u32, Error> {
        self.handles.lock().add(Handle { obj, rights })
    }

    /// (threads alive, exit code once ended), for the snapshot.
    pub fn status(&self) -> (usize, Option<i64>) {
        let s = self.state.lock();
        (s.threads.len(), s.exit)
    }
}

/// Every task the kernel knows (ended ones until their last handle goes).
pub fn all() -> Vec<Arc<Task>> {
    TASKS.lock().values().cloned().collect()
}

/// Wakes waiters with `code`: stores it in each waiter's task, returns 0.
fn notify(waiters: Vec<Waiter>, code: i64) {
    for w in waiters {
        if w.thread.run() != Run::Blocked {
            continue;
        }
        let r = w.thread.task.aspace.write(w.ptr, &code.to_le_bytes());
        sched::wake(&w.thread, if r.is_ok() { 0 } else { Error::Fault.code() });
    }
}

/// Ends a thread; the last one ends its task with the same code.
pub fn exit_thread(t: &Arc<Thread>, code: i64) {
    let waiters = {
        let mut s = t.state.lock();
        if s.run == Run::Dead {
            return;
        }
        s.exit = Some(code);
        core::mem::take(&mut s.waiters)
    };
    sched::remove(t);
    THREADS.lock().remove(&t.id);
    t.charge.lock().take();
    t.sig.update(0, abi::signal::TERMINATED);
    let last = {
        let mut s = t.task.state.lock();
        s.threads.retain(|x| x.id != t.id);
        s.threads.is_empty()
    };
    notify(waiters, code);
    if last {
        exit_task(&t.task, code);
    }
}

/// Ends a task: its threads stop, its handles close, waiters get `code`.
pub fn exit_task(task: &Arc<Task>, code: i64) {
    let (threads, waiters) = {
        let mut s = task.state.lock();
        if s.exit.is_some() {
            return;
        }
        s.exit = Some(code);
        (core::mem::take(&mut s.threads), core::mem::take(&mut s.waiters))
    };
    for t in &threads {
        let w = {
            let mut s = t.state.lock();
            s.exit = Some(code);
            core::mem::take(&mut s.waiters)
        };
        sched::remove(t);
        THREADS.lock().remove(&t.id);
        t.charge.lock().take();
        t.sig.update(0, abi::signal::TERMINATED);
        notify(w, code);
    }
    let handles = task.handles.lock().take_all();
    drop(handles);
    drop(threads);
    TASKS.lock().remove(&task.id);
    task.pending_release.store(true, Ordering::Release);
    try_release(task);
    task.charge.lock().take();
    task.sig.update(0, abi::signal::TERMINATED);
    notify(waiters, code);
    if task.id == 1 {
        crate::println!("init exited ({})", code);
        crate::selftest::init_exited(code);
    }
}

/// Frees an ended task's memory once none of its threads is on a CPU (the
/// last one to leave calls this again from `sched::finish_switch`).
pub fn try_release(task: &Arc<Task>) {
    if task.running.load(Ordering::Acquire) == 0 && task.pending_release.swap(false, Ordering::AcqRel) {
        arch::task_exited(&task.aspace);
    }
}

/// A user-mode fault: the task is killed.
pub fn fault(t: &Arc<Thread>, what: core::fmt::Arguments) {
    crate::println!("fault: task {} ({}) {}: killed", t.task.id, t.task.name, what);
    exit_task(&t.task.clone(), Error::Killed.code());
}

/// init gets the serial port (handle 0), the boot pack (handle 1), the
/// root quota (handle 2); then, with a server, the server connection
/// (handle 3) and ingress (handle 4).
pub fn start_init() {
    let Some(init) = crate::bootimg::find("init") else {
        crate::println!("task: no init");
        return;
    };
    let mut handles = alloc::vec![Handle { obj: Object::Serial(crate::serial::get()), rights: Rights::ALL }];
    if let Some(pack) = crate::bootimg::find("boot") {
        let m = crate::memory::Memory::create(pack.len() as u64, None).and_then(|m| m.write(0, pack).map(|_| m));
        match m {
            Ok(m) => {
                crate::println!("task: boot pack {} KiB", pack.len() / 1024);
                handles.push(Handle { obj: Object::Memory(m), rights: Rights::READ | Rights::DUP | Rights::TRANSFER });
            }
            Err(e) => crate::println!("task: boot pack: {:?}", e),
        }
    }
    let root = crate::quota::root(arch::memory_total());
    handles.push(Handle { obj: Object::Quota(root.clone()), rights: Rights::ALL });
    if let Some(r) = arch::remote_channel(0) {
        crate::println!("task: server connection for init (handle 3)");
        handles.push(Handle { obj: Object::Channel(r), rights: Rights::ALL });
    }
    if let Some(r) = arch::remote_channel(1) {
        crate::println!("task: ingress from the server for init (handle 4)");
        handles.push(Handle { obj: Object::Channel(r), rights: Rights::ALL });
    }
    crate::println!("task: root quota {} MiB", root.info().mem_limit >> 20);
    match spawn("init", init, handles, &root) {
        Ok(t) => crate::println!("task: started init (task {})", t.id),
        Err(e) => crate::println!("task: init: {:?}", e),
    }
}
