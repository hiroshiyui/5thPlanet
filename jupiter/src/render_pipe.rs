//! Render-pipeline worker thread.
//!
//! The VDP2 compositor (`saturn::vdp2::render_frame`) is a *pure read* of the
//! VDP state at a frame boundary — it produces pixels but never mutates the
//! machine. So it can run on a second core, overlapped with the *next* frame's
//! emulation on the main thread, taking the displayed frame rate from the
//! compute+render rate up toward the compute-only ceiling (the loosely-coupled
//! edge — the core CPU/bus/interrupt cluster stays single-threaded).
//!
//! Each frame the main thread clones the render inputs (VDP2 + the VDP1 display
//! framebuffer — both plain data) and hands them to the worker; the worker
//! composites into a recycled buffer and hands it back. The displayed frame is
//! one behind the emulated frame (standard render-pipeline latency). The output
//! pixels are bit-identical to an inline `render_frame` — only *when* they're
//! shown changes — so accuracy and the `bios_boot` golden are unaffected (the
//! core's `run_frame` is untouched; this path is frontend-only).
//!
//! If the worker dies (a panic inside the compositor), the pipe logs it once,
//! reports it via [`RenderPipe::is_dead`], and degrades to compositing inline
//! on the caller's thread — the display keeps updating instead of freezing
//! silently on the last frame.
//!
//! The module is `sdl`-free (operates on `Saturn` + `Vec<u8>` + channels), so
//! the buffer/handshake logic is unit-tested without a window.

use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

use saturn::Saturn;
use saturn::vdp1::Framebuffer;
use saturn::vdp2::Vdp2;

/// A render request: a snapshot of the VDP state plus the buffer to draw into.
struct Job {
    vdp2: Vdp2,
    sprite_fb: Framebuffer,
    /// DIE field-weave source (back buffer + DIL), cloned when the frame is
    /// double-interlaced (and weave isn't disabled via `SAT_VDP1_NOWEAVE`);
    /// `None` otherwise (no extra clone). See [`saturn::vdp1::Vdp1::weave_source`].
    sprite_weave: Option<(Framebuffer, u8)>,
    out: Vec<u8>,
}

/// A finished frame: the filled buffer and the active `(width, height)`.
struct Done {
    out: Vec<u8>,
    dims: (usize, usize),
}

/// Owns the render worker and a single recycled spare buffer. With the 1-deep
/// pipeline the main thread holds the *display* buffer and the pipe holds the
/// *spare*; they swap each frame (see the integration in `main.rs`).
pub struct RenderPipe {
    job_tx: SyncSender<Job>,
    done_rx: Receiver<Done>,
    /// A free buffer ready to be dispatched, or `None` while one is in flight.
    spare: Option<Vec<u8>>,
    /// True between [`submit`] and the matching [`wait`].
    in_flight: bool,
    worker: Option<JoinHandle<()>>,
    /// Set (and reported once on stderr) when the worker has gone away — it
    /// panicked inside `render_frame`. From then on [`submit`](Self::submit)
    /// renders inline into [`ready`](Self::ready).
    dead: bool,
    /// A frame rendered inline (dead-worker fallback), handed out by `wait`.
    ready: Option<Done>,
}

impl RenderPipe {
    /// Spawn the worker. `buf_bytes` sizes the two frame buffers (one held by
    /// the caller as the display buffer, one here as the spare).
    pub fn new(buf_bytes: usize) -> Self {
        // Bound 1: at most one job queued; the main thread waits for the
        // previous frame before dispatching the next, so it never piles up.
        let (job_tx, job_rx) = sync_channel::<Job>(1);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<Done>();
        let worker = std::thread::Builder::new()
            .name("render".into())
            .spawn(move || {
                while let Ok(mut job) = job_rx.recv() {
                    let dims = saturn::vdp2::render_frame_weave(
                        &job.vdp2,
                        Some(&job.sprite_fb),
                        job.sprite_weave.as_ref().map(|(fb, dil)| (fb, *dil)),
                        &mut job.out,
                    );
                    if done_tx.send(Done { out: job.out, dims }).is_err() {
                        break; // main thread gone
                    }
                }
            })
            .expect("spawn render worker");
        Self {
            job_tx,
            done_rx,
            spare: Some(vec![0u8; buf_bytes]),
            in_flight: false,
            worker: Some(worker),
            dead: false,
            ready: None,
        }
    }

