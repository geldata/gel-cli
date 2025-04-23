#![cfg_attr(windows, allow(dead_code))]

use std::future::Future;
use std::pin::Pin;
use std::process;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::thread;

use arc_swap::ArcSwapOption;
use backtrace::Backtrace;
use fn_error_context::context;
use futures_util::task::AtomicWaker;

use crate::bug;
use crate::commands::ExitCode;

static CUR_INTERRUPT: ArcSwapOption<SignalState> = ArcSwapOption::const_empty();
static CUR_TERM: ArcSwapOption<TermSentinel> = ArcSwapOption::const_empty();

#[cfg(windows)]
struct TermSentinel {}

#[cfg(unix)]
struct TermSentinel {
    tty: std::fs::File,
    termios: libc::termios,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Signal {
    Interrupt,
    Term,
    Hup,
}

/// A RAII guard for masking out signals and waiting for them synchronously
///
/// Trap temporarily replaces signal handlers to an empty handler, effectively
/// activating singnals that are ignored by default.
///
/// Old signal handlers are restored in `Drop` handler.
#[cfg(unix)]
pub struct Trap {
    oldset: nix::sys::signal::SigSet,
    oldsigs: Vec<(Signal, nix::sys::signal::SigAction)>,
}

#[cfg(not(unix))]
pub struct Trap {}

type SigMask = u8;

#[derive(Debug, thiserror::Error)]
#[error("Interrupted by user")]
pub struct InterruptError(pub Signal);

pub struct Interrupt {
    event: Arc<Event>,
}

pub struct SignalState {
    backtrace: Backtrace,
    event: Arc<Event>,
    signals: SigMask,
}

pub struct MemorizeTerm {}

struct Event {
    first: crossbeam_utils::atomic::AtomicCell<Option<Signal>>,
    last: crossbeam_utils::atomic::AtomicCell<Option<Signal>>,
    waker: AtomicWaker,
}

struct EventWait<'a>(&'a Event);

impl Event {
    fn new() -> Self {
        Event {
            first: crossbeam_utils::atomic::AtomicCell::new(None),
            last: crossbeam_utils::atomic::AtomicCell::new(None),
            waker: AtomicWaker::new(),
        }
    }
    fn set(&self, sig: Signal) {
        self.first.compare_exchange(None, Some(sig)).ok();
        self.last.store(Some(sig));
        self.waker.wake()
    }
    fn wait(&self) -> EventWait {
        EventWait(self)
    }
    fn clear(&self) {
        self.first.store(None);
        self.last.store(None);
    }
}

