use md5::{Digest, Md5};
use once_cell::sync::Lazy;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

static MID: Lazy<String> = Lazy::new(|| super::super::get_guid().to_lowercase());

#[derive(Clone, Copy)]
struct Profile {
    appid: &'static str,
    clientver: &'static str,
    /// Android 网关签名盐。概念版和标准版不同。
    sign_salt: &'static str,
    /// /v5/url 的 key 盐，后面还要拼 appid。
    url_key_salt: &'static str,
}

const STANDARD: Profile = Profile {
    appid: "1005",
    clientver: "20489",
    sign_salt: "OIlwieks28dk2k092lksi2UIkp",
    url_key_salt: "57ae12eb6890223e355ccfcb74edf70d",
};

/// 酷狗概念版。取链必须用这套身份，标准版 appid 不认概念版会员。
const LITE: Profile = Profile {
    appid: "3116",
    clientver: "11440",
    sign_salt: "LnT6xpN3khm36zse0QzvmgTZ3waWdRSA",
    url_key_salt: "185672dd44712f60bb1736df5a377e82",
};

tokio::task_local! {
    static ACTIVE_PROFILE: Profile;
}

pub(super) fn is_lite() -> bool {
    ACTIVE_PROFILE
        .try_with(|profile| profile.appid == LITE.appid)
        .unwrap_or(false)
}

pub async fn with_lite<Fut: std::future::Future>(future: Fut) -> Fut::Output {
    ACTIVE_PROFILE.scope(LITE, future).await
}

fn active_profile() -> Profile {
    ACTIVE_PROFILE.try_with(|profile| *profile).unwrap_or(STANDARD)
}

pub(super) fn md5(value: &str) -> String {
    format!("{:x}", Md5::digest(value.as_bytes()))
}

fn signature(params: &BTreeMap<String, String>, body: &str) -> String {
    signature_with_salt(params, body, active_profile().sign_salt)
}

fn signature_with_salt(params: &BTreeMap<String, String>, body: &str, salt: &str) -> String {
    let joined: String = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
    md5(&format!("{salt}{joined}{body}{salt}"))
}

pub(super) fn number(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .unwrap_or(0)
}

pub(super) fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

pub(super) fn validate_page(page: u32, limit: u32) -> Result<(), String> {
    if page == 0 || limit == 0 || limit > 100 {
        return Err("无效的分页参数".into());
    }
    Ok(())
}

pub(super) fn validate_id(id: &str) -> Result<(), String> {
    if id.parse::<u64>().ok().filter(|id| *id > 0).is_none() {
        return Err("无效的酷狗 ID".into());
    }
    Ok(())
}

pub(super) fn check_response(value: Value) -> Result<Value, String> {
    let code = ["error_code", "errcode", "ErrorCode"]
        .iter()
        .map(|k| number(&value[k]))
        .find(|c| *c != 0)
        .unwrap_or(0);
    if value
        .get("status")
        .is_some_and(|s| number(s) != 1 && number(s) != 200)
        // 歌词服务使用 status=200 / errcode=200 表示成功。
        || (code != 0 && !(code == 200 && number(&value["status"]) == 200))
    {
        let message = ["error_msg", "errmsg", "error", "msg"]
            .iter()
            .filter_map(|k| value[k].as_str())
            .find(|s| !s.is_empty())
            .unwrap_or("平台拒绝请求");
        log_response("rejected", &value);
        return Err(format!("酷狗请求失败 ({code}): {message}"));
    }
    log_response("ok", &value);
    Ok(value)
}

fn log_response(kind: &str, value: &Value) {
    if std::env::var("HOTDOWNLOADER_KUGOU_LOG").is_err() {
        return;
    }
    let mut copy = value.clone();
    redact(&mut copy);
    let text = copy.to_string();
    let text = if text.len() > 1800 {
        format!("{}…", &text[..1800])
    } else {
        text
    };
    eprintln!("kugou {kind}: {text}");
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["token", "key", "signature", "userid", "clienttoken"] {
                if map.contains_key(key) {
                    map.insert(key.to_owned(), Value::String("<redacted>".into()));
                }
            }
            for nested in map.values_mut() {
                redact(nested);
            }
        }
        Value::Array(items) => {
            for nested in items {
                redact(nested);
            }
        }
        _ => {}
    }
}

