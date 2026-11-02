//! Video playback through Media Foundation: decodes frames into textures,
//! no audio.
//!
//! A `VideoPlayer` decodes frames on a worker thread and uploads textures to
//! egui. Visible clips autoplay silently on a loop, like animated images.
//! Each viewport reports its visible clips, so scrolling or switching tabs
//! suspends playback without overriding a pause requested by the reader.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{ColorImage, Context, TextureHandle, TextureOptions, Vec2};

#[cfg(windows)]
use windows::Win32::Media::MediaFoundation::*;
#[cfg(windows)]
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
#[cfg(windows)]
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
#[cfg(windows)]
use windows::Win32::System::Variant::*;
#[cfg(windows)]
use windows::core::{GUID, HSTRING};

/// Play state of one video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlayState {
    Paused,
    Playing,
}

/// A frame ready to draw.
struct Frame {
    image: ColorImage,
    /// Presentation time in seconds from the start.
    pts: f64,
}

/// What the worker sends back.
enum WorkerMsg {
    /// The video's native size and duration.
    Info {
        width: u32,
        height: u32,
        duration_s: f64,
    },
    /// A decoded frame.
    Frame(Frame),
    Paused,
    /// The worker cannot decode this file.
    Failed(String),
}

/// What the main thread asks the worker to do.
enum WorkerCmd {
    /// Resume decoding, or restart after the end of the stream.
    Play,
    /// Stop decoding.
    Pause,
    /// Seek to this time and pause.
    Seek(f64),
    /// The UI has consumed the last frame; decoding may continue.
    FrameShown,
}

/// A video being decoded.
struct Video {
    width: u32,
    height: u32,
    duration_s: f64,
    state: PlayState,
    user_paused: bool,
    /// The texture shown now.
    texture: Option<TextureHandle>,
    /// Where we are in the video, in seconds from the start.
    position: f64,
    /// Sends commands to the worker.
    worker_tx: Sender<WorkerCmd>,
    error: Option<String>,
}

impl Video {
    fn play(&mut self) {
        if self.state == PlayState::Playing {
            return;
        }
        self.state = PlayState::Playing;
        let _ = self.worker_tx.send(WorkerCmd::Play);
    }

    fn pause(&mut self) {
        if self.state == PlayState::Paused {
            return;
        }
        self.state = PlayState::Paused;
        let _ = self.worker_tx.send(WorkerCmd::Pause);
    }

    fn seek(&mut self, to: f64) {
        self.position = to.clamp(0.0, self.duration_s);
        self.state = PlayState::Paused;
        let _ = self.worker_tx.send(WorkerCmd::Seek(self.position));
    }
}

type Videos = Arc<Mutex<HashMap<PathBuf, Video>>>;