impl MemorizeTerm {
    #[cfg(unix)]
    #[context("cannot get terminal mode")]
    pub fn new() -> anyhow::Result<MemorizeTerm> {
        use std::os::unix::io::AsRawFd;

        let tty = std::fs::File::open("/dev/tty")?;
        let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
        if unsafe { libc::tcgetattr(tty.as_raw_fd(), mode.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mode = unsafe { mode.assume_init() };
        let sentinel = Arc::new(TermSentinel { tty, termios: mode });
        let old = CUR_TERM.compare_and_swap(&None::<Arc<_>>, Some(sentinel));
        if old.is_some() {
            return Err(bug::error("nested terminal mode change is unsupported"));
        }
        Ok(MemorizeTerm {})
    }
    #[cfg(windows)]
    #[context("cannot get terminal mode")]
    pub fn new() -> anyhow::Result<MemorizeTerm> {
        Ok(MemorizeTerm {})
    }
}

impl Drop for MemorizeTerm {
    fn drop(&mut self) {
        // Drop means code was executed normally and it isn't exit due
        // to signal. In this case `rpassword` will take care of interrupt
        CUR_TERM.swap(None);
    }
}

#[cfg(unix)]
fn reset_terminal(sentinel: &TermSentinel) {
    use std::os::unix::io::AsRawFd;

    unsafe {
        libc::tcsetattr(sentinel.tty.as_raw_fd(), libc::TCSANOW, &sentinel.termios);
    }
}

#[cfg(windows)]
fn reset_terminal(_sentinel: &TermSentinel) {
    // On windows it's reset automatically
}

#[cfg(unix)]
fn signal_message(signal: Signal) -> i32 {
    let id = signal.to_unix();
    if signal == Signal::Interrupt {
        log::warn!("Exiting due to interrupt");
    } else {
        log::warn!(
            "Exiting on signal {}",
            signal_hook::low_level::signal_name(id).unwrap_or("<unknown>")
        );
    }
    id
}

#[cfg(windows)]
fn signal_message(_signal: Signal) -> i32 {
    log::warn!("Exiting due to interrupt");
    2 // same as SIGINT on linux
}

fn exit_on(signal: Signal) -> ! {
    if let Some(sentinel) = &*CUR_TERM.load() {
        reset_terminal(sentinel);
    }
    let id = signal_message(signal);
    process::exit(128 + id);
}

pub fn init_signals() {
    #[cfg(windows)]
    ctrlc::set_handler(move || {
        if let Some(state) = CUR_INTERRUPT.load_full() {
            state.event.set(Signal::Interrupt);
        } else {
            exit_on(Signal::Interrupt);
        }
    })
    .expect("Ctrl+C handler can be set");

    #[cfg(unix)]
    thread::spawn(|| {
        use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;

        let mut signals = Signals::new([SIGINT, SIGHUP, SIGTERM]).expect("signals initialized");
        for signal in signals.into_iter() {
            if let Some(state) = CUR_INTERRUPT.load_full() {
                if let Some(sig) = Signal::from_unix(signal) {
                    if sig.as_bit() & state.signals != 0 {
                        state
                            .event
                            .set(Signal::from_unix(signal).expect("known signal"));
                    } else {
                        exit_on(sig);
                    }
                }
            } else if let Some(sig) = Signal::from_unix(signal) {
                exit_on(sig);
            }
        }
    });
}

impl Interrupt {
    pub fn ctrl_c() -> Interrupt {
        Interrupt::new(Signal::Interrupt.as_bit())
    }
    pub fn term() -> Interrupt {
        Interrupt::new(Signal::all_bits())
    }
    fn new(signals: SigMask) -> Interrupt {
        let event = Arc::new(Event::new());
        let new = Arc::new(SignalState {
            backtrace: Backtrace::new_unresolved(),
            event: event.clone(),
            signals,
        });
        let old = CUR_INTERRUPT.compare_and_swap(&None::<Arc<_>>, Some(new));
        if let Some(state) = &*old {
            let mut old_bt = state.backtrace.clone();
            old_bt.resolve();
            panic!(
                "Second Interrupt created simultaneously.\n\n\
                Previous was created at:\n{:?}",
                old_bt
            );
        };
        Interrupt { event }
    }
    pub async fn wait_result<T>(&self) -> anyhow::Result<T> {
        Err(InterruptError(self.event.wait().await).into())
    }
    pub async fn wait(&self) -> Signal {
        self.event.wait().await
    }
    pub fn err_if_occurred(&self) -> anyhow::Result<()> {
        if let Some(sig) = self.event.first.load() {
            self.event.clear();
            return Err(ExitCode::new(128 + signal_message(sig)).into());
        }
        Ok(())
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        let old = CUR_INTERRUPT.swap(None::<Arc<_>>).expect("Interrupt set");
        if let Some(sig) = old.event.last.load() {
            exit_on(sig);
        }
    }
}

impl Future for EventWait<'_> {
    type Output = Signal;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Signal> {
        // quick check to avoid registration if already done.
        if let Some(sig) = self.0.last.swap(None) {
            return Poll::Ready(sig);
        }

        self.0.waker.register(cx.waker());

        // Need to check condition **after** `register` to avoid a race
        // condition that would result in lost notifications.
        if let Some(sig) = self.0.last.swap(None) {
            Poll::Ready(sig)
        } else {
            Poll::Pending
        }
    }
}

impl Signal {
    fn all_bits() -> SigMask {
        0b111
    }
    fn as_bit(&self) -> SigMask {
        match self {
            Signal::Interrupt => 0b001,
            Signal::Term => 0b010,
            Signal::Hup => 0b100,
        }
    }
}

#[cfg(unix)]
impl Signal {
    fn to_unix(self) -> i32 {
        use signal_hook::consts::signal::*;

        match self {
            Signal::Interrupt => SIGINT,
            Signal::Term => SIGTERM,
            Signal::Hup => SIGHUP,
        }
    }
    fn to_nix(self) -> nix::sys::signal::Signal {
        use nix::sys::signal::Signal::*;

        match self {
            Signal::Interrupt => SIGINT,
            Signal::Term => SIGTERM,
            Signal::Hup => SIGHUP,
        }
    }
    fn from_unix(sig: i32) -> Option<Self> {
        use signal_hook::consts::signal::*;

        match sig {
            SIGINT => Some(Signal::Interrupt),
            SIGTERM => Some(Signal::Term),
            SIGHUP => Some(Signal::Hup),
            _ => None,
        }
    }
}

impl Trap {
    /// Create and activate the signal trap for specified signals. Signals not
    /// in list will be delivered asynchronously as always.
    #[cfg(unix)]
    pub fn new(signals: &[Signal]) -> Trap {
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, sigaction};
        use nix::sys::signal::{SigSet, SigmaskHow};

        extern "C" fn empty_handler(_: libc::c_int) {}

        unsafe {
            let mut sigset = SigSet::empty();
            for &sig in signals {
                sigset.add(sig.to_nix());
            }
            let oldset = sigset
                .thread_swap_mask(SigmaskHow::SIG_BLOCK)
                .expect("can set thread mask");
            let mut oldsigs = Vec::new();
            // Set signal handlers to an empty function, this allows ignored
            // signals to become pending, effectively allowing them to be
            // waited for.
            for &sig in signals {
                oldsigs.push((
                    sig,
                    sigaction(
                        sig.to_nix(),
                        &SigAction::new(
                            SigHandler::Handler(empty_handler),
                            SaFlags::empty(),
                            sigset,
                        ),
                    )
                    .expect("sigaction works"),
                ));
            }
            Trap { oldset, oldsigs }
        }
    }

    #[cfg(not(unix))]
    pub fn new(_: &[Signal]) -> Trap {
        Trap {}
    }
}

#[cfg(unix)]
impl Drop for Trap {
    fn drop(&mut self) {
        use nix::sys::signal::sigaction;

        unsafe {
            for &(sig, ref sigact) in self.oldsigs.iter() {
                sigaction(sig.to_nix(), sigact).expect("sigaction works");
            }
            self.oldset.thread_set_mask().expect("sigset works");
        }
    }
}
