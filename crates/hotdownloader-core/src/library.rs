//! 下载目录音频文件管理。
//!
//! 曲库展示的是磁盘上真实存在的歌曲文件，而不是任务记录，因此这里
//! 直接读写下载目录中的音频文件（含子目录）：扫描、删除、重命名、写入标签、读取内嵌封面。
//! 供 Tauri 命令、独立服务 HTTP 入口与 MCP 工具共用。

use std::path::{Component, Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::tag::{ItemKey, Tag, TagType};
use serde::{Deserialize, Serialize};

/// 下载落盘后常见的可播放音频扩展名（小写、不含点）。
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "wma", "ape",
];

/// 重命名时会被替换为下划线的非法字符。
const ILLEGAL_NAME_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// 单个音频文件的元信息，字段与前端 `AudioFileEntry` 契约保持一致。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioFileEntry {
    /// 文件名（含扩展名），不含目录。
    pub name: String,
    /// 文件绝对路径。
    pub path: String,
    /// 文件字节大小。
    pub size: u64,
    /// 小写扩展名，不含点。
    pub extension: String,
    /// 文件内是否嵌有封面图片。
    pub has_cover: bool,
}

/// 递归扫描 `directory`，返回按相对路径排序的音频文件。
///
/// 符号链接目录不会进入，避免循环或扫到下载目录之外。下载目录本身不可读时返回错误；
/// 子目录或单个损坏条目会被跳过，不中断整个列表。
pub fn list_audio_files(directory: &str) -> Result<Vec<AudioFileEntry>, String> {
    let root = Path::new(directory);
    let mut files = Vec::new();
    collect_audio_files(root, true, &mut files)?;
    files.sort_by(|left, right| {
        relative_sort_key(root, &left.path).cmp(&relative_sort_key(root, &right.path))
    });
    Ok(files)
}

/// 收集目录中的音频文件。`is_root` 为真时，目录读失败要报错而不是跳过。
fn collect_audio_files(
    dir: &Path,
    is_root: bool,
    files: &mut Vec<AudioFileEntry>,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if is_root => return Err(format!("读取下载目录失败: {error}")),
        Err(error) => {
            log::warn!("跳过无法读取的目录 {}: {error}", dir.display());
            return Ok(());
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };

        // 符号链接指向目录时不递归；指向文件时仍按普通音频处理。
        if file_type.is_symlink() {
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => push_audio(&path, metadata.len(), files),
                _ => {}
            }
            continue;
        }
        if file_type.is_dir() {
            collect_audio_files(&path, false, files)?;
            continue;
        }
        if file_type.is_file() {
            let Ok(metadata) = entry.metadata() else { continue };
            push_audio(&path, metadata.len(), files);
        }
    }
    Ok(())
}

fn push_audio(path: &Path, size: u64, files: &mut Vec<AudioFileEntry>) {
    let Some(extension) = audio_extension(path) else {
        return;
    };
    let Some(name) = path.file_name() else {
        return;
    };
    files.push(AudioFileEntry {
        name: name.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        size,
        extension,
        has_cover: has_embedded_cover(path),
    });
}

/// 相对下载根目录的路径，用 `/` 排序，避免只按文件名把不同目录混在一起。
fn relative_sort_key(root: &Path, path: &str) -> String {
    Path::new(path)
        .strip_prefix(root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.replace('\\', "/"))
}

/// 单个文件删除失败的信息。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteFailure {
    /// 请求删除的原始路径。
    pub path: String,
    /// 失败原因。
    pub error: String,
}

/// 批量删除结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    /// 成功删除的文件数量。
    pub deleted: usize,
    /// 删除失败的条目。
    pub failed: Vec<DeleteFailure>,
}

/// 删除 `directory` 下由 `paths` 指定的音频文件。
///
/// 每条路径都会先规范化，并确认落在下载目录内。目录外、`..` 穿越或非音频文件记入失败，
/// 不会删除目标之外的文件。
pub fn delete_audio_files(directory: &str, paths: &[String]) -> Result<DeleteResult, String> {
    if !Path::new(directory).is_dir() {
        return Err(format!("下载目录不存在: {directory}"));
    }

    let mut deleted = 0usize;
    let mut failed = Vec::new();
    for raw in paths {
        let target = match resolve_audio_file(directory, raw) {
            Ok(target) => target,
            Err(error) => {
                failed.push(DeleteFailure {
                    path: raw.clone(),
                    error,
                });
                continue;
            }
        };
        match std::fs::remove_file(&target) {
            Ok(()) => deleted += 1,
            Err(error) => failed.push(DeleteFailure {
                path: raw.clone(),
                error: error.to_string(),
            }),
        }
    }

    Ok(DeleteResult { deleted, failed })
}