    /// Clone the current VDP render inputs and dispatch a render on the worker.
    /// No-op (returns `false`) if no spare buffer is free yet — call [`wait`]
    /// first to reclaim one. The clone is the only main-thread cost (~0.8 MB,
    /// far cheaper than compositing).
    pub fn submit(&mut self, sat: &Saturn) -> bool {
        let Some(out) = self.spare.take() else {
            return false;
        };
        if self.dead {
            return self.render_inline(sat, out);
        }
        let job = Job {
            vdp2: sat.bus.vdp2.clone(),
            sprite_fb: sat.bus.vdp1.display_fb().clone(),
            sprite_weave: sat
                .bus
                .vdp1
                .weave_source()
                .map(|(fb, dil)| (fb.clone(), dil)),
            out,
        };
        match self.job_tx.send(job) {
            Ok(()) => {
                self.in_flight = true;
                true
            }
            Err(e) => {
                // Worker gone: reclaim the buffer and render this frame inline.
                self.mark_dead();
                self.render_inline(sat, e.0.out)
            }
        }
    }

    /// Block for the in-flight frame and return its `(buffer, dims)`. `None` if
    /// nothing was in flight (e.g. the very first frame). The returned buffer is
    /// the caller's new display buffer; the caller hands its *old* display
    /// buffer back via [`recycle`] so it becomes the next spare.
    pub fn wait(&mut self) -> Option<(Vec<u8>, (usize, usize))> {
        if !self.in_flight {
            return None;
        }
        self.in_flight = false;
        if let Some(done) = self.ready.take() {
            return Some((done.out, done.dims));
        }
        match self.done_rx.recv() {
            Ok(done) => Some((done.out, done.dims)),
            Err(_) => {
                // The worker died mid-render, taking that frame's buffer with
                // it; the caller's spare-feed path supplies a new one.
                self.mark_dead();
                None
            }
        }
    }

    /// Whether the render worker has died (a panic in the compositor); the pipe
    /// is now compositing on the caller's thread. The frontend reports it.
    // Only the SDL3 main loop reports it.
    #[cfg_attr(not(feature = "sdl-frontend"), allow(dead_code))]
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Record the worker's death, logging it the first time only.
    fn mark_dead(&mut self) {
        if !self.dead {
            self.dead = true;
            eprintln!(
                "render worker died (panic in the VDP compositor); compositing on the emulation thread instead"
            );
        }
    }

    /// Dead-worker fallback: composite `sat`'s current frame into `out` on this
    /// thread (the same pure `render_frame_weave` the worker runs) and queue it
    /// for the next [`wait`](Self::wait).
    fn render_inline(&mut self, sat: &Saturn, mut out: Vec<u8>) -> bool {
        let dims = saturn::vdp2::render_frame_weave(
            &sat.bus.vdp2,
            Some(sat.bus.vdp1.display_fb()),
            sat.bus.vdp1.weave_source(),
            &mut out,
        );
        self.ready = Some(Done { out, dims });
        self.in_flight = true;
        true
    }

    /// Whether the pipe has neither a spare buffer nor a render in flight —
    /// i.e. it cannot accept a [`submit`] until the caller [`recycle`]s a
    /// buffer into it. Skipping a submit for lack of a spare must not become
    /// permanent: `wait` only returns buffers for submitted jobs, so the
    /// caller has to check this *outside* the wait path.
    // Only the SDL3 main loop drives the spare-feed path.
    #[cfg_attr(not(feature = "sdl-frontend"), allow(dead_code))]
    pub fn needs_spare(&self) -> bool {
        self.spare.is_none() && !self.in_flight
    }

