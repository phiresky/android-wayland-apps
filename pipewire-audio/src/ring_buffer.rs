//! Lock-free single-producer single-consumer ring buffer for f32 audio samples.
//!
//! The producer (PipeWire process callback) writes interleaved f32 samples.
//! The consumer (AAudio data callback) reads them out.
//! Both sides are real-time safe — no allocations, no locks.

use std::sync::atomic::{AtomicUsize, Ordering};

pub struct RingBuffer {
    buf: Box<[f32]>,
    /// Always a power of 2 for fast masking.
    capacity: usize,
    /// Write position (only modified by producer).
    head: AtomicUsize,
    /// Read position (only modified by consumer).
    tail: AtomicUsize,
}

// SAFETY: The ring buffer is designed for single-producer single-consumer access.
// The AtomicUsize head/tail provide the necessary synchronization.
unsafe impl Send for RingBuffer {}
unsafe impl Sync for RingBuffer {}

impl RingBuffer {
    /// Create a new ring buffer. `capacity_samples` is rounded up to the next power of 2.
    pub fn new(capacity_samples: usize) -> Self {
        let capacity = capacity_samples.next_power_of_two().max(2);
        Self {
            buf: vec![0.0f32; capacity].into_boxed_slice(),
            capacity,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn mask(&self) -> usize {
        self.capacity - 1
    }

    /// Number of samples available to read.
    pub fn available(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    /// Write samples into the ring buffer (producer side).
    /// If the buffer is full, oldest samples are silently dropped.
    pub fn write(&self, data: &[f32]) {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let free = self.capacity - head.wrapping_sub(tail);

        // If not enough space, advance tail to make room (drop oldest)
        if data.len() > free {
            let drop_count = data.len() - free;
            self.tail.store(tail.wrapping_add(drop_count), Ordering::Release);
        }

        let mask = self.mask();
        // SAFETY: We have exclusive producer access. The indices are masked to stay in bounds.
        let buf_ptr = self.buf.as_ptr() as *mut f32;
        for (i, &sample) in data.iter().enumerate() {
            let idx = (head.wrapping_add(i)) & mask;
            unsafe { buf_ptr.add(idx).write(sample) };
        }

        self.head.store(head.wrapping_add(data.len()), Ordering::Release);
    }

    /// Read samples from the ring buffer (consumer side).
    /// Returns the number of samples actually read. Caller should zero-fill the remainder.
    pub fn read(&self, out: &mut [f32]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let available = head.wrapping_sub(tail);
        let to_read = out.len().min(available);

        let mask = self.mask();
        for i in 0..to_read {
            let idx = (tail.wrapping_add(i)) & mask;
            out[i] = self.buf[idx];
        }

        self.tail.store(tail.wrapping_add(to_read), Ordering::Release);
        to_read
    }
}