/// 校验并定位下载目录内的音频文件。
///
/// 接受绝对路径或相对于下载目录的路径。路径中出现 `..` 直接拒绝；
/// 规范化后若不再位于下载目录内，同样拒绝。
fn resolve_audio_file(directory: &str, path: &str) -> Result<PathBuf, String> {
    let root = Path::new(directory);
    if !root.is_dir() {
        return Err(format!("下载目录不存在: {directory}"));
    }
    if path.trim().is_empty() {
        return Err("非法文件路径".to_string());
    }
    let raw = Path::new(path);
    if raw
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("路径不能包含 ..".to_string());
    }

    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };
    let root = root
        .canonicalize()
        .map_err(|error| format!("无法解析下载目录: {error}"))?;
    let target = candidate
        .canonicalize()
        .map_err(|_| format!("文件不存在: {}", candidate.display()))?;
    if target == root || !target.starts_with(&root) {
        return Err("文件不在下载目录内".to_string());
    }
    if !target.is_file() {
        return Err(format!("文件不存在: {}", target.display()));
    }
    if audio_extension(&target).is_none() {
        return Err("不是可操作的音频文件".to_string());
    }
    Ok(target)
}

fn audio_extension(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        Some(extension)
    } else {
        None
    }
}

/// 根据磁盘文件构造 `AudioFileEntry`。
fn entry_for(path: &Path) -> Result<AudioFileEntry, String> {
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("读取文件信息失败: {error}"))?;
    let extension = audio_extension(path).ok_or_else(|| "不是可操作的音频文件".to_string())?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("非法文件名")?
        .to_string();
    Ok(AudioFileEntry {
        name,
        path: path.to_string_lossy().into_owned(),
        size: metadata.len(),
        extension,
        has_cover: has_embedded_cover(path),
    })
}

/// 规范化用户提供的文件名：去掉目录部分、替换非法字符、必要时补上音频扩展名。
fn normalize_file_name(raw: &str, fallback_extension: &str) -> Result<String, String> {
    let name = Path::new(raw)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("非法文件名")?
        .trim();
    if name.is_empty() {
        return Err("文件名不能为空".to_string());
    }
    let mut cleaned: String = name
        .chars()
        .map(|ch| if ILLEGAL_NAME_CHARS.contains(&ch) { '_' } else { ch })
        .collect();
    let extension = Path::new(&cleaned)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !AUDIO_EXTENSIONS.contains(&extension.as_str()) {
        let base = cleaned.trim_end_matches('.').trim();
        if base.is_empty() {
            return Err("文件名不能为空".to_string());
        }
        cleaned = format!("{base}.{fallback_extension}");
    }
    Ok(cleaned)
}

/// 重命名下载目录内的音频文件，返回新的文件条目。
///
/// `new_name` 只取文件名部分，结果留在文件当前所在目录，不会搬回下载根目录。
/// 不含受支持扩展名时会沿用原扩展名。
pub fn rename_audio_file(
    directory: &str,
    path: &str,
    new_name: &str,
) -> Result<AudioFileEntry, String> {
    let old_path = resolve_audio_file(directory, path)?;
    let fallback_extension = old_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp3")
        .to_ascii_lowercase();
    let file_name = normalize_file_name(new_name, &fallback_extension)?;
    let new_path = old_path
        .parent()
        .ok_or_else(|| "无法确定文件所在目录".to_string())?
        .join(&file_name);
    let root = Path::new(directory)
        .canonicalize()
        .map_err(|error| format!("无法解析下载目录: {error}"))?;
    if new_path == root || !new_path.starts_with(&root) {
        return Err("文件不在下载目录内".to_string());
    }

    if new_path == old_path {
        return entry_for(&old_path);
    }
    if new_path.exists() {
        return Err(format!("目标文件已存在: {file_name}"));
    }
    std::fs::rename(&old_path, &new_path).map_err(|error| format!("重命名失败: {error}"))?;
    entry_for(&new_path)
}

