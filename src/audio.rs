use anyhow::Result;
use std::{
    collections::VecDeque,
    fs::File,
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime},
};
use windows::Win32::{Media::Audio::*, System::Com::*};

#[derive(Clone)]
struct Packet {
    end: SystemTime,
    bytes: Arc<[u8]>,
}
#[derive(Default)]
struct Data {
    fmt: Vec<u8>,
    hz: usize,
    align: usize,
    packets: VecDeque<Packet>,
    error: String,
}
pub struct Loopback {
    data: Arc<Mutex<Data>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Loopback {
    pub fn start() -> Self {
        let data = Arc::new(Mutex::new(Data::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (d, s) = (data.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let result = unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED)
                    .ok()
                    .and_then(|_| {
                        let r = capture(&d, &s);
                        CoUninitialize();
                        r
                    })
            };
            if let Err(e) = result {
                d.lock().unwrap().error = e.to_string();
            }
        });
        Self {
            data,
            stop,
            worker: Some(worker),
        }
    }
    pub fn error(&self) -> String {
        self.data.lock().unwrap().error.clone()
    }
    pub fn snapshot(&self, end: SystemTime, seconds: usize) -> Option<AudioClip> {
        let d = self.data.lock().unwrap();
        if d.fmt.is_empty() {
            return None;
        }
        let start = end - Duration::from_secs(seconds as u64);
        let packets = d
            .packets
            .iter()
            .filter(|p| p.end >= start)
            .cloned()
            .collect();
        Some(AudioClip {
            fmt: d.fmt.clone(),
            hz: d.hz,
            align: d.align,
            packets,
            start,
            seconds,
        })
    }
}
pub struct AudioClip {
    fmt: Vec<u8>,
    hz: usize,
    align: usize,
    packets: Vec<Packet>,
    start: SystemTime,
    seconds: usize,
}
impl AudioClip {
    pub fn write(&self, path: &Path) -> Result<()> {
        let size = self.seconds * self.hz * self.align;
        let mut pcm = vec![0; size];
        for p in &self.packets {
            let Ok(elapsed) = p.end.duration_since(self.start) else {
                continue;
            };
            let finish =
                (elapsed.as_secs_f64() * self.hz as f64).round() as i64 * self.align as i64;
            let offset = finish - p.bytes.len() as i64;
            let lo = offset.max(0);
            let hi = finish.min(size as i64);
            if hi > lo {
                pcm[lo as usize..hi as usize]
                    .copy_from_slice(&p.bytes[(lo - offset) as usize..(hi - offset) as usize]);
            }
        }
        let fmt = &self.fmt;
        let mut f = File::create(path)?;
        f.write_all(b"RIFF")?;
        f.write_all(&((20 + fmt.len() + size) as u32).to_le_bytes())?;
        f.write_all(b"WAVEfmt ")?;
        f.write_all(&(fmt.len() as u32).to_le_bytes())?;
        f.write_all(fmt)?;
        f.write_all(b"data")?;
        f.write_all(&(size as u32).to_le_bytes())?;
        f.write_all(&pcm)?;
        Ok(())
    }
}
impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}
fn capture(data: &Mutex<Data>, stop: &AtomicBool) -> windows::core::Result<()> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            1_000_000,
            0,
            format,
            None,
        );
        let hz = (*format).nSamplesPerSec as usize;
        let align = (*format).nBlockAlign as usize;
        let fmt = std::slice::from_raw_parts(format.cast::<u8>(), 18 + (*format).cbSize as usize)
            .to_vec();
        CoTaskMemFree(Some(format.cast()));
        init?;
        let reader: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        {
            let mut d = data.lock().unwrap();
            d.hz = hz;
            d.align = align;
            d.fmt = fmt;
        }
        let mut anchor: Option<(u64, SystemTime)> = None;
        while !stop.load(Ordering::Acquire) {
            while reader.GetNextPacketSize()? > 0 {
                let (mut ptr, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0, 0, 0);
                reader.GetBuffer(&mut ptr, &mut frames, &mut flags, None, Some(&mut qpc))?;
                let len = frames as usize * align;
                let bytes = if ptr.is_null() || flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    vec![0; len]
                } else {
                    std::slice::from_raw_parts(ptr, len).to_vec()
                };
                reader.ReleaseBuffer(frames)?;
                let dur = Duration::from_secs_f64(frames as f64 / hz as f64);
                let (base, t) = *anchor.get_or_insert((qpc, SystemTime::now() - dur));
                let end = t + Duration::from_nanos(qpc.saturating_sub(base) * 100) + dur;
                let mut d = data.lock().unwrap();
                while d
                    .packets
                    .front()
                    .is_some_and(|p| end.duration_since(p.end).unwrap_or_default().as_secs() > 125)
                {
                    d.packets.pop_front();
                }
                d.packets.push_back(Packet {
                    end,
                    bytes: bytes.into(),
                });
            }
            thread::sleep(Duration::from_millis(8));
        }
        client.Stop()
    }
}
