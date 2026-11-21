use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    hash::{Hash, Hasher},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use wry::http::{Request, Response};
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Clip {
    pub file: PathBuf,
    pub title: String,
    pub star: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created: String,
}
pub fn id(path: &Path) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    format!("{:016x}", h.finish())
}
pub fn read<T: DeserializeOwned + Default>(p: &Path) -> Result<T> {
    if !p.exists() {
        return Ok(T::default());
    }
    Ok(serde_json::from_slice(&fs::read(p)?)
        .with_context(|| format!("Could not read {}", p.display()))?)
}
pub fn write<T: Serialize>(p: &Path, value: &T) -> Result<()> {
    let tmp = p.with_extension("rust.tmp");
    fs::write(&tmp, serde_json::to_vec(value)?)?;
    fs::rename(tmp, p)?;
    Ok(())
}
pub type Files = Arc<RwLock<HashMap<String, PathBuf>>>;
pub struct Library {
    pub clips: Vec<Clip>,
    pub files: Files,
    pub cuts: HashMap<PathBuf, Value>,
    pub root: PathBuf,
    pub videos: PathBuf,
}
impl Library {
    pub fn load(root: PathBuf, videos: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("thumbs"))?;
        fs::create_dir_all(&videos)?;
        let mut clips: Vec<Clip> = read(&root.join("library.json"))?;
        clips.retain(|c| c.file.is_file());
        for p in fs::read_dir(&videos)?.flatten().map(|e| e.path()) {
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4"))
                && !clips.iter().any(|c| c.file == p)
            {
                let created = fs::metadata(&p)?.modified()?;
                clips.push(Clip {
                    title: p
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    file: p,
                    star: false,
                    tags: Vec::new(),
                    created: chrono::DateTime::<chrono::Local>::from(created)
                        .format("%Y-%m-%dT%H:%M:%S")
                        .to_string(),
                });
            }
        }
        clips.sort_by(|a, b| b.created.cmp(&a.created));
        let files = Arc::new(RwLock::new(
            clips
                .iter()
                .map(|c| (id(&c.file), c.file.clone()))
                .collect(),
        ));
        let cuts = read(&root.join("cuts.json"))?;
        Ok(Self {
            clips,
            files,
            cuts,
            root,
            videos,
        })
    }
    pub fn add(&mut self, file: PathBuf, title: String) -> Result<()> {
        self.files.write().unwrap().insert(id(&file), file.clone());
        self.clips.insert(
            0,
            Clip {
                file,
                title,
                star: false,
                tags: Vec::new(),
                created: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            },
        );
        self.persist()
    }
    pub fn persist(&self) -> Result<()> {
        write(&self.root.join("library.json"), &self.clips)
    }
    pub fn rows(&self) -> Vec<Value> {
        self.clips.iter().map(|c|json!({"id":id(&c.file),"title":c.title,"tags":c.tags,"star":c.star,"date":c.created,"draft":self.cuts.get(&c.file)})).collect()
    }
    pub fn target(&self, prefix: &str) -> PathBuf {
        self.videos.join(format!(
            "{prefix} {} {}.mp4",
            chrono::Local::now().format("%Y-%m-%d %H-%M-%S"),
            crate::media::unique()
        ))
    }
}
pub fn serve(req: Request<Vec<u8>>, files: &Files, root: &Path) -> Response<Vec<u8>> {
    let route = req.uri().path();
    let fail = || Response::builder().status(404).body(Vec::new()).unwrap();
    if route == "/" {
        return Response::builder().header("Content-Type","text/html; charset=utf-8").header("Content-Security-Policy","default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src 'self' data:; media-src 'self'; object-src 'none'; base-uri 'none'; frame-src 'none'").body(include_bytes!("ui.html").to_vec()).unwrap();
    }
    let Some((kind, key)) = route.trim_start_matches('/').split_once('/') else {
        return fail();
    };
    let Some(file) = files.read().unwrap().get(key).cloned() else {
        return fail();
    };
    if kind == "thumb" || kind == "strip" {
        let p = root.join("thumbs").join(format!("{key}-{kind}.jpg"));
        return match fs::read(p) {
            Ok(bytes) => Response::builder()
                .header("Content-Type", "image/jpeg")
                .body(bytes)
                .unwrap(),
            Err(_) => fail(),
        };
    }
    if kind != "video" {
        return fail();
    }
    let Ok(mut f) = fs::File::open(file) else {
        return fail();
    };
    let Ok(m) = f.metadata() else { return fail() };
    let len = m.len();
    if len == 0 {
        return fail();
    }
    let range = req
        .headers()
        .get("Range")
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split_once('-'));
    let (start, last) = range.map_or((0, len - 1), |(a, b)| {
        (a.parse().unwrap_or(0), b.parse().unwrap_or(len - 1))
    });
    if start >= len || last < start {
        return Response::builder()
            .status(416)
            .header("Content-Range", format!("bytes */{len}"))
            .body(vec![])
            .unwrap();
    }
    let end = last
        .min(len - 1)
        .min(start.saturating_add(2 * 1024 * 1024 - 1));
    let partial = range.is_some() || end + 1 < len;
    let mut response = Response::builder()
        .header("Content-Type", "video/mp4")
        .header("Accept-Ranges", "bytes");
    if partial {
        response = response
            .status(206)
            .header("Content-Range", format!("bytes {start}-{end}/{len}"));
    }
    let mut bytes = vec![0; (end - start + 1) as usize];
    if f.seek(SeekFrom::Start(start))
        .and_then(|_| f.read_exact(&mut bytes))
        .is_err()
    {
        return fail();
    }
    response
        .header("Content-Length", bytes.len().to_string())
        .body(bytes)
        .unwrap()
}