/// 音频标签修改请求：仅提供的字段会被写入，空字符串表示清除该字段，
/// 未提供的字段保持不变。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataUpdate {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub lyrics: Option<String>,
}

/// 写入/修改下载目录内音频文件的标签（标题、歌手、专辑、歌词）。
///
/// 路径规则与删除相同：绝对或相对路径都可以，但必须落在下载目录内。
pub fn write_audio_metadata(
    directory: &str,
    path: &str,
    update: &MetadataUpdate,
) -> Result<(), String> {
    if update.title.is_none()
        && update.artist.is_none()
        && update.album.is_none()
        && update.lyrics.is_none()
    {
        return Err("没有需要修改的标签字段".to_string());
    }
    let target = resolve_audio_file(directory, path)?;

    let mut tagged_file =
        lofty::read_from_path(&target).map_err(|error| format!("读取音频文件失败: {error}"))?;
    // 与下载收尾保持一致：移除 128 字节的旧 ID3v1，避免多字节字符截断。
    if tagged_file.remove(TagType::Id3v1).is_some() {
        log::info!("已移除 ID3v1 标签，避免多字节字符截断引发崩溃");
    }
    let tag_type = tagged_file.primary_tag_type();
    if tagged_file.primary_tag().is_none() {
        tagged_file.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged_file
        .primary_tag_mut()
        .ok_or_else(|| "无法获取音频标签".to_string())?;

    for (key, value) in [
        (ItemKey::TrackTitle, update.title.as_ref()),
        (ItemKey::TrackArtist, update.artist.as_ref()),
        (ItemKey::AlbumTitle, update.album.as_ref()),
        (ItemKey::Lyrics, update.lyrics.as_ref()),
    ] {
        let Some(value) = value else { continue };
        tag.remove_key(&key);
        if !value.is_empty() {
            tag.insert_text(key, value.clone());
        }
    }

    tagged_file
        .save_to_path(&target, WriteOptions::default())
        .map_err(|error| format!("保存标签失败: {error}"))?;
    log::info!("音频标签已写入: {}", target.display());
    Ok(())
}

/// 音频文件内嵌封面的原始字节。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioCover {
    /// 图片 MIME，例如 `image/jpeg`。无法识别时为 `application/octet-stream`。
    pub mime: String,
    /// 封面原始字节，对应 lofty `Picture` 的数据。
    pub data: Vec<u8>,
}

/// 读取下载目录内音频文件的内嵌封面。
///
/// 路径校验与删除相同。文件没有图片时返回 `Ok(None)`，而不是错误。
pub fn read_audio_cover(directory: &str, path: &str) -> Result<Option<AudioCover>, String> {
    let target = resolve_audio_file(directory, path)?;
    read_embedded_cover(&target)
}

fn has_embedded_cover(path: &Path) -> bool {
    matches!(read_embedded_cover(path), Ok(Some(_)))
}

/// 优先取封面类型的图片，否则取第一张非空内嵌图。
fn read_embedded_cover(path: &Path) -> Result<Option<AudioCover>, String> {
    let tagged = lofty::read_from_path(path)
        .map_err(|error| format!("读取音频文件失败: {error}"))?;
    let mut fallback = None;
    for tag in tagged.tags() {
        for picture in tag.pictures() {
            if picture.data().is_empty() {
                continue;
            }
            let cover = cover_from_picture(picture);
            if picture.pic_type() == PictureType::CoverFront {
                return Ok(Some(cover));
            }
            if fallback.is_none() {
                fallback = Some(cover);
            }
        }
    }
    Ok(fallback)
}

fn cover_from_picture(picture: &Picture) -> AudioCover {
    let mime = picture
        .mime_type()
        .map(MimeType::as_str)
        .filter(|mime| is_safe_mime(mime))
        .unwrap_or("application/octet-stream");
    AudioCover {
        mime: mime.to_string(),
        data: picture.data().to_vec(),
    }
}

