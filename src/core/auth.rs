//! 账号认证：离线模式 + 微软设备码（Device Code）流程。
//!
//! 微软流程：MSA 设备码 → XBL → XSTS → Minecraft 令牌 → 所有权校验 → 玩家资料。

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use md5::{Digest as Md5Digest, Md5};
use serde::{Deserialize, Serialize};

use reqwest::Client;

/// Prism Launcher 公开注册的 Azure 应用 ID（开源社区通用）。
const MSA_CLIENT_ID: &str = "c36a9fb6-4f2a-41ff-90bd-ae7cc92031eb";

const DEVICE_CODE_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const MSA_TOKEN_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str =
    "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_ENTITLEMENT_URL: &str =
    "https://api.minecraftservices.com/entitlements/mcstore";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

/* ------------------------------ 账号 ------------------------------ */

/// 账号类型：离线、微软正版、第三方（自定义离线 / 第三方服务器账号）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Offline,
    Microsoft,
    ThirdParty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    /// 无连字符的 32 位 UUID
    pub uuid: String,
    pub access_token: String,
    pub kind: AccountKind,
    /// 备注（第三方账号可填服务器地址等说明），离线/微软可为空。
    #[serde(default)]
    pub note: String,
}

impl Account {
    /// 创建离线账号（UUID 与官方启动器离线算法一致：MD5 版本 3）。
    pub fn offline(name: &str) -> Self {
        Self {
            name: name.to_string(),
            uuid: offline_uuid(name),
            access_token: "0".to_string(),
            kind: AccountKind::Offline,
            note: String::new(),
        }
    }

    /// 创建第三方账号：可指定 UUID 与令牌；留空时按离线规则派生 UUID。
    pub fn third_party(name: &str, uuid: &str, token: &str, note: &str) -> Self {
        let uuid = uuid.trim();
        let uuid = if uuid.is_empty() {
            offline_uuid(name)
        } else {
            uuid.replace('-', "").to_string()
        };
        Self {
            name: name.trim().to_string(),
            uuid,
            access_token: if token.trim().is_empty() {
                "0".to_string()
            } else {
                token.trim().to_string()
            },
            kind: AccountKind::ThirdParty,
            note: note.trim().to_string(),
        }
    }

    fn microsoft(name: String, uuid: String, access_token: String) -> Self {
        Self {
            name,
            uuid,
            access_token,
            kind: AccountKind::Microsoft,
            note: String::new(),
        }
    }

    /// 账号类型中文标签。
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            AccountKind::Offline => "离线",
            AccountKind::Microsoft => "微软",
            AccountKind::ThirdParty => "第三方",
        }
    }

    /// 对应游戏参数 `--userType`
    pub fn user_type(&self) -> &'static str {
        match self.kind {
            AccountKind::Offline | AccountKind::ThirdParty => "legacy",
            AccountKind::Microsoft => "msa",
        }
    }
}

/// 离线 UUID：`UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes())`
fn offline_uuid(name: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(b"OfflinePlayer:");
    hasher.update(name.as_bytes());
    let mut bytes: [u8; 16] = hasher.finalize().into();

    bytes[6] = (bytes[6] & 0x0f) | 0x30; // 版本 3（基于名称）
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // IETF 变体

    hex::encode(bytes)
}

/* ---------------------------- 设备码流程 ---------------------------- */

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: Option<u64>,
    #[allow(dead_code)]
    pub message: Option<String>,
}

/// 第一步：请求设备码。
pub async fn request_device_code(client: &Client) -> Result<DeviceCode> {
    let params = [
        ("client_id", MSA_CLIENT_ID),
        ("scope", "XboxLive.SignIn XboxLive.offline_access"),
    ];
    let resp = client
        .post(DEVICE_CODE_URL)
        .form(&params)
        .send()
        .await
        .context("请求设备码失败（检查网络）")?
        .error_for_status()?;
    let code: DeviceCode = resp.json().await.context("解析设备码失败")?;
    Ok(code)
}

/// 第二步起：轮询 MSA 令牌并完成整条登录链。
///
/// `on_status` 用于向界面汇报当前阶段。
pub async fn complete_device_login(
    client: &Client,
    device: DeviceCode,
    on_status: impl Fn(&str),
) -> Result<Account> {
    on_status("等待浏览器授权，请在网页输入代码…");
    let msa_token = poll_msa_token(client, &device).await?;

    on_status("Xbox Live 认证中…");
    let (uhs, xbl_token) = xbl_authenticate(client, &msa_token).await?;

    on_status("XSTS 安全授权中…");
    let xsts_token = xsts_authorize(client, &xbl_token).await?;

    on_status("获取 Minecraft 访问令牌…");
    let mc_token = minecraft_login(client, &uhs, &xsts_token).await?;

    on_status("验证游戏所有权…");
    if !check_entitlement(client, &mc_token).await? {
        bail!("该微软账号未拥有 Minecraft（Java 版），无法登录");
    }

    on_status("读取玩家资料…");
    let (uuid, name) = fetch_profile(client, &mc_token).await?;

    Ok(Account::microsoft(name, uuid, mc_token))
}

