<template>
    <n-form-item label="示例">
        <div class="preview-container">
            <div class="preview-line">
                <span class="preview-label">歌手：</span>
                <code class="preview-value">{{ exampleArtists }}</code>
            </div>
            <div class="preview-line">
                <span class="preview-label">文件名：</span>
                <code class="preview-value">{{ exampleFilename }}.flac</code>
            </div>
        </div>
    </n-form-item>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { NFormItem } from 'naive-ui'
import { useSettingsStore } from '../../stores/settingsStore'

const settingsStore = useSettingsStore()

// 用于预览的示例歌曲信息。artist 字段在示例中按用户配置的歌手分隔符拼接，
// 这样两个设置的修改可以即时在同一个示例里看到效果。
const exampleSong = {
    song: '千里之外',
    artistList: ['周杰伦', '费玉清'],
    album: '依然范特西',
    quality: 'flac',
}

// 过滤非法字符的函数（与后端 sanitize_name 一致）
const sanitize = (raw: string) => raw.replace(/[\\/:*?"<>|]/g, '_')

// 当前配置下的多歌手拼接结果（空值时回退为中文顿号）
const exampleArtists = computed(() => {
    const sep = settingsStore.settings.artistSeparator || '、'
    return exampleSong.artistList.join(sep)
})

// 与后端 apply_template 相同，只替换当前这一段。
const applySegment = (segment: string, artist: string) =>
    segment
        .replaceAll('{song}', exampleSong.song)
        .replaceAll('{artist}', artist)
        .replaceAll('{album}', exampleSong.album)
        .replaceAll('{quality}', exampleSong.quality)

// 空段、`.`、`..` 丢掉，避免预览出目录穿越。
const usableSegment = (sanitized: string) => {
    const trimmed = sanitized.trim()
    return trimmed !== '' && trimmed !== '.' && trimmed !== '..'
}

// 模板没有可用段时，沿用「歌名 - 歌手」；两边都没有文字则用稳定名称。
const fallbackFilename = (artist: string) => {
    const fallback = sanitize(applySegment('{song} - {artist}', artist))
    const noSongText = sanitize(exampleSong.song).trim() === '' && sanitize(artist).trim() === ''
    if (fallback.trim() === '' || noSongText) {
        return '未知歌曲'
    }
    return fallback
}

// 先按模板里手写的 / 分段，再对每段做变量替换和非法字符过滤。
// 歌名、歌手、专辑、音质以及歌手分隔符里的 / 仍会变成 _。
const exampleFilename = computed(() => {
    const artist = exampleArtists.value
    const template = settingsStore.settings.namingTemplate || '{song} - {artist}'
    const segments = template
        .split('/')
        .map((segment) => sanitize(applySegment(segment, artist)))
        .filter((segment) => usableSegment(segment))
    if (segments.length === 0) {
        return fallbackFilename(artist)
    }
    return segments.join('/')
})
</script>

<style scoped>

.preview-container {
    min-width: 0;
    width: 100%;
    display: flex;
    flex-direction: column;
    gap: 4px;
}

.preview-line {
    display: block;
    /* 确保每行占满 */
    width: 100%;
    font-size: 12px;
    color: var(--color-text-secondary);
    line-height: 1.8;
    word-break: break-all;
}

.preview-label {
    color: var(--color-text-secondary);
    margin-right: 4px;
}

.preview-value {
    color: var(--color-text);
    font-family: monospace;
    font-size: 12px;
    background: rgba(127, 127, 127, 0.15);
    padding: 1px 4px;
    border-radius: 3px;
}
</style>
