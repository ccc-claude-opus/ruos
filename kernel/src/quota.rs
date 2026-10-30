//! Quotas: hierarchical budgets of memory, tasks and threads. A charge
//! counts against the quota and every ancestor; it fails if any of them
//! would go over its limit.
use alloc::sync::Arc;

use abi::{Error, QuotaInfo};

use crate::sync::IrqLock;

#[derive(Clone, Copy)]
pub enum Res {
    Memory,
    Tasks,
    Threads,
}

#[derive(Clone, Copy, Default)]
struct Counts {
    mem: u64,
    tasks: u64,
    threads: u64,
}

impl Counts {
    fn get(&mut self, r: Res) -> &mut u64 {
        match r {
            Res::Memory => &mut self.mem,
            Res::Tasks => &mut self.tasks,
            Res::Threads => &mut self.threads,
        }
    }
}

pub struct Quota {
    parent: Option<Arc<Quota>>,
    limit: Counts,
    used: IrqLock<Counts>,
}

static ROOT: IrqLock<Option<Arc<Quota>>> = IrqLock::new(None);

/// Everything there is: all of `memory`, any number of tasks and threads.
pub fn root(memory: u64) -> Arc<Quota> {
    ROOT.lock()
        .get_or_insert_with(|| {
            Arc::new(Quota {
                parent: None,
                limit: Counts { mem: memory, tasks: u64::MAX, threads: u64::MAX },
                used: IrqLock::new(Counts::default()),
            })
        })
        .clone()
}

impl Quota {
    pub fn child(self: &Arc<Self>, mem: u64, tasks: u64, threads: u64) -> Arc<Quota> {
        Arc::new(Quota {
            parent: Some(self.clone()),
            limit: Counts { mem, tasks, threads },
            used: IrqLock::new(Counts::default()),
        })
    }

    pub fn charge(&self, r: Res, n: u64) -> Result<(), Error> {
        let mut q = Some(self);
        while let Some(x) = q {
            let used = *x.used.lock().get(r);
            if used.saturating_add(n) > *x.limit.clone().get(r) {
                return Err(Error::QuotaExceeded);
            }
            q = x.parent.as_deref();
        }
        let mut q = Some(self);
        while let Some(x) = q {
            *x.used.lock().get(r) += n;
            q = x.parent.as_deref();
        }
        Ok(())
    }

    pub fn refund(&self, r: Res, n: u64) {
        let mut q = Some(self);
        while let Some(x) = q {
            let mut u = x.used.lock();
            let v = u.get(r);
            *v = v.saturating_sub(n);
            drop(u);
            q = x.parent.as_deref();
        }
    }

    /// Bytes still available here and in every ancestor.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn mem_left(&self) -> u64 {
        let mut left = u64::MAX;
        let mut q = Some(self);
        while let Some(x) = q {
            left = left.min(x.limit.mem.saturating_sub(x.used.lock().mem));
            q = x.parent.as_deref();
        }
        left
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn parent(&self) -> Option<&Arc<Quota>> {
        self.parent.as_ref()
    }

    pub fn info(&self) -> QuotaInfo {
        let u = *self.used.lock();
        QuotaInfo {
            mem_used: u.mem,
            mem_limit: self.limit.mem,
            tasks_used: u.tasks,
            tasks_limit: self.limit.tasks,
            threads_used: u.threads,
            threads_limit: self.limit.threads,
        }
    }
}

/// A charge that is refunded when dropped.
pub struct Charge {
    pub quota: Arc<Quota>,
    res: Res,
    n: u64,
}

impl Charge {
    pub fn new(quota: &Arc<Quota>, res: Res, n: u64) -> Result<Charge, Error> {
        quota.charge(res, n)?;
        Ok(Charge { quota: quota.clone(), res, n })
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.quota.refund(self.res, self.n);
    }
}
