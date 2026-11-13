use crate::download::context::SongInfo;

/// 过滤各平台常见的非法文件名字符，保持已有文件命名规则。
/// 只处理单个路径段；模板里手写的 `/` 在分段时已经拆开。
pub fn sanitize_name(raw: &str) -> String {
    raw.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_")
}

/// 将命名模板中的歌曲、歌手、专辑和音质变量替换为实际值。
pub fn apply_template(
    template: &str,
    artist: &str,
    title: &str,
    album: &str,
    quality: &str,
) -> String {
    template
        .replace("{song}", title)
        .replace("{artist}", artist)
        .replace("{album}", album)
        .replace("{quality}", quality)
}

/// 空段、`.`、`..` 不能当目录或文件名，避免模板穿越到下载目录之外。
fn usable_segment(sanitized: &str) -> bool {
    let trimmed = sanitized.trim();
    !trimmed.is_empty() && trimmed != "." && trimmed != ".."
}

/// 模板没有可用路径段时，沿用原来的「歌名 - 歌手」；两边都没有文字则用稳定名称。
fn fallback_filename(info: &SongInfo) -> String {
    let fallback_sanitized = sanitize_name(&apply_template(
        "{song} - {artist}",
        &info.artist,
        &info.title,
        &info.album,
        &info.quality,
    ));
    let no_song_text = sanitize_name(&info.title).trim().is_empty()
        && sanitize_name(&info.artist).trim().is_empty();
    if fallback_sanitized.trim().is_empty() || no_song_text {
        "未知歌曲".to_string()
    } else {
        fallback_sanitized
    }
}

/// 生成不含扩展名的相对路径。模板中的 `/` 是目录，变量值里的 `/` 仍换成 `_`。
pub fn build_filename(template: &str, info: &SongInfo) -> String {
    let mut segments = Vec::new();
    for raw_segment in template.split('/') {
        let rendered = apply_template(
            raw_segment,
            &info.artist,
            &info.title,
            &info.album,
            &info.quality,
        );
        let sanitized = sanitize_name(&rendered);
        if usable_segment(&sanitized) {
            segments.push(sanitized);
        }
    }

    if segments.is_empty() {
        fallback_filename(info)
    } else {
        segments.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::build_filename;
    use crate::download::context::SongInfo;

    fn song(title: &str, artist: &str, album: &str, quality: &str) -> SongInfo {
        SongInfo {
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            quality: quality.into(),
            cover_url: String::new(),
        }
    }

    #[test]
    fn template_slash_builds_relative_path() {
        let info = song("千里之外", "周杰伦 & 费玉清", "依然范特西", "flac");
        assert_eq!(
            build_filename("{album}/{artist}-{song}", &info),
            "依然范特西/周杰伦 & 费玉清-千里之外"
        );
        // 连续斜杠和首尾斜杠只是空段，不产生空目录。
        assert_eq!(
            build_filename("/{album}//{artist}-{song}/", &info),
            "依然范特西/周杰伦 & 费玉清-千里之外"
        );
    }

    #[test]
    fn slash_inside_song_fields_becomes_underscore() {
        let info = song("a/b", "Singer/Name", "Al/bum", "fl/ac");
        assert_eq!(build_filename("{song} - {artist}", &info), "a_b - Singer_Name");
        assert_eq!(
            build_filename("{album}/{artist}-{song}", &info),
            "Al_bum/Singer_Name-a_b"
        );
        assert_eq!(
            build_filename("{quality}/{song}", &info),
            "fl_ac/a_b"
        );
    }

    #[test]
    fn dot_segments_are_dropped() {
        let info = song("千里之外", "周杰伦", "..", "flac");
        assert_eq!(build_filename("../{song}", &info), "千里之外");
        assert_eq!(build_filename("{album}/../{song}", &info), "千里之外");
        assert_eq!(
            build_filename("foo/../../bar/{song}", &info),
            "foo/bar/千里之外"
        );
        assert_eq!(build_filename("./{song}/.", &info), "千里之外");
    }

    #[test]
    fn all_invalid_segments_fall_back() {
        let info = song("千里之外", "周杰伦", "依然范特西", "flac");
        assert_eq!(build_filename("..", &info), "千里之外 - 周杰伦");
        assert_eq!(build_filename("/./../", &info), "千里之外 - 周杰伦");

        let blank = song("  ", "\t", "..", ".");
        assert_eq!(build_filename("///", &blank), "未知歌曲");
        assert_eq!(build_filename("{album}/{quality}", &blank), "未知歌曲");
    }
}