fn is_safe_mime(mime: &str) -> bool {
    !mime.is_empty()
        && mime.contains('/')
        && mime.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::picture::{Picture, PictureType};
    use lofty::tag::{Tag, TagType};

    use super::{
        delete_audio_files, list_audio_files, read_audio_cover, rename_audio_file,
        write_audio_metadata, MetadataUpdate,
    };

    /// 1×1 红色 PNG，CRC 由构造时算好，供 lofty 识别为内嵌封面。
    const PNG_1X1: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8,
        2, 0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0,
        0, 3, 1, 1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "hotdownloader-library-{:x}",
                rand::random::<u64>()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn symlink_dir(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(target, link).unwrap();
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            panic!("当前平台无法创建目录符号链接");
        }
    }

    fn relative_paths(root: &Path, files: &[super::AudioFileEntry]) -> Vec<String> {
        files
            .iter()
            .map(|file| {
                Path::new(&file.path)
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    /// 最小 PCM WAV：8 kHz、单声道、16 bit、8 个采样。lofty 能把它当成音频容器。
    fn write_pcm_wav(path: &Path) {
        let samples = [0u8; 16];
        let mut body = Vec::new();
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&8_000u32.to_le_bytes());
        body.extend_from_slice(&16_000u32.to_le_bytes());
        body.extend_from_slice(&2u16.to_le_bytes());
        body.extend_from_slice(&16u16.to_le_bytes());
        body.extend_from_slice(b"data");
        body.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        body.extend_from_slice(&samples);

        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        file.extend_from_slice(b"WAVE");
        file.extend_from_slice(&body);
        write_file(path, &file);
    }

    /// 用 lofty 把 PNG 写成 ID3v2 内嵌封面，再读回确认容器里真的有这张图。
    fn embed_png(path: &Path, png: &[u8]) {
        let mut tagged = lofty::read_from_path(path).expect("测试音频应能被 lofty 读取");
        let _ = tagged.remove(TagType::Id3v2);
        let mut tag = Tag::new(TagType::Id3v2);
        let mut cursor = png;
        let mut picture = Picture::from_reader(&mut cursor).expect("测试图片应是 PNG");
        picture.set_pic_type(PictureType::CoverFront);
        tag.push_picture(picture);
        let _ = tagged.insert_tag(tag);
        tagged
            .save_to_path(path, WriteOptions::default())
            .expect("应能把 PNG 封面写入测试音频");

        let saved = lofty::read_from_path(path).unwrap();
        let found = saved.tags().iter().any(|tag| {
            tag.pictures()
                .iter()
                .any(|picture| picture.data() == png)
        });
        assert!(found, "lofty 写入后应能读回内嵌封面");
    }

    #[test]
    fn lists_audio_files_recursively_and_skips_symlink_directories() {
        let temp = TempDir::new();
        let root = temp.path().join("downloads");
        std::fs::create_dir_all(&root).unwrap();
        write_file(&root.join("top.mp3"), b"top");
        write_file(&root.join("a").join("b.mp3"), b"b");
        write_file(&root.join("m").join("a.mp3"), b"a");
        write_file(&root.join("m").join("inner").join("deep.flac"), b"deep");
        write_file(&root.join("m").join("note.txt"), b"skip");
        write_file(&root.join("readme.txt"), b"skip");

        let outside = temp.path().join("linked-target");
        write_file(&outside.join("hidden.mp3"), b"hidden");
        symlink_dir(&outside, &root.join("link"));

        let files = list_audio_files(root.to_str().unwrap()).unwrap();
        assert_eq!(
            relative_paths(&root, &files),
            vec![
                "a/b.mp3",
                "m/a.mp3",
                "m/inner/deep.flac",
                "top.mp3",
            ]
        );
        assert_eq!(
            files.iter().map(|file| file.name.as_str()).collect::<Vec<_>>(),
            vec!["b.mp3", "a.mp3", "deep.flac", "top.mp3"]
        );
        assert!(files.iter().all(|file| !file.has_cover));
        assert!(outside.join("hidden.mp3").exists());
    }

    #[test]
    fn rejects_delete_outside_download_directory() {
        let temp = TempDir::new();
        let root = temp.path().join("downloads");
        std::fs::create_dir_all(&root).unwrap();
        let dir = root.to_str().unwrap();

        let outside = temp.path().join("secret.mp3");
        write_file(&outside, b"secret");
        let inside = root.join("keep.mp3");
        write_file(&inside, b"keep");

        let outside_result =
            delete_audio_files(dir, &[outside.to_string_lossy().into_owned()]).unwrap();
        assert_eq!(outside_result.deleted, 0);
        assert!(outside_result.failed[0].error.contains("不在下载目录内"));
        assert!(outside.exists());
        assert!(inside.exists());

        let sibling = temp.path().join("downloads-evil").join("a.mp3");
        write_file(&sibling, b"evil");
        let sibling_result =
            delete_audio_files(dir, &[sibling.to_string_lossy().into_owned()]).unwrap();
        assert_eq!(sibling_result.deleted, 0);
        assert!(sibling_result.failed[0].error.contains("不在下载目录内"));
        assert!(sibling.exists());

        let nested = root.join("album").join("keep.mp3");
        write_file(&nested, b"nested");
        let traversal = root
            .join("album")
            .join("..")
            .join("album")
            .join("keep.mp3");
        let traversal_result =
            delete_audio_files(dir, &[traversal.to_string_lossy().into_owned()]).unwrap();
        assert_eq!(traversal_result.deleted, 0);
        assert!(traversal_result.failed[0].error.contains("路径不能包含 .."));
        assert!(nested.exists());

        let relative = root.join("album").join("gone.mp3");
        write_file(&relative, b"gone");
        let deleted = delete_audio_files(dir, &["album/gone.mp3".to_string()]).unwrap();
        assert_eq!(deleted.deleted, 1);
        assert!(deleted.failed.is_empty());
        assert!(!relative.exists());
        assert!(nested.exists());

        // 写标签走同一套路径校验，目录外的文件在读音频之前就被拒绝。
        let update = MetadataUpdate {
            title: Some("不应写入".to_string()),
            ..MetadataUpdate::default()
        };
        assert!(write_audio_metadata(dir, outside.to_str().unwrap(), &update).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"secret");
    }

    #[test]
    fn rename_stays_in_original_directory() {
        let temp = TempDir::new();
        let root = temp.path().join("downloads");
        let album = root.join("album");
        let original = album.join("old.mp3");
        write_file(&original, b"audio");
        let dir = root.to_str().unwrap();

        let entry = rename_audio_file(dir, "album/old.mp3", "../new.mp3").unwrap();
        let renamed = PathBuf::from(&entry.path);
        assert_eq!(
            renamed.canonicalize().unwrap(),
            album.join("new.mp3").canonicalize().unwrap()
        );
        assert_eq!(entry.name, "new.mp3");
        assert!(!root.join("new.mp3").exists());
        assert!(!original.exists());
        assert_ne!(
            renamed.parent().unwrap().canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );

        let outside = temp.path().join("secret.mp3");
        write_file(&outside, b"secret");
        let rejected = rename_audio_file(dir, outside.to_str().unwrap(), "stolen.mp3");
        assert!(rejected.is_err());
        assert!(outside.exists());
        assert!(!root.join("stolen.mp3").exists());
        assert!(album.join("new.mp3").exists());
    }

    #[test]
    fn returns_embedded_cover_only_when_picture_present() {
        let temp = TempDir::new();
        let root = temp.path().join("downloads");
        let bare = root.join("bare.wav");
        let covered = root.join("album").join("covered.wav");
        write_pcm_wav(&bare);
        write_pcm_wav(&covered);
        let dir = root.to_str().unwrap();

        let before = list_audio_files(dir).unwrap();
        assert!(before.iter().all(|file| !file.has_cover));
        assert!(read_audio_cover(dir, bare.to_str().unwrap())
            .unwrap()
            .is_none());

        embed_png(&covered, PNG_1X1);
        let cover = read_audio_cover(dir, "album/covered.wav")
            .unwrap()
            .expect("有内嵌封面时应返回图片");
        assert_eq!(cover.mime, "image/png");
        assert_eq!(cover.data, PNG_1X1);

        let after = list_audio_files(dir).unwrap();
        let covered_entry = after.iter().find(|file| file.name == "covered.wav").unwrap();
        let bare_entry = after.iter().find(|file| file.name == "bare.wav").unwrap();
        assert!(covered_entry.has_cover);
        assert!(!bare_entry.has_cover);
        assert!(covered_entry.path.contains("album"));

        let outside = temp.path().join("outside.wav");
        write_pcm_wav(&outside);
        embed_png(&outside, PNG_1X1);
        assert!(read_audio_cover(dir, outside.to_str().unwrap()).is_err());
        assert!(outside.exists());
    }
}
