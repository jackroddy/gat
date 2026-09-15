use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::termios::{
    LocalModes, OptionalActions, SpecialCodeIndex, Termios, tcgetattr, tcsetattr,
};

/// How long to keep draining after a query gives up, before restoring the
/// terminal.
//
// a reply that lands after the restore is delivered to whatever reads the
// tty next, which is the user's shell: it appears as though they typed the
// escape sequence at their prompt. tcsetattr(TCSAFLUSH) discards only what
// has already queued, so a slow terminal needs this grace window too
//
// TODO: 60ms has no recorded source
const DRAIN_GRACE: Duration = Duration::from_millis(60);

/// The longest drain window [`Probe::set_grace`] will widen to.
//
// drain() cannot tell nothing arriving from a reply still
// in flight, so it waits out the whole window on every
// query, not just on the rare late one
//
// TODO: 250ms has no recorded source
const MAX_GRACE: Duration = Duration::from_millis(250);

/// A borrowed controlling terminal, switched into a mode where escape-sequence
/// replies can be read back.
pub struct Probe {
    tty: File,
    original: Termios,
    grace: Duration,
}

impl Probe {
    /// A probe on the controlling terminal, or `None` if it cannot be opened
    /// and switched into that mode.
    pub fn open() -> Option<Probe> {
        let tty = OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
        let original = tcgetattr(&tty).ok()?;

        let mut raw = original.clone();
        raw.local_modes &= !(LocalModes::ICANON | LocalModes::ECHO);
        raw.input_modes = rustix::termios::InputModes::empty();

        // read() returns whatever has arrived without blocking; the deadline
        // is enforced by poll() below rather than by VTIME, which cannot
        // express a budget shared across several reads
        raw.special_codes[SpecialCodeIndex::VMIN] = 0;
        raw.special_codes[SpecialCodeIndex::VTIME] = 0;
        tcsetattr(&tty, OptionalActions::Now, &raw).ok()?;

        Some(Probe {
            tty,
            original,
            grace: DRAIN_GRACE,
        })
    }

    /// Send `query` and accumulate the reply until `done` accepts it or
    /// `budget` runs out. Returns everything read.
    pub fn ask(
        &mut self,
        query: &[u8],
        budget: Duration,
        done: impl Fn(&[u8]) -> bool,
    ) -> Vec<u8> {
        self.exchange(query, budget, done).0
    }

    /// Send `query` and report how long `done` took to accept the reply, or
    /// `None` if it never did.
    pub fn timed(
        &mut self,
        query: &[u8],
        budget: Duration,
        done: impl Fn(&[u8]) -> bool,
    ) -> Option<Duration> {
        self.exchange(query, budget, done).1
    }

    /// Widen the drain window to `grace`, never narrowing it and never past
    /// [`MAX_GRACE`].
    pub fn set_grace(&mut self, grace: Duration) {
        self.grace = grace.clamp(DRAIN_GRACE, MAX_GRACE);
    }

    fn exchange(
        &mut self,
        query: &[u8],
        budget: Duration,
        done: impl Fn(&[u8]) -> bool,
    ) -> (Vec<u8>, Option<Duration>) {
        let mut got = Vec::with_capacity(512);
        let sent = Instant::now();
        if self.tty.write_all(query).is_err() {
            return (got, None);
        }

        let deadline = sent + budget;
        let mut answered = None;
        let mut buf = [0u8; 256];
        while got.len() < 4096 {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            if !self.readable_within(left) {
                break;
            }
            match self.tty.read(&mut buf) {
                // VMIN=0 makes a read that outruns the incoming bytes return
                // 0 rather than blocking; it does not mean end of input
                Ok(0) => continue,
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) if retryable(&e) => continue,
                Err(_) => break,
            }
            if done(&got) {
                answered = Some(sent.elapsed());
                break;
            }
        }

        // whether we finished or timed out, the terminal may still be mid
        // reply, and anything left behind reaches the shell
        self.drain();
        (got, answered)
    }

    /// Read whatever the terminal has sent, waiting at most `budget` for the
    /// first byte. Empty when nothing arrives.
    pub fn read_available(&mut self, budget: Duration) -> Vec<u8> {
        let mut got = Vec::new();
        if !self.readable_within(budget) {
            return got;
        }
        let mut buf = [0u8; 256];
        loop {
            match self.tty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);

                    // a short read means the queue is drained; a full one may
                    // have more behind it, such as a pasted burst of keys
                    if n < buf.len() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        got
    }

    fn readable_within(&self, left: Duration) -> bool {
        let timeout = Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos() as _,
        };
        let mut fds = [PollFd::new(&self.tty, PollFlags::IN)];
        loop {
            match rustix::event::poll(&mut fds, Some(&timeout)) {
                Ok(0) => return false,
                Ok(_) => return true,
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => return false,
            }
        }
    }

    fn drain(&mut self) {
        let deadline = Instant::now() + self.grace;
        let mut buf = [0u8; 256];
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if !self.readable_within(left) {
                break;
            }
            match self.tty.read(&mut buf) {
                Ok(0) => continue,
                Ok(_) => {}
                Err(e) if retryable(&e) => continue,
                Err(_) => break,
            }
        }
    }
}

fn retryable(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock)
}

impl Drop for Probe {
    fn drop(&mut self) {
        // TCSAFLUSH, so anything that arrived during the restore is discarded
        // rather than handed to the shell
        let _ = tcsetattr(&self.tty, OptionalActions::Flush, &self.original);
    }
}
