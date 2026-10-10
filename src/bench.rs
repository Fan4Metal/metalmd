//! `--bench <file>`: records how long opening a document takes and exits.
//!
//! Times are milliseconds since the process was created (on Windows, from the
//! OS's process creation time, so loader and DLL time count too).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

/// Timestamps recorded outside the app (eframe's startup hooks).
static EXTERNAL_MARKS: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// Records a timestamp from code that has no access to the `Bench`.
pub fn mark_external(name: &str) {
    if let Ok(mut marks) = EXTERNAL_MARKS.lock() {
        marks.push((name.to_owned(), Instant::now()));
    }
}

pub struct Bench {
    output: PathBuf,
    start: Instant,
    /// Time between process creation and `Bench::new`.
    pre_main_ms: f64,
    marks: Vec<(String, f64)>,
    frames_after_doc: u32,
    finished: bool,
}

impl Bench {
    pub fn new(output: PathBuf) -> Bench {
        let mut bench = Bench {
            output,
            start: Instant::now(),
            pre_main_ms: process_age_ms(),
            marks: Vec::new(),
            frames_after_doc: 0,
            finished: false,
        };
        bench.mark("main");
        bench
    }

    fn now_ms(&self) -> f64 {
        self.pre_main_ms + self.start.elapsed().as_secs_f64() * 1000.0
    }

    pub fn mark(&mut self, name: &str) {
        let t = self.now_ms();
        self.marks.push((name.to_owned(), t));
    }

    pub fn mark_once(&mut self, name: &str) {
        if !self.marks.iter().any(|(n, _)| n == name) {
            self.mark(name);
        }
    }

    /// Counts frames that showed the document; marks the second one, which
    /// starts after the first was presented.
    pub fn frames_after_doc(&mut self, painted: bool) -> u32 {
        if painted {
            self.frames_after_doc += 1;
            if self.frames_after_doc == 2 {
                self.mark("next_frame_end");
            }
        }
        self.frames_after_doc
    }

    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Ok(external) = EXTERNAL_MARKS.lock() {
            for (name, at) in external.iter() {
                let t = self.pre_main_ms + at.duration_since(self.start).as_secs_f64() * 1000.0;
                self.marks.push((name.clone(), t));
            }
        }
        self.marks.sort_by(|a, b| a.1.total_cmp(&b.1));
        let marks: Vec<String> = self
            .marks
            .iter()
            .map(|(name, t)| format!("\"{name}\": {t:.2}"))
            .collect();
        let json = format!("{{{}}}\n", marks.join(", "));
        let _ = std::fs::write(&self.output, json);
    }
}

#[cfg(windows)]
fn process_age_ms() -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut creation, mut exit, mut kernel, mut user, mut now) = (zero, zero, zero, zero, zero);
    // SAFETY: valid out-pointers to stack FILETIMEs; the pseudo handle from
    // GetCurrentProcess needs no closing.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return 0.0;
    }
    // SAFETY: valid out-pointer.
    unsafe { GetSystemTimePreciseAsFileTime(&mut now) };
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    ticks(now).saturating_sub(ticks(creation)) as f64 / 10_000.0
}

#[cfg(not(windows))]
fn process_age_ms() -> f64 {
    0.0
}
