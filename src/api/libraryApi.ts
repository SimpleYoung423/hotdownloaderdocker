import { invoke, isTauri } from '@tauri-apps/api/core'
import { webHeaders, webRequest, webSession } from './webClient'

/** 下载目录中的单个音频文件；字段与 Rust `AudioFileEntry` 对应。 */
export interface AudioFileEntry {
    /** 文件名（含扩展名）。 */
    name: string
    /** 文件绝对路径。 */
    path: string
    /** 文件字节大小。 */
    size: number
    /** 小写扩展名，不含点。 */
    extension: string
    /** 是否有内嵌封面。没有封面时不要占位。 */
    hasCover: boolean
}

/** 内嵌封面的原始字节，供曲库缩略图生成 blob URL。 */
export interface LibraryCover {
    mime: string
    bytes: Uint8Array
}

export interface LibraryResponse {
    /** 实际扫描的下载目录。 */
    directory: string
    files: AudioFileEntry[]
}

export interface DeleteFailure {
    path: string
    error: string
}

export interface DeleteResult {
    deleted: number
    failed: DeleteFailure[]
}

/**
 * 列出下载目录中的音频文件（曲库数据源）。
 *
 * 独立服务由 Rust 决定下载目录；桌面端由调用方传入当前设置中的目录。
 */
export async function fetchLibrary(directory?: string): Promise<LibraryResponse> {
    if (isTauri()) {
        const target = directory ?? ''
        const files = await invoke<AudioFileEntry[]>('list_audio_files', { directory: target })
        return { directory: target, files }
    }
    return webRequest<LibraryResponse>('/api/library')
}

/** 删除下载目录中指定的音频文件。 */
export async function deleteLibraryFiles(paths: string[], directory?: string): Promise<DeleteResult> {
    if (isTauri()) {
        return invoke<DeleteResult>('delete_audio_files', { directory: directory ?? '', paths })
    }
    return webRequest<DeleteResult>('/api/library/delete', {
        method: 'POST',
        body: JSON.stringify({ paths }),
    })
}

/**
 * 读取一首歌的内嵌封面。
 *
 * 网页鉴权在 Authorization 头里，不能直接用 img src。这里带上 webHeaders 取字节。
 * 没有封面时返回 null。
 */
export async function fetchLibraryCover(path: string, directory?: string): Promise<LibraryCover | null> {
    if (isTauri()) {
        try {
            const result = await invoke<{ mime: string; data: number[] } | null>('read_audio_cover', {
                directory: directory ?? '',
                path,
            })
            if (!result || result.data.length === 0) return null
            return { mime: result.mime || 'application/octet-stream', bytes: new Uint8Array(result.data) }
        } catch {
            return null
        }
    }

    const headers = new Headers(webHeaders())
    const params = new URLSearchParams({ path })
    const response = await fetch(`/api/library/cover?${params.toString()}`, {
        headers,
        cache: 'no-store',
    })
    if (response.status === 401) {
        webSession.authorized = false
        return null
    }
    if (response.status === 404 || !response.ok) return null
    const mime = (response.headers.get('content-type') ?? 'application/octet-stream').split(';')[0].trim()
    const bytes = new Uint8Array(await response.arrayBuffer())
    if (bytes.length === 0) return null
    return { mime: mime || 'application/octet-stream', bytes }
}