async fn poll_msa_token(client: &Client, device: &DeviceCode) -> Result<String> {
    let mut interval = Duration::from_secs(device.interval.unwrap_or(5));
    let deadline = Instant::now() + Duration::from_secs(device.expires_in);

    loop {
        tokio::time::sleep(interval).await;

        let params = [
            ("client_id", MSA_CLIENT_ID),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device.device_code.as_str()),
        ];
        let resp = client.post(MSA_TOKEN_URL).form(&params).send().await?;
        let data: TokenResponse = resp.json().await?;

        if let Some(token) = data.access_token {
            return Ok(token);
        }
        match data.error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += Duration::from_secs(5),
            Some(other) => bail!(
                "微软登录失败：{}",
                data.error_description.unwrap_or_else(|| other.to_string())
            ),
            None => bail!("微软令牌响应异常"),
        }

        if Instant::now() >= deadline {
            bail!("设备码已过期，请重新发起登录");
        }
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    #[allow(dead_code)]
    refresh_token: Option<String>,
    #[allow(dead_code)]
    expires_in: Option<u64>,
    error: Option<String>,
    error_description: Option<String>,
}

/* ------------------------------ XBL ------------------------------ */

#[derive(Debug, Deserialize)]
struct XboxResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: DisplayClaims,
}

#[derive(Debug, Deserialize)]
struct DisplayClaims {
    xui: Vec<Xui>,
}

#[derive(Debug, Deserialize)]
struct Xui {
    uhs: String,
}

async fn xbl_authenticate(
    client: &Client,
    msa_token: &str,
) -> Result<(String, String)> {
    let body = serde_json::json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("d={msa_token}")
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT"
    });

    let resp = client
        .post(XBL_URL)
        .header("x-xbl-contract-version", "1")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;

    if status.is_success() {
        let parsed: XboxResponse =
            serde_json::from_str(&text).context("解析 XBL 响应失败")?;
        let uhs = parsed
            .display_claims
            .xui
            .first()
            .context("XBL 响应缺少用户哈希（uhs）")?
            .uhs
            .clone();
        Ok((uhs, parsed.token))
    } else {
        bail!(
            "Xbox Live 认证失败（HTTP {}）：{text}",
            status.as_u16()
        )
    }
}

async fn xsts_authorize(client: &Client, xbl_token: &str) -> Result<String> {
    let body = serde_json::json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [xbl_token]
        },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT"
    });

    let resp = client
        .post(XSTS_URL)
        .header("x-xbl-contract-version", "1")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;

    if status.is_success() {
        let parsed: XboxResponse =
            serde_json::from_str(&text).context("解析 XSTS 响应失败")?;
        Ok(parsed.token)
    } else {
        #[derive(Deserialize)]
        struct XstsErr {
            #[serde(rename = "XErr")]
            x_err: Option<i64>,
        }
        let err: XstsErr =
            serde_json::from_str(&text).unwrap_or(XstsErr { x_err: None });
        bail!("XSTS 授权失败：{}", xerr_message(err.x_err))
    }
}

fn xerr_message(code: Option<i64>) -> String {
    match code {
        Some(2148916233) => "账号没有 Xbox 档案，请先在 xbox.com 完成注册".to_string(),
        Some(2148916235) => "当前所在地区不支持 Xbox Live".to_string(),
        Some(2148916236) | Some(2148916237) => "需要成人身份验证".to_string(),
        Some(2148916238) => {
            "该账号为未成年账号，需要被加入成人的 Xbox 家庭组".to_string()
        }
        Some(c) => format!("未知错误（XErr {c}）"),
        None => "未知错误".to_string(),
    }
}

/* --------------------------- Minecraft --------------------------- */

async fn minecraft_login(
    client: &Client,
    uhs: &str,
    xsts_token: &str,
) -> Result<String> {
    let body = serde_json::json!({
        "identityToken": format!("XBL3.0 x={uhs};{xsts_token}")
    });
    let resp = client
        .post(MC_LOGIN_URL)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    #[derive(Deserialize)]
    struct LoginResp {
        access_token: String,
    }
    let parsed: LoginResp = resp.json().await?;
    Ok(parsed.access_token)
}

async fn check_entitlement(client: &Client, mc_token: &str) -> Result<bool> {
    let resp = client
        .get(MC_ENTITLEMENT_URL)
        .bearer_auth(mc_token)
        .send()
        .await?
        .error_for_status()?;
    #[derive(Deserialize)]
    struct Entitlements {
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        name: String,
    }
    let data: Entitlements = resp.json().await?;
    Ok(data
        .items
        .iter()
        .any(|i| i.name == "game_minecraft" || i.name == "product_minecraft"))
}

async fn fetch_profile(client: &Client, mc_token: &str) -> Result<(String, String)> {
    let resp = client
        .get(MC_PROFILE_URL)
        .bearer_auth(mc_token)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("获取玩家资料失败（尚未创建游戏档案或档案被限制）");
    }
    #[derive(Deserialize)]
    struct Profile {
        id: String,
        name: String,
    }
    let p: Profile = resp.json().await?;
    Ok((p.id, p.name))
}