/// The video player: one worker thread per video, textures uploaded on the
/// UI thread.
pub struct VideoPlayer {
    videos: Videos,
    receiver: Receiver<(PathBuf, WorkerMsg)>,
    sender: Sender<(PathBuf, WorkerMsg)>,
    /// Clips painted by each viewport, excluding invisible measuring passes.
    visible: HashMap<egui::ViewportId, HashSet<PathBuf>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl VideoPlayer {
    #[cfg(test)]
    pub(super) fn test_playing(ctx: &Context, path: &Path, position: f64) -> Self {
        let mut player = Self::new(|| {});
        let (worker_tx, _) = channel();
        player.videos.lock().unwrap().insert(
            path.to_owned(),
            Video {
                width: 64,
                height: 64,
                duration_s: 2.0,
                state: PlayState::Playing,
                user_paused: false,
                texture: Some(ctx.load_texture(
                    "test-video",
                    ColorImage::new([64, 64], vec![egui::Color32::BLACK; 64 * 64]),
                    TextureOptions::LINEAR,
                )),
                position,
                worker_tx,
                error: None,
            },
        );
        player
            .visible
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .insert(path.to_owned());
        player
    }

    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> VideoPlayer {
        let (sender, receiver) = channel();
        VideoPlayer {
            videos: Arc::new(Mutex::new(HashMap::new())),
            receiver,
            sender,
            visible: HashMap::new(),
            repaint: Arc::new(repaint),
        }
    }

    /// Prepares `path` for drawing: starts the worker if it is not running.
    /// Returns `None` if the video is not ready yet.
    pub fn get(&mut self, ctx: &Context, path: &Path) -> Option<VideoState> {
        let mut videos = self.videos.lock().expect("video lock");
        if !videos.contains_key(path) {
            let sender = self.sender.clone();
            let worker_path = path.to_owned();
            let repaint = self.repaint.clone();
            let (worker_tx, worker_rx) = channel();
            std::thread::Builder::new()
                .name(format!("video:{}", path.display()))
                .spawn(move || {
                    #[cfg(windows)]
                    {
                        if let Err(e) =
                            worker(worker_path.clone(), worker_rx, sender.clone(), &*repaint)
                        {
                            let _ = sender.send((worker_path, WorkerMsg::Failed(e)));
                            repaint();
                        }
                    }
                    #[cfg(not(windows))]
                    {
                        let _ = sender.send((
                            worker_path,
                            WorkerMsg::Failed("video playback is Windows-only".into()),
                        ));
                        repaint();
                    }
                })
                .expect("spawn video worker");
            videos.insert(
                path.to_owned(),
                Video {
                    width: 0,
                    height: 0,
                    duration_s: 0.0,
                    state: PlayState::Paused,
                    user_paused: false,
                    texture: None,
                    position: 0.0,
                    worker_tx,
                    error: None,
                },
            );
        }

        // Drain messages from all workers.
        while let Ok((msg_path, msg)) = self.receiver.try_recv() {
            if let Some(video) = videos.get_mut(&msg_path) {
                match msg {
                    WorkerMsg::Info {
                        width,
                        height,
                        duration_s,
                    } => {
                        video.width = width;
                        video.height = height;
                        video.duration_s = duration_s;
                    }
                    WorkerMsg::Frame(frame) => {
                        video.width = frame.image.size[0] as u32;
                        video.height = frame.image.size[1] as u32;
                        if let Some(texture) = &mut video.texture {
                            texture.set(frame.image, TextureOptions::LINEAR);
                        } else {
                            video.texture = Some(ctx.load_texture(
                                format!("video:{}", msg_path.display()),
                                frame.image,
                                TextureOptions::LINEAR,
                            ));
                        }
                        video.position = frame.pts;
                        let _ = video.worker_tx.send(WorkerCmd::FrameShown);
                    }
                    WorkerMsg::Paused => {
                        video.state = PlayState::Paused;
                    }
                    WorkerMsg::Failed(error) => {
                        video.error = Some(error);
                    }
                }
            }
        }

        let video = videos.get_mut(path)?;
        if let Some(error) = &video.error {
            return Some(VideoState::Failed(error.clone()));
        }
        if video.width == 0 {
            return Some(VideoState::Loading);
        }

        let size = Vec2::new(video.width as f32, video.height as f32);
        Some(VideoState::Ready(VideoFrame {
            texture: video.texture.clone()?,
            size,
            playing: video.state == PlayState::Playing,
            position: video.position,
            duration: video.duration_s,
        }))
    }

    /// Begin collecting visible clips for one window's active document.
    pub fn begin_view(&mut self, viewport: egui::ViewportId) {
        if let Some(paths) = self.visible.get_mut(&viewport) {
            paths.clear();
        }
    }

    /// Autoplay a clip actually painted on screen, unless the reader paused it.
    pub fn mark_visible(&mut self, viewport: egui::ViewportId, path: &Path) {
        self.visible
            .entry(viewport)
            .or_default()
            .insert(path.to_owned());
        if let Some(video) = self.videos.lock().expect("video lock").get_mut(path)
            && !video.user_paused
            && video.error.is_none()
        {
            video.play();
        }
    }

    /// Pause clips absent from every visible window (virtualized blocks and
    /// inactive tabs are not drawn, so they cannot pause themselves).
    pub fn end_view(&mut self) {
        let mut videos = self.videos.lock().expect("video lock");
        for (path, video) in videos.iter_mut() {
            if !self.visible.values().any(|paths| paths.contains(path)) {
                video.pause();
            }
        }
    }

    pub fn retain_views(&mut self, mut is_open: impl FnMut(egui::ViewportId) -> bool) {
        self.visible.retain(|viewport, _| is_open(*viewport));
        self.end_view();
    }

    /// A manual pause persists across frames, scrolling, and tab switches.
    pub fn toggle(&mut self, path: &Path) {
        let mut videos = self.videos.lock().expect("video lock");
        if let Some(video) = videos.get_mut(path) {
            match video.state {
                PlayState::Paused => {
                    video.user_paused = false;
                    video.play();
                }
                PlayState::Playing => {
                    video.user_paused = true;
                    video.pause();
                }
            }
        }
    }

    /// Seeks `path` to `position` (0.0 to 1.0).
    pub fn seek(&mut self, path: &Path, position: f64) {
        let mut videos = self.videos.lock().expect("video lock");
        if let Some(video) = videos.get_mut(path) {
            video.user_paused = true;
            video.seek(position * video.duration_s);
        }
    }
}

/// The state of a video the viewer asks for.
pub enum VideoState {
    Loading,
    Ready(VideoFrame),
    Failed(String),
}

/// A video ready to draw.
pub struct VideoFrame {
    pub texture: TextureHandle,
    /// Native size in pixels.
    pub size: Vec2,
    pub playing: bool,
    /// Current position in seconds.
    pub position: f64,
    /// Total duration in seconds.
    pub duration: f64,
}

#[cfg(windows)]
fn worker(
    path: PathBuf,
    receiver: Receiver<WorkerCmd>,
    sender: Sender<(PathBuf, WorkerMsg)>,
    repaint: &dyn Fn(),
) -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("initialize COM: {e}"))?;
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
            CoUninitialize();
            return Err(format!("MFStartup: {e}"));
        }
    }
    // All reader/decoder objects must be released before MFShutdown.
    let result = worker_inner(&path, receiver, sender, repaint);
    unsafe {
        let _ = MFShutdown();
        CoUninitialize();
    }
    result
}

