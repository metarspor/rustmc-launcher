//! AI 助手：OpenAI 兼容 Chat Completions 客户端，支持 function calling。
//!
//! 任何兼容 OpenAI 协议的服务都能接（DeepSeek / 豆包 / 通义 / OpenAI / OpenRouter …），
//! 只要在设置里填对 `base_url`、`api_key`、`model`。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 一条对话消息（既用于请求，也用于本地历史）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// system / user / assistant / tool
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// assistant 消息里模型要求调用的工具。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// tool 角色消息：对应哪个 tool_call_id。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// 模型传来的参数是 JSON 字符串，原样回传给执行方。
    pub arguments: String,
}

/// 工具定义（function calling）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: FunctionSpec,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

impl ToolSpec {
    pub fn new(name: &'static str, description: &'static str, parameters: Value) -> Self {
        ToolSpec {
            kind: "function",
            function: FunctionSpec {
                name,
                description,
                parameters,
            },
        }
    }
}

/// 一次 chat completion 的响应（只取我们关心的字段）。
#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

/// 发起一次 chat completion 请求。
///
/// `messages` 是完整对话历史（含 system、user、assistant、tool）。
/// `tools` 是可用工具列表（空数组表示不启用 function calling）。
pub async fn chat(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[Message],
    tools: &[ToolSpec],
) -> Result<Message, String> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/chat/completions");

    let mut body = json!({
        "model": model,
        "messages": messages,
        "temperature": 0.3,
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::to_value(tools).map_err(|e| e.to_string())?;
        body["tool_choice"] = json!("auto");
    }

    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("HTTP 错误：{e}"))?;

    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败：{e}"))?;
    let parsed: ChatResponse =
        serde_json::from_str(&text).map_err(|e| format!("解析响应失败：{e}\n原始返回：{text}"))?;
    let msg = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| "模型没有返回任何 choices".to_string())?
        .message;
    Ok(msg)
}

/// 系统提示词：告诉 AI 它是谁、能做什么、怎么用中文回答。
pub fn system_prompt() -> &'static str {
    "你是 RustMC Launcher 内置的 AI 助手，运行在一个 Minecraft 启动器里。\n\
     用户用中文跟你对话，你也必须用中文回答，简洁友好。\n\
     \n\
     你可以通过调用工具直接帮用户完成操作：\n\
     - 查询可下载的 Minecraft 版本、已安装版本\n\
     - 下载并安装指定的 Minecraft 版本\n\
     - 安装模组加载器（Fabric / Quilt / Forge / NeoForge）\n\
     - 在 Modrinth 搜索并安装模组、资源包、光影、整合包\n\
     - 启动已安装的游戏\n\
     \n\
     规则：\n\
     1. 用户说\"帮我装个 XXX 模组\"、\"下载 1.20.1\"这种明确需求时，直接调用对应工具，不要只给建议。\n\
     2. 下载/安装是耗时操作，调用工具后耐心等待结果，不要重复触发。\n\
     3. 工具执行完后，用一两句话告诉用户结果（成功/失败、装到哪了、下一步建议）。\n\
     4. 如果缺少必要信息（比如不知道用户要哪个 MC 版本、模组叫什么），先问一句再调工具。\n\
     5. 不要编造版本号或模组名；拿不准就先搜索。"
}
