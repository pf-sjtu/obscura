//! Shared per-command V8 watchdog for the CDP server.
//!
//! One long-lived watchdog thread bounds every in-flight V8 command with a
//! deadline, instead of spawning and joining a thread per command (which adds
//! ~240us per command on the hot dispatch path). `arm` and `disarm` are a mutex
//! plus a condvar notify, in the low microseconds.
//!
//! With the thread-per-connection server (issue #430) several connections can
//! have a command armed at the same time (one isolate per connection, each on
//! its own OS thread), so a single global slot would let one connection's arm
//! overwrite another's and leave that command unbounded. The watchdog therefore
//! tracks a set of armed slots keyed by a monotonic generation, fires whichever
//! have overrun, and terminates each through its thread-safe `IsolateHandle`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::runtime::IsolateHandle;

struct Slot {
    deadline: Instant,
    handle: IsolateHandle,
    fired: Arc<AtomicBool>,
    /// The thread that armed this slot. Ops that legitimately block the
    /// isolate mid-command (synchronous XHR) register a hold under this key
    /// so an overrun is extended instead of terminated.
    owner: ThreadId,
}

/// Per-thread sync-block holds: while a value is present and its deadline
/// (epoch millis) is in the future, an expired slot owned by that thread is
/// extended rather than fired. `op_fetch_url_sync` sets this for the duration
/// of its worker-thread join; a bounded extension keeps a stuck worker from
/// parking the watchdog forever.
static BLOCK_HOLDS: OnceLock<Mutex<HashMap<ThreadId, u64>>> = OnceLock::new();

fn block_holds() -> &'static Mutex<HashMap<ThreadId, u64>> {
    BLOCK_HOLDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Hold watchdog termination for the current thread until `dur` from now.
/// Reaps stale entries so the map stays tiny. Returns the deadline written.
pub fn hold_sync_block(dur: Duration) -> u64 {
    let now = epoch_millis();
    let until = now + dur.as_millis() as u64;
    let mut holds = block_holds().lock().unwrap();
    holds.retain(|_, deadline| *deadline > now);
    holds.insert(std::thread::current().id(), until);
    until
}

/// Release a hold previously registered with [`hold_sync_block`].
pub fn release_sync_block() {
    block_holds()
        .lock()
        .unwrap()
        .remove(&std::thread::current().id());
}

fn sync_block_held(tid: ThreadId) -> bool {
    let holds = block_holds().lock().unwrap();
    holds.get(&tid).is_some_and(|deadline| *deadline > epoch_millis())
}

struct Shared {
    // (armed slots keyed by generation, monotonic generation counter)
    state: Mutex<(HashMap<u64, Slot>, u64)>,
    cv: Condvar,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

fn shared() -> &'static Arc<Shared> {
    SHARED.get_or_init(|| {
        let s = Arc::new(Shared {
            state: Mutex::new((HashMap::new(), 0)),
            cv: Condvar::new(),
        });
        let worker = s.clone();
        std::thread::Builder::new()
            .name("cdp-watchdog".into())
            .spawn(move || watchdog_loop(worker))
            .expect("spawn cdp watchdog");
        s
    })
}

fn watchdog_loop(s: Arc<Shared>) {
    let mut guard = s.state.lock().unwrap();
    loop {
        let now = Instant::now();
        // Terminate every slot that has overrun its deadline. The dispatcher's
        // `disarm` will observe `fired` and clear the V8 termination flag before
        // that isolate runs its next command.
        let expired: Vec<u64> = guard
            .0
            .iter()
            .filter(|(_, slot)| slot.deadline <= now)
            .map(|(gen, _)| *gen)
            .collect();
        for gen in expired {
            // A thread mid-way through a bounded synchronous block (sync XHR)
            // gets its deadline pushed back rather than terminated.
            let held = guard
                .0
                .get(&gen)
                .is_some_and(|slot| sync_block_held(slot.owner));
            if held {
                if let Some(slot) = guard.0.get_mut(&gen) {
                    slot.deadline = Instant::now() + Duration::from_millis(250);
                }
                continue;
            }
            if let Some(slot) = guard.0.remove(&gen) {
                slot.fired.store(true, Ordering::SeqCst);
                slot.handle.terminate_execution();
            }
        }
        // Sleep until the nearest remaining deadline, or until arm/disarm wakes
        // us. The worker holds the lock until it waits, so a notify cannot be
        // lost into the void.
        let next = guard
            .0
            .values()
            .map(|slot| slot.deadline.saturating_duration_since(now))
            .min();
        guard = match next {
            None => s.cv.wait(guard).unwrap(),
            Some(dur) => s.cv.wait_timeout(guard, dur).unwrap().0,
        };
    }
}

/// Handle to an armed command; pass to [`disarm`].
pub struct Armed {
    gen: u64,
    fired: Arc<AtomicBool>,
}

/// Arm the shared watchdog for the current command. If the isolate is still
/// executing `budget` later, it is terminated. O(1), no thread spawn. Safe to
/// call concurrently from several connections: each command gets its own slot.
pub fn arm(handle: IsolateHandle, budget: Duration) -> Armed {
    let s = shared();
    let mut guard = s.state.lock().unwrap();
    guard.1 += 1;
    let gen = guard.1;
    let fired = Arc::new(AtomicBool::new(false));
    guard.0.insert(
        gen,
        Slot {
            deadline: Instant::now() + budget,
            handle,
            fired: fired.clone(),
            owner: std::thread::current().id(),
        },
    );
    s.cv.notify_one();
    Armed { gen, fired }
}

/// Disarm the command's watchdog. Returns true if it had already fired
/// (terminated the isolate), in which case the caller must clear the V8
/// termination flag before the next command runs.
pub fn disarm(armed: Armed) -> bool {
    let s = shared();
    let mut guard = s.state.lock().unwrap();
    guard.0.remove(&armed.gen);
    // Wake the worker so it recomputes its sleep if we removed the nearest slot.
    s.cv.notify_one();
    armed.fired.load(Ordering::SeqCst)
}