#[cfg(windows)]
fn worker_inner(
    path: &Path,
    receiver: Receiver<WorkerCmd>,
    sender: Sender<(PathBuf, WorkerMsg)>,
    repaint: &dyn Fn(),
) -> Result<(), String> {
    use std::sync::mpsc::RecvTimeoutError;

    let mut decoder = Decoder::open(path)?;
    let send = |msg| {
        let sent = sender.send((path.to_owned(), msg)).is_ok();
        repaint();
        sent
    };
    send(WorkerMsg::Info {
        width: decoder.width,
        height: decoder.height,
        duration_s: decoder.duration_s,
    });
    // Show a poster while paused, so the viewer can display its Play control.
    let first = decoder.read_frame()?.ok_or("video has no frames")?;
    let mut position = first.pts;
    if !send(WorkerMsg::Frame(first)) {
        return Ok(());
    }

    let mut playing = false;
    // Bound the number of queued frames even when the document is not drawn.
    let mut awaiting_frame = true;
    let mut ended = false;
    let mut pending: Option<Frame> = None;
    let mut clock = (Instant::now(), position);
    loop {
        let cmd = if playing && !awaiting_frame {
            if pending.is_none() {
                pending = decoder.read_frame()?;
                if pending.is_none() {
                    // Keep the final frame on screen for its remaining time,
                    // then continue from the first frame without a paused UI.
                    let duration = decoder.duration_s.max(position + 1.0 / 30.0);
                    clock.0 += Duration::from_secs_f64((duration - clock.1).max(0.0));
                    clock.1 = 0.0;
                    decoder.seek(0.0)?;
                    pending = Some(decoder.read_frame()?.ok_or("video has no frames")?);
                }
            }
            // Pace decoding by the file's timestamps. Only one future frame is
            // held, and commands interrupt the wait immediately.
            let frame = pending.as_ref().unwrap();
            let due = clock.0 + Duration::from_secs_f64((frame.pts - clock.1).max(0.0));
            let delay = due.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(delay) {
                Ok(cmd) => cmd,
                Err(RecvTimeoutError::Timeout) => {
                    let frame = pending.take().unwrap();
                    position = frame.pts;
                    if !send(WorkerMsg::Frame(frame)) {
                        return Ok(());
                    }
                    awaiting_frame = true;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        } else {
            match receiver.recv() {
                Ok(cmd) => cmd,
                Err(_) => return Ok(()),
            }
        };
        match cmd {
            WorkerCmd::Play => {
                if ended {
                    decoder.seek(0.0)?;
                    position = 0.0;
                    pending = None;
                    ended = false;
                }
                clock = (Instant::now(), position);
                playing = true;
            }
            WorkerCmd::Pause => playing = false,
            WorkerCmd::FrameShown => awaiting_frame = false,
            WorkerCmd::Seek(to) => {
                decoder.seek(to)?;
                pending = None;
                playing = false;
                ended = false;
                if let Some(frame) = decoder.read_frame()? {
                    position = frame.pts;
                    if !send(WorkerMsg::Frame(frame)) {
                        return Ok(());
                    }
                    awaiting_frame = true;
                } else {
                    ended = true;
                }
                send(WorkerMsg::Paused);
            }
        }
    }
}

/// A synchronous reader owned exclusively by its COM-initialized worker.
#[cfg(windows)]
struct Decoder {
    reader: IMFSourceReader,
    width: u32,
    height: u32,
    stride: i32,
    duration_s: f64,
}

#[cfg(windows)]
impl Decoder {
    fn open(path: &Path) -> Result<Self, String> {
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1)
                .map_err(|e| format!("create reader attributes: {e}"))?;
            let attributes = attributes.ok_or("missing reader attributes")?;
            // Most video decoders produce YUV, not RGB. The Source Reader
            // needs its video processor enabled to negotiate RGB32 output.
            attributes
                .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                .map_err(|e| format!("enable video processing: {e}"))?;
            // The API accepts a local filename directly (including # and %).
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attributes)
                .map_err(|e| format!("create reader: {e}"))?;
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .map_err(|e| format!("deselect all: {e}"))?;
            reader
                .SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)
                .map_err(|e| format!("select video: {e}"))?;

            // Let the reader supply the decoded frame size, rate and stride;
            // attributes of the compressed native type may not apply to RGB.
            let out_type = MFCreateMediaType().map_err(|e| format!("create type: {e}"))?;
            out_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| format!("set major: {e}"))?;
            out_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                .map_err(|e| format!("set subtype: {e}"))?;
            reader
                .SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    None,
                    &out_type,
                )
                .map_err(|e| format!("set output type: {e}"))?;

            let mut decoder = Self {
                reader,
                width: 0,
                height: 0,
                stride: 0,
                duration_s: 0.0,
            };
            decoder.update_format()?;
            if let Ok(mut variant) = decoder
                .reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            {
                // MF_PD_DURATION is VT_UI8, measured in 100 ns units.
                if variant.Anonymous.Anonymous.vt == VT_UI8 {
                    decoder.duration_s =
                        variant.Anonymous.Anonymous.Anonymous.uhVal as f64 / 10_000_000.0;
                }
                let _ =
                    windows::Win32::System::Com::StructuredStorage::PropVariantClear(&mut variant);
            }
            Ok(decoder)
        }
    }

    fn update_format(&mut self) -> Result<(), String> {
        unsafe {
            let current = self
                .reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .map_err(|e| format!("get current type: {e}"))?;
            if current
                .GetGUID(&MF_MT_SUBTYPE)
                .map_err(|e| format!("get subtype: {e}"))?
                != MFVideoFormat_RGB32
            {
                return Err("decoder output is not RGB32".into());
            }
            let size = current
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .map_err(|e| format!("get frame size: {e}"))?;
            self.width = (size >> 32) as u32;
            self.height = size as u32;
            self.stride = match current.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
                Ok(stride) => stride as i32,
                Err(_) => MFGetStrideForBitmapInfoHeader(MFVideoFormat_RGB32.data1, self.width)
                    .map_err(|e| format!("get stride: {e}"))?,
            };
            Ok(())
        }
    }

    fn seek(&self, to: f64) -> Result<(), String> {
        unsafe {
            let mut var = PROPVARIANT::default();
            let anon = std::ptr::addr_of_mut!(var.Anonymous.Anonymous);
            (&mut *anon).vt = VT_I8;
            (&mut *anon).Anonymous.hVal = (to * 10_000_000.0) as i64;
            self.reader
                .SetCurrentPosition(&GUID::zeroed(), &var)
                .map_err(|e| format!("seek: {e}"))
        }
    }

    fn read_frame(&mut self) -> Result<Option<Frame>, String> {
        unsafe {
            loop {
                let mut flags = 0u32;
                let mut timestamp = 0i64;
                let mut sample = None;
                // Read once: a second ReadSample would discard every other frame.
                self.reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .map_err(|e| format!("read sample: {e}"))?;
                if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                    return Err("source reader reported a decoding error".into());
                }
                if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                    self.update_format()?;
                }
                if let Some(sample) = sample {
                    let buffer = sample
                        .ConvertToContiguousBuffer()
                        .map_err(|e| format!("get buffer: {e}"))?;
                    let mut data = std::ptr::null_mut();
                    let mut len = 0;
                    buffer
                        .Lock(&mut data, None, Some(&mut len))
                        .map_err(|e| format!("lock buffer: {e}"))?;
                    let image = if data.is_null() {
                        Err("empty video buffer".into())
                    } else {
                        rgb32_image(
                            std::slice::from_raw_parts(data, len as usize),
                            self.width,
                            self.height,
                            self.stride,
                        )
                    };
                    // Unlock even when the buffer dimensions are invalid.
                    let unlocked = buffer.Unlock();
                    let image = image?;
                    unlocked.map_err(|e| format!("unlock buffer: {e}"))?;
                    return Ok(Some(Frame {
                        image,
                        pts: timestamp as f64 / 10_000_000.0,
                    }));
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    return Ok(None);
                }
            }
        }
    }
}

