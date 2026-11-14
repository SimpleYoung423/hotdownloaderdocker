import { PLATFORMS } from '../config/platforms'
import { kugouLiteLogin, kugouLogin } from '../api/kugouLogin'
import { getLoginStatus } from '../api/musicApi'
import { neteaseLogin } from '../api/neteaseLogin'

/**
 * 查询各平台登录状态。酷我没有登录接口，不会出现在结果里。
 * 单个平台查询失败时当作未登录，避免挡住其余平台。
 */
export async function loadLoggedInPlatforms(): Promise<Set<string>> {
    const [qq, kugou, kugouLite, netease] = await Promise.all([
        getLoginStatus('qqmusic').then((status) => status.logged_in).catch(() => false),
        kugouLogin('status').then((status) => status.logged_in === true).catch(() => false),
        kugouLiteLogin('status').then((status) => status.logged_in === true).catch(() => false),
        neteaseLogin('status').then((status) => status.logged_in === true).catch(() => false),
    ])
    const loggedIn = new Set<string>()
    if (qq) loggedIn.add('qqmusic')
    if (kugou) loggedIn.add('kugou')
    if (kugouLite) loggedIn.add('kugoulite')
    if (netease) loggedIn.add('netease')
    return loggedIn
}

/** 多个平台已登录时，取平台列表里更靠前的那个。都未登录时返回空。 */
export function firstLoggedInPlatform(loggedIn: Set<string>): string | null {
    return PLATFORMS.find((platform) => loggedIn.has(platform.key))?.key ?? null
}