pub(super) async fn request(
    path: &str,
    router: &str,
    params: Value,
    body: Option<Value>,
) -> Result<Value, String> {
    request_with(
        &super::super::CLIENT,
        "https://gateway.kugou.com",
        path,
        router,
        params,
        body,
    )
    .await
}

pub(super) async fn request_with(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    router: &str,
    params: Value,
    body: Option<Value>,
) -> Result<Value, String> {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs()
        .to_string();
    let mut query: BTreeMap<String, String> = [
        ("dfid", "-"),
        ("mid", MID.as_str()),
        ("uuid", "-"),
        ("appid", active_profile().appid),
        ("clientver", active_profile().clientver),
        ("clienttime", &time),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    if let Some(params) = params.as_object() {
        for (k, v) in params {
            query.insert(k.clone(), text(v));
        }
    }
    // search_lyric 使用 clearDefaultParams，仅保留其声明的参数。
    if base == "https://lyrics.kugou.com" && path == "/v1/search" {
        for key in ["dfid", "mid", "uuid", "clienttime"] {
            query.remove(key);
        }
    }
    if path == "/v5/url" {
        let profile = active_profile();
        query.insert(
            "key".into(),
            md5(&format!(
                "{}{}{}{}{}",
                query["hash"],
                profile.url_key_salt,
                profile.appid,
                query["mid"],
                query.get("userid").map(String::as_str).unwrap_or("0")
            )),
        );
    }
    let body_text = body.as_ref().map(Value::to_string).unwrap_or_default();
    let signed = if base == "https://login-user.kugou.com" {
        signature_with_salt(&query, &body_text, "NVPh5oo715z5DIWAeQlhMDsWXXQV4hwt")
    } else {
        signature(&query, &body_text)
    };
    query.insert("signature".into(), signed);
    if std::env::var("HOTDOWNLOADER_KUGOU_LOG").is_ok() {
        eprintln!(
            "kugou req: {path} appid={} clientver={}",
            query.get("appid").map(String::as_str).unwrap_or(""),
            query.get("clientver").map(String::as_str).unwrap_or("")
        );
    }
    let mut req = client
        .request(
            if body.is_some() {
                reqwest::Method::POST
            } else {
                reqwest::Method::GET
            },
            format!("{base}{path}"),
        )
        .query(&query)
        .header(
            "User-Agent",
            "Android15-1070-11083-46-0-DiscoveryDRADProtocol-wifi",
        )
        .header("dfid", query.get("dfid").map(String::as_str).unwrap_or("-"))
        .header(
            "mid",
            query.get("mid").map(String::as_str).unwrap_or(MID.as_str()),
        )
        .header("clienttime", &time)
        .header("kg-rc", "1")
        .header("kg-thash", "5d816a0")
        .header("kg-rec", "1")
        .header("kg-rf", "B9EDA08A64250DEFFBCADDEE00F8F25F")
        .header("kg-tid", "255");
    if !router.is_empty() {
        req = req.header("x-router", router);
    }
    if body.is_some() {
        req = req
            .header("Content-Type", "application/json")
            .body(body_text);
    }
    let response = req
        .send()
        .await
        .map_err(|e| format!("酷狗网络错误: {}", e.without_url()))?
        .error_for_status()
        .map_err(|e| format!("酷狗 HTTP 错误: {}", e.without_url()))?;
    check_response(
        response
            .json::<Value>()
            .await
            .map_err(|e| format!("解析酷狗响应失败: {e}"))?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn errors_are_not_empty_results() {
        assert!(check_response(json!({"status":200,"errcode":200,"errmsg":"OK"})).is_ok());
        assert!(check_response(json!({
            "status": 2,
            "errcode": 20028,
            "error": "需要验证"
        }))
        .unwrap_err()
        .contains("需要验证"));
        assert!(check_response(json!({
            "status": 1,
            "error_code": 0,
            "data": []
        }))
        .is_ok());
    }
    #[test]
    fn signature_sorts_and_includes_body() {
        let params = [("z".into(), "中文".into()), ("a".into(), "1".into())].into();
        assert_eq!(
            signature(&params, "{}"),
            md5(&format!(
                "{salt}a=1z=中文{{}}{salt}",
                salt = STANDARD.sign_salt
            ))
        );
    }
}
