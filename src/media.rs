use crate::audio::Loopback;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::windows::{io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::*,
};
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Settings {
    pub seconds: u32,
    pub fps: u32,
    pub key: u32,
    pub audio: bool,
    pub monitor: usize,
    pub height: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            seconds: 30,
            fps: 30,
            key: 8,
            audio: true,
            monitor: 0,
            height: 1080,
        }
    }
}
impl Settings {
    pub fn valid(&self) -> bool {
        [15, 30, 45, 60, 90, 120].contains(&self.seconds)
            && [30, 60].contains(&self.fps)
            && (1..=12).contains(&self.key)
            && [720, 1080, 1440].contains(&self.height)
    }
}
static SEQ: AtomicU64 = AtomicU64::new(0);
pub fn unique() -> String {
    format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}
pub fn command(tool: &str) -> Command {
    let local = std::env::current_exe()
        .unwrap()
        .with_file_name(format!("{tool}.exe"));
    let mut c = if local.exists() {
        Command::new(local)
    } else {
        Command::new(tool)
    };
    c.creation_flags(0x08000000).stdin(Stdio::null());
    c
}
pub fn run(cmd: &mut Command) -> Result<Vec<u8>> {
    let r=cmd.output().context("Could not launch the media tool. Install FFmpeg and FFprobe or place both beside Afterimage.exe.")?;
    ensure!(
        r.status.success(),
        "{}",
        String::from_utf8_lossy(&r.stderr)
            .chars()
            .rev()
            .take(500)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    );
    Ok(r.stdout)
}
pub fn duration(path: &Path) -> Result<f64> {
    let out = run(command("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path))?;
    Ok(String::from_utf8(out)?.trim().parse()?)
}
pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new(parent: &Path) -> Result<Self> {
        let dir = parent.join(unique());
        fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Job(HANDLE);
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
impl Job {
    fn attach(child: &Child) -> Result<Self> {
        unsafe {
            let j = Self(CreateJobObjectW(None, None)?);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                j.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                std::mem::size_of_val(&limits) as u32,
            )?;
            AssignProcessToJobObject(j.0, HANDLE(child.as_raw_handle()))?;
            Ok(j)
        }
    }
}
pub struct Recorder {
    child: Child,
    _job: Job,
    dir: Scratch,
    audio: Option<Loopback>,
}
impl Recorder {
    pub fn start(root: &Path, s: &Settings, display: (i32, i32, u32, u32)) -> Result<Self> {
        let dir = Scratch::new(&root.join("rust-buffer"))?;
        let (x, y, w, h) = display;
        let err = fs::File::create(dir.0.join("capture.log"))?;
        let mut child = command("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "gdigrab",
                "-framerate",
                &s.fps.to_string(),
                "-offset_x",
                &x.to_string(),
                "-offset_y",
                &y.to_string(),
                "-video_size",
                &format!("{w}x{h}"),
                "-i",
                "desktop",
                "-vf",
                &format!("scale=-2:trunc(min(ih\\,{})/2)*2", s.height),
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-crf",
                "22",
                "-pix_fmt",
                "yuv420p",
                "-g",
                &s.fps.to_string(),
                "-sc_threshold",
                "0",
                "-f",
                "segment",
                "-segment_time",
                "1",
                "-reset_timestamps",
                "1",
            ])
            .arg(dir.0.join("%08d.ts"))
            .stdout(Stdio::null())
            .stderr(err)
            .spawn()
            .context("Could not start FFmpeg")?;
        let job = Job::attach(&child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?;
        Ok(Self {
            child,
            _job: job,
            dir,
            audio: s.audio.then(Loopback::start),
        })
    }
    fn segments(&self) -> Result<Vec<PathBuf>> {
        let mut files = fs::read_dir(&self.dir.0)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "ts"))
            .collect::<Vec<_>>();
        files.sort();
        Ok(files)
    }
    pub fn tick(&mut self) -> Result<usize> {
        if self.child.try_wait()?.is_some() {
            bail!(
                "Capture stopped. {}",
                fs::read_to_string(self.dir.0.join("capture.log"))
                    .unwrap_or_default()
                    .chars()
                    .take(350)
                    .collect::<String>()
            )
        }
        let files = self.segments()?;
        for f in files.iter().take(files.len().saturating_sub(123)) {
            fs::remove_file(f)?;
        }
        Ok(files.len().saturating_sub(1).min(120))
    }
    pub fn audio_error(&self) -> String {
        self.audio.as_ref().map(Loopback::error).unwrap_or_default()
    }
    pub fn snapshot(&self, seconds: usize, root: &Path) -> Result<Snapshot> {
        snapshot_files(self.segments()?, self.audio.as_ref(), seconds, root)
    }
}
fn snapshot_files(
    mut files: Vec<PathBuf>,
    loopback: Option<&Loopback>,
    seconds: usize,
    root: &Path,
) -> Result<Snapshot> {
    files.pop();
    ensure!(
        !files.is_empty(),
        "Recording just started. Try again in a second."
    );
    let files = &files[files.len().saturating_sub(seconds)..];
    let dir = Scratch::new(&root.join("rust-jobs"))?;
    let mut list = String::new();
    for (i, file) in files.iter().enumerate() {
        fs::copy(file, dir.0.join(format!("{i}.ts")))?;
        list.push_str(&format!("file '{i}.ts'\n"));
    }
    fs::write(dir.0.join("list.txt"), list)?;
    let mut audio = false;
    if let Some(r) = loopback {
        audio = r.save(
            &dir.0.join("audio.wav"),
            fs::metadata(files.last().unwrap())?.modified()?,
            files.len(),
        )?;
    }
    Ok(Snapshot { dir, audio })
}
impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub struct Snapshot {
    dir: Scratch,
    audio: bool,
}
impl Snapshot {
    pub fn finish(self, target: &Path) -> Result<()> {
        let mut c = command("ffmpeg");
        c.current_dir(&self.dir.0).args([
            "-v", "error", "-f", "concat", "-safe", "0", "-i", "list.txt",
        ]);
        if self.audio {
            c.args([
                "-i",
                "audio.wav",
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
            ]);
        }
        publish(c.args(["-c:v", "copy", "-movflags", "+faststart"]), target)
    }
}
fn publish(c: &mut Command, target: &Path) -> Result<()> {
    let temp = target.with_file_name(format!(".afterimage-{}.part", unique()));
    let result = run(c.args(["-f", "mp4"]).arg(&temp)).and_then(|_| Ok(fs::rename(&temp, target)?));
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.map(|_| ())
}
pub fn export(
    input: &Path,
    target: &Path,
    start: f64,
    end: f64,
    shape: u32,
    mute: bool,
) -> Result<()> {
    ensure!(
        start.is_finite() && end.is_finite() && start >= 0.0 && end > start,
        "Choose a valid selection."
    );
    let mut c = command("ffmpeg");
    c.args(["-v", "error", "-ss", &start.to_string(), "-i"])
        .arg(input)
        .args([
            "-t",
            &(end - start).to_string(),
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-vf",
            match shape {
                1 => "crop=trunc(min(iw\\,ih*9/16)/2)*2:trunc(min(ih\\,iw*16/9)/2)*2,setsar=1",
                2 => "crop=trunc(min(iw\\,ih)/2)*2:trunc(min(iw\\,ih)/2)*2,setsar=1",
                _ => "scale=trunc(iw/2)*2:trunc(ih/2)*2,setsar=1",
            },
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "19",
            "-pix_fmt",
            "yuv420p",
        ]);
    if mute {
        c.arg("-an");
    } else {
        c.args(["-c:a", "aac", "-b:a", "192k"]);
    }
    publish(c.args(["-movflags", "+faststart"]), target)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_lengths_and_vertical_export() -> Result<()> {
        let dir = Scratch::new(&std::env::temp_dir().join("afterimage-media-tests"))?;
        run(command("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=10",
                "-t",
                "47",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "10",
                "-sc_threshold",
                "0",
                "-f",
                "segment",
                "-segment_time",
                "1",
                "-reset_timestamps",
                "1",
            ])
            .arg(dir.0.join("%08d.ts")))?;
        let mut files = fs::read_dir(&dir.0)?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .map(|e| e.path())
            .collect::<Vec<_>>();
        files.sort();
        for (request, expected) in [(30, 30.0), (45, 45.0), (120, 46.0)] {
            let path = dir.0.join(format!("replay-{request}.mp4"));
            snapshot_files(files.clone(), None, request, &dir.0)?.finish(&path)?;
            assert!((duration(&path)? - expected).abs() < 0.05);
        }
        let edited = dir.0.join("edit.mp4");
        export(&dir.0.join("replay-45.mp4"), &edited, 1.0, 3.0, 1, true)?;
        assert!((duration(&edited)? - 2.0).abs() < 0.05);
        let info = run(command("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=width,height,codec_type",
                "-of",
                "json",
            ])
            .arg(edited))?;
        let info: serde_json::Value = serde_json::from_slice(&info)?;
        let streams = info["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0]["width"], 50);
        assert_eq!(streams[0]["height"], 90);
        Ok(())
    }
}