    /// Return a buffer to the pipe as the next spare (the caller's previous
    /// display buffer, free once the new one has been swapped in).
    pub fn recycle(&mut self, buf: Vec<u8>) {
        // Keep at most one spare; drop extras (shouldn't happen in the 1-deep
        // protocol, but stay leak-free if misused).
        if self.spare.is_none() {
            self.spare = Some(buf);
        }
    }
}

impl Drop for RenderPipe {
    fn drop(&mut self) {
        // Close the job channel so the worker's recv() returns Err and it exits,
        // then join it.
        // (Dropping job_tx happens via struct drop order; force it by replacing
        // with a closed channel.)
        let (dead, _) = sync_channel::<Job>(1);
        self.job_tx = dead;
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saturn::vdp2::FRAMEBUFFER_BYTES;

    fn booted() -> Saturn {
        // A blank-BIOS Saturn is enough: render_frame reads whatever VDP state
        // is present (display off → opaque black), which is all we need to
        // exercise the pipe's buffer handshake.
        let mut sat = Saturn::with_blank_bios();
        sat.halt_slave();
        sat
    }

    #[test]
    fn submit_then_wait_returns_a_rendered_frame() {
        let mut pipe = RenderPipe::new(FRAMEBUFFER_BYTES);
        let sat = booted();
        assert!(pipe.submit(&sat));
        assert!(!pipe.submit(&sat), "no spare while a render is in flight");
        let (buf, dims) = pipe.wait().expect("a frame comes back");
        assert_eq!(buf.len(), FRAMEBUFFER_BYTES);
        assert!(dims.0 > 0 && dims.1 > 0, "valid dims {dims:?}");
    }

    #[test]
    fn a_dead_worker_is_reported_and_rendering_continues_inline() {
        // A buffer too small for the frame makes render_frame panic in the
        // worker — the stand-in for any compositor panic.
        let mut pipe = RenderPipe::new(16);
        let sat = booted();
        assert!(!pipe.is_dead());
        assert!(pipe.submit(&sat));
        assert!(pipe.wait().is_none(), "the panicked frame never comes back");
        assert!(pipe.is_dead());
        // The caller's spare-feed path hands in a proper buffer; frames keep
        // coming, composited inline.
        assert!(pipe.needs_spare());
        pipe.recycle(vec![0u8; FRAMEBUFFER_BYTES]);
        assert!(pipe.submit(&sat));
        let (buf, dims) = pipe.wait().expect("inline frame");
        assert_eq!(buf.len(), FRAMEBUFFER_BYTES);
        assert!(dims.0 > 0 && dims.1 > 0);
        assert!(pipe.wait().is_none(), "one frame per submit");
    }

    #[test]
    fn wait_without_submit_is_none() {
        let mut pipe = RenderPipe::new(FRAMEBUFFER_BYTES);
        assert!(pipe.wait().is_none());
    }

    #[test]
    fn recycle_then_resubmit_cycles_buffers() {
        // Drive several frames through the 1-deep protocol the way main.rs does:
        // submit, then each iteration wait → swap → recycle old → submit.
        let mut pipe = RenderPipe::new(FRAMEBUFFER_BYTES);
        let sat = booted();
        let mut display = vec![0u8; FRAMEBUFFER_BYTES];
        assert!(pipe.submit(&sat)); // prime
        for _ in 0..5 {
            let (rendered, _dims) = pipe.wait().expect("frame");
            let old = std::mem::replace(&mut display, rendered);
            pipe.recycle(old);
            assert!(pipe.submit(&sat), "a spare is available after recycle");
        }
        assert_eq!(display.len(), FRAMEBUFFER_BYTES);
    }
}