/// RGB32 buffers can have row padding and can be stored bottom-up.
fn rgb32_image(data: &[u8], width: u32, height: u32, stride: i32) -> Result<ColorImage, String> {
    let row_bytes = (width as usize)
        .checked_mul(4)
        .ok_or("video row is too large")?;
    let pitch = stride.unsigned_abs() as usize;
    let height = height as usize;
    let needed = height
        .checked_sub(1)
        .and_then(|rows| rows.checked_mul(pitch))
        .and_then(|offset| offset.checked_add(row_bytes))
        .ok_or("invalid video dimensions")?;
    if width == 0 || pitch < row_bytes || data.len() < needed {
        return Err("video buffer is smaller than its frame dimensions".into());
    }
    let size = row_bytes
        .checked_mul(height)
        .ok_or("video frame is too large")?;
    let mut rgba = Vec::with_capacity(size);
    for y in 0..height {
        let row = if stride < 0 { height - 1 - y } else { y };
        for pixel in data[row * pitch..row * pitch + row_bytes]
            .as_chunks::<4>()
            .0
        {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
        }
    }
    Ok(ColorImage::from_rgba_unmultiplied(
        [width as usize, height],
        &rgba,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb32_handles_padding_and_bottom_up_rows() {
        // Two pixels per row plus four padding bytes. Alpha is ignored.
        let data = [
            0, 0, 255, 0, 0, 255, 0, 0, 99, 99, 99, 99, 255, 0, 0, 0, 255, 255, 255, 0,
        ];
        let top_down = rgb32_image(&data, 2, 2, 12).unwrap();
        assert_eq!(
            top_down.pixels,
            [
                egui::Color32::RED,
                egui::Color32::GREEN,
                egui::Color32::BLUE,
                egui::Color32::WHITE,
            ]
        );
        let bottom_up = rgb32_image(&data, 2, 2, -12).unwrap();
        assert_eq!(
            bottom_up.pixels,
            [
                egui::Color32::BLUE,
                egui::Color32::WHITE,
                egui::Color32::RED,
                egui::Color32::GREEN,
            ]
        );
    }

    #[test]
    fn rgb32_rejects_inconsistent_buffer_dimensions() {
        assert!(rgb32_image(&[0; 15], 2, 2, 8).is_err());
        assert!(rgb32_image(&[0; 16], 2, 2, 4).is_err());
        assert!(rgb32_image(&[], 0, 2, 0).is_err());
        assert!(rgb32_image(&[], 2, 0, 8).is_err());
    }

    #[test]
    fn clicking_the_playing_video_pauses_it() {
        let mut player = VideoPlayer::new(|| {});
        let path = PathBuf::from("test.mp4");
        let (worker_tx, worker_rx) = channel();
        player.videos.lock().unwrap().insert(
            path.clone(),
            Video {
                width: 32,
                height: 32,
                duration_s: 2.0,
                state: PlayState::Paused,
                user_paused: false,
                texture: None,
                position: 0.0,
                worker_tx,
                error: None,
            },
        );
        player.toggle(&path);
        assert!(matches!(worker_rx.try_recv(), Ok(WorkerCmd::Play)));
        player.toggle(&path);
        assert!(matches!(worker_rx.try_recv(), Ok(WorkerCmd::Pause)));
        assert!(player.videos.lock().unwrap()[&path].user_paused);
        assert_eq!(
            player.videos.lock().unwrap()[&path].state,
            PlayState::Paused
        );
    }

    #[test]
    fn autoplay_visibility_and_manual_pause_are_independent_for_each_video() {
        let mut player = VideoPlayer::new(|| {});
        let a = PathBuf::from("a.mp4");
        let b = PathBuf::from("b.mp4");
        let root = egui::ViewportId::ROOT;
        let other = egui::ViewportId::from_hash_of("other");
        for path in [&a, &b] {
            let (worker_tx, _) = channel();
            player.videos.lock().unwrap().insert(
                path.clone(),
                Video {
                    width: 64,
                    height: 64,
                    duration_s: 2.0,
                    state: PlayState::Paused,
                    user_paused: false,
                    texture: None,
                    position: 0.0,
                    worker_tx,
                    error: None,
                },
            );
        }
        player.begin_view(root);
        player.mark_visible(root, &a);
        player.mark_visible(root, &b);
        player.end_view();
        assert!(
            player
                .videos
                .lock()
                .unwrap()
                .values()
                .all(|v| v.state == PlayState::Playing)
        );
        player.toggle(&a);
        player.mark_visible(root, &a);
        assert_eq!(player.videos.lock().unwrap()[&a].state, PlayState::Paused);

        // Scrolling both out, or switching to a tab without them, suspends B.
        player.begin_view(root);
        player.end_view();
        assert!(
            player
                .videos
                .lock()
                .unwrap()
                .values()
                .all(|v| v.state == PlayState::Paused)
        );
        player.mark_visible(root, &a);
        player.mark_visible(root, &b);
        player.end_view();
        assert_eq!(player.videos.lock().unwrap()[&a].state, PlayState::Paused);
        assert_eq!(player.videos.lock().unwrap()[&b].state, PlayState::Playing);

        // A clip still painted in another window stays playing.
        player.mark_visible(other, &b);
        player.begin_view(root);
        player.end_view();
        assert_eq!(player.videos.lock().unwrap()[&b].state, PlayState::Playing);
        player.retain_views(|viewport| viewport == root);
        assert_eq!(player.videos.lock().unwrap()[&b].state, PlayState::Paused);
    }

    #[cfg(windows)]
    #[test]
    fn h264_preview_play_pause_seek_and_replay() {
        // H.264/YUV420, 64x64, 10 fps, 2 seconds; red above blue.
        // A filename with URL-significant characters also exercises direct paths.
        let path = std::env::temp_dir().join(format!(
            "metalmd-video-{}-тест #100%.mp4",
            std::process::id()
        ));
        std::fs::write(&path, include_bytes!("../../tests/fixtures/video-h264.mp4")).unwrap();
        let (commands, receiver) = channel();
        let (sender, messages) = channel();
        let worker_path = path.clone();
        let repaints = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_repaints = repaints.clone();
        let thread = std::thread::spawn(move || {
            worker(worker_path, receiver, sender.clone(), &|| {
                worker_repaints.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
            .expect("native video worker")
        });
        let receive = || {
            messages
                .recv_timeout(Duration::from_secs(10))
                .expect("worker response")
                .1
        };
        match receive() {
            WorkerMsg::Info {
                width,
                height,
                duration_s,
            } => {
                assert_eq!((width, height), (64, 64));
                assert!((duration_s - 2.0).abs() < 0.01);
            }
            _ => panic!("expected video metadata"),
        }
        let receive_frame = || match receive() {
            WorkerMsg::Frame(frame) => {
                commands.send(WorkerCmd::FrameShown).unwrap();
                frame
            }
            _ => panic!("expected a decoded frame"),
        };
        let preview = receive_frame();
        assert!(preview.pts.abs() < 0.01);
        let top = preview.image.pixels[16 * 64 + 32];
        let bottom = preview.image.pixels[48 * 64 + 32];
        assert!(top.r() > 200 && top.b() < 50, "red must be above blue");
        assert!(bottom.b() > 200 && bottom.r() < 50);
        assert!(
            messages.recv_timeout(Duration::from_millis(150)).is_err(),
            "paused preview"
        );

        let start = Instant::now();
        commands.send(WorkerCmd::Play).unwrap();
        let next = match receive() {
            WorkerMsg::Frame(frame) => frame,
            _ => panic!("expected a playback frame"),
        };
        assert!(
            (next.pts - 0.1).abs() < 0.01,
            "must not skip every other frame"
        );
        assert!(
            start.elapsed() >= Duration::from_millis(70),
            "must pace frames"
        );
        assert!(
            messages.recv_timeout(Duration::from_millis(150)).is_err(),
            "decoding must wait for the UI to consume its frame"
        );
        commands.send(WorkerCmd::Pause).unwrap();
        commands.send(WorkerCmd::FrameShown).unwrap();
        // Seek while paused must decode and show the new position.
        commands.send(WorkerCmd::Seek(1.0)).unwrap();
        let sought = receive_frame();
        assert!((sought.pts - 1.0).abs() < 0.01);
        assert!(matches!(receive(), WorkerMsg::Paused));
        assert!(messages.recv_timeout(Duration::from_millis(150)).is_err());

        commands.send(WorkerCmd::Play).unwrap();
        loop {
            match receive() {
                WorkerMsg::Frame(frame) => {
                    commands.send(WorkerCmd::FrameShown).unwrap();
                    if frame.pts < sought.pts {
                        assert!(frame.pts.abs() < 0.01, "loop restarts at the beginning");
                        break;
                    }
                }
                _ => panic!("unexpected playback response"),
            }
        }
        drop(commands);
        thread.join().unwrap();
        assert!(repaints.load(std::sync::atomic::Ordering::Relaxed) > 5);
        std::fs::remove_file(path).unwrap();
    }
}
