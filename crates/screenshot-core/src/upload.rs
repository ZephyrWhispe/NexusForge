//! 上传 provider 轨（D-29 B4 T-B4-9，§9.0 截图-⑦ / §9.1-⑩）
//!
//! 这一层只管三件事，而且只在这一点上管：trait 形状（B6 的 WebDAV 驱动注册即得，
//! 见 `id`/`label`/`upload` 三方法与 [`UploadRegistry`] 的注册口）、HTTP 表单这一档
//! 实现、以及"什么样的端点才配收到凭据"的裁决。
//!
//! **凭据模型（§9.1-⑩ 红线）**：配置里只有*请求头名*（`upload_header_name`），
//! 永远没有值。值是逐调用参数（`header_value: Option<&str>`），沿调用栈走一圈就结束。
//! 它一旦进 `ScreenshotConfig` 就等于进 screenshot.json、进设置中心的往返、进配置备份，
//! 而"备份文件里躺着一个图床 token"是这类集成最常见的泄漏形态。
//!
//! 另一个刻意的不便利：`upload()` 在没有凭据的档上不自动降级成匿名请求。
//! 配了请求头名却没给值 → 直接拒（`SCREENSHOT_UPLOAD_003`），因为"悄悄发一个不带
//! 鉴权的请求"最坏的走法是拿到一个 200 的空目录页并把它的 URL 当成用户的直链返回。

use std::sync::Arc;

use async_trait::async_trait;
use host_core::error::AppError;
use host_core::util::b64_encode;
use parking_lot::RwLock;

use crate::types::ScreenshotConfig;

/// 端点不合规（含 scheme/主机/形态三类）：写侧与传侧共用同一码，用户看到的
/// 就是"这个地址不行"，而不是两套说法
const CODE_ENDPOINT: &str = "SCREENSHOT_UPLOAD_001";
/// 网络与响应侧失败（连接、超时、非 2xx、响应体给不出直链）
const CODE_TRANSPORT: &str = "SCREENSHOT_UPLOAD_002";
/// 这次调用根本不该发生：目标未启用、要求凭据却没给、凭据无处可放
const CODE_STATE: &str = "SCREENSHOT_UPLOAD_003";

/// 内置 provider 一档的 id（`upload_target` 的取值之一；另一档 `"webdav"` 只在
/// 宿主桥注入后经 [`WebDavUploadProvider::from_config`] 注册——见 T-B6-12）
pub const HTTP_FORM_ID: &str = "http-form";
pub const HTTP_FORM_LABEL: &str = "HTTP 表单（urlencoded + Base64）";
/// WebDAV 档（T-B6-12，B4 挂账清偿）：提交经 src-tauri 宿主桥注入的发送闭包走
/// file-core 的唯一装配/提交腿——本 crate 对 WebDAV 协议零知识（DESIGN O1）。
pub const WEBDAV_ID: &str = "webdav";
pub const WEBDAV_LABEL: &str = "WebDAV（PUT 到端点目录）";

/// 一次上传的硬上限：表单图不会比 4K 全屏 PNG 更大多少，给 60s 是留给自建端点的落盘
const UPLOAD_TIMEOUT_SECS: u64 = 60;

/// 本档表单字段名的默认值（`upload_field` 留空即走它——绝大多数自建接收端就认 `file`）
pub const DEFAULT_FORM_FIELD: &str = "file";

fn err(code: &str, msg: impl std::fmt::Display) -> AppError {
    AppError::module(code, msg.to_string(), None)
}

/// 待上传的一件产物：字节 + 它在服务端的名字 + 它来自哪一次完成。
///
/// `source` 只进失败事件（`screenshot.upload_failed`）而不进任何响应体——面板要能
/// 点名"哪一行没传上去"，而这一行可能就是用户正准备复制的那张图。
#[derive(Debug, Clone)]
pub struct UploadTicket {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub source: String,
}

/// 上传目标契约（§9.0 截图-⑦ 蓝本 `IUploaderProvider` 的本仓形状）。
///
/// 形状照 [`host_core::storage::StorageDriver`]：`id` 是 `&'static str`（注册表按它查，
/// 不允许运行期变脸）、`label` 是 UI 文案、其余能力靠默认实现兜住，好让 B6 的
/// WebDAV 驱动只写它真正不同的那部分。
#[async_trait]
pub trait UploadProvider: Send + Sync {
    /// 唯一标识："http-form" | 后续 "webdav" | ...
    fn id(&self) -> &'static str;
    /// UI 显示名
    fn label(&self) -> String;
    /// 展示用端点（已去 query/fragment；凭据一律不在 URL 里，见
    /// [`validate_upload_endpoint`] 的 userinfo 臂）。无端点概念的目标返回空串
    fn endpoint_display(&self) -> String {
        String::new()
    }
    /// 把 `bytes` 以 `filename` 的名义交出去，换回一条可直链访问的 URL。
    ///
    /// `header_value` 是**这一次调用**的凭据：trait 故意把它放在参数而不是字段上，
    /// 因为注册表里的 `Arc<dyn UploadProvider>` 跨调用长存，凭据放进实例就等于活到
    /// 下一次上传、并可能被任何持有注册表读口的代码看见。
    async fn upload(
        &self,
        bytes: &[u8],
        filename: &str,
        header_value: Option<&str>,
    ) -> Result<String, AppError>;
}

/// 端点裁决（红线：明文 http 到公网一律拒——凭据与图片不得裸奔）。
///
/// 这一份是**权威判据**：设置写侧（`apply_config`）与发送前（[`HttpFormProvider::upload`]）
/// 各调一次不是重复实现，而是因为手改 JSON 那条路只经过后者。
pub fn validate_upload_endpoint(url: &str) -> Result<(), AppError> {
    let reject = |why: &str| -> AppError {
        let hint = "仅允许 https，或明说本机的 http://127.0.0.1 · http://localhost · http://[::1]";
        AppError::module(CODE_ENDPOINT, format!("上传端点不合规：{why}"), Some(hint))
    };
    if url.trim().is_empty() {
        return Err(reject("地址为空"));
    }
    // 换行/制表等任何空白都不该出现在 URL 里；放过去就是给请求走私留口子
    if url.chars().any(|c| c.is_whitespace() || c == '\0') {
        return Err(reject("含空白或控制字符"));
    }
    let not_url = "不是绝对地址（缺 scheme://）";
    let (scheme, rest) = url.split_once("://").ok_or_else(|| reject(not_url))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err(reject(&format!("只支持 https/http，收到 {scheme}://")));
    }
    // authority 到第一个 / ? # 为止
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return Err(reject("没有主机名"));
    }
    if authority.rfind('@').is_some() {
        return Err(reject(
            "内嵌用户名/密码（凭据请填请求头名，值在每次调用时给）",
        ));
    }
    // IPv6 字面量带方括号，端口在其后；其余按第一个冒号切端口
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed
            .split_once(']')
            .map(|(h, _)| h)
            .ok_or_else(|| reject("IPv6 地址缺右括号"))?
    } else {
        authority
            .split_once(':')
            .map(|(h, _)| h)
            .unwrap_or(authority)
    };
    if host.is_empty() {
        return Err(reject("没有主机名"));
    }
    if scheme == "https" {
        return Ok(());
    }
    let loopback = matches!(
        host.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    );
    if !loopback {
        return Err(reject(&format!("明文 http 指向非本机主机 {host}")));
    }
    Ok(())
}

/// 展示用端点：去掉 query 与 fragment。
///
/// 自建接收端很常把 token 挂在 `?key=` 上，而目标列表是要渲染进设置面板的——
/// 少切这一刀就等于把凭据抄在屏幕上（用户截图提问时顺手就发出去了）。
pub fn endpoint_for_display(url: &str) -> String {
    let cut = url.find(['?', '#']).unwrap_or(url.len());
    url[..cut].to_owned()
}

/// RFC 3986 unreserved 集合之外一律 `%XX`。
///
/// 空格编成 `%20` 而不是 form-encoding 的 `+`：两端都解得开，且不会出现
/// "值里本来就带 + 号，被对面解成空格"这一类只有事后对账才发现的歧义。
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `application/x-www-form-urlencoded` 的表单正文（纯函数，注入面的唯一裁决点）。
///
/// 三格全部百分号编码，因为这三格没有一个是可信来源：字段名来自用户填的配置，
/// 文件名来自磁盘路径，字节来自屏幕。少了任何一格的编码，用户截图里的一段
/// `&admin=1` 就会变成第二个表单字段——这就是"表单参数注入"在本仓的具体形状。
/// Base64 自身也非 form-safe（`+` `/` `=`），所以它同样是必编的。
pub fn build_form_body(field: &str, filename: &str, bytes: &[u8]) -> String {
    format!(
        "{}={}&filename={}",
        percent_encode(field),
        percent_encode(&b64_encode(bytes)),
        percent_encode(filename)
    )
}

/// 响应体首行 = 服务端交回的直链或对象 id（自建接收端最省事的返回形态）
pub fn response_first_line(body: &str) -> String {
    body.lines().next().unwrap_or_default().trim().to_owned()
}

/// 把直链模板落到这一次上传上。
///
/// `{id}` ← 文件名去扩展名（自建端点按我们上传的 `filename` 落盘，所以这一格是
/// 本地可推的，不需要信响应体）；`{url}` ← 响应首行。模板为空 = 直接用响应首行。
pub fn apply_link_template(template: Option<&str>, filename: &str, response: &str) -> String {
    let url = response_first_line(response);
    let Some(raw) = template.map(str::trim) else {
        return url;
    };
    if raw.is_empty() {
        return url;
    }
    // 只取 basename：`filename` 可能带着保存目录进来，而 `{id}` 要的是对象名不是路径
    let base = filename
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(filename);
    let id = base.rsplit_once('.').map(|(a, _)| a).unwrap_or(base);
    raw.replace("{id}", id).replace("{url}", &url)
}

/// 唯一一档内置实现：urlencoded 表单 + Base64 字段。
///
/// **不开 reqwest 的 multipart feature**（§9.1-⑩ 裁定：表单档够用，feature 变更要
/// 显式登记而不是顺手）。因此这里没有 boundary、没有 part 头，只有一个正文。
#[derive(Debug, Clone)]
pub struct HttpFormProvider {
    endpoint: String,
    field: String,
    header_name: Option<String>,
    link_template: Option<String>,
}

impl HttpFormProvider {
    /// 从配置构造（端点为空 → `None`：没填地址就不该有一档"看得见但传不了"的目标）
    pub fn from_config(cfg: &ScreenshotConfig) -> Option<Self> {
        if cfg.upload_endpoint.trim().is_empty() {
            return None;
        }
        Some(Self {
            endpoint: cfg.upload_endpoint.clone(),
            field: if cfg.upload_field.trim().is_empty() {
                DEFAULT_FORM_FIELD.to_owned()
            } else {
                cfg.upload_field.clone()
            },
            header_name: trim_option(&cfg.upload_header_name),
            link_template: trim_option(&cfg.upload_link_template),
        })
    }

    /// 全进程共用一个连接池（`Client` 内部是 Arc，clone 只拷句柄）：一次截图传一张图，
    /// 每次都新建客户端等于把 TLS 握手重新付一遍
    fn client() -> reqwest::Client {
        static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        CLIENT
            .get_or_init(|| {
                reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(UPLOAD_TIMEOUT_SECS))
                    .build()
                    // 兜底的无超时装不上任何请求参数，但真装不上时"能发出去"仍然好过
                    // 这条通路整个不可用——超时是体验，不可用是功能缺失
                    .unwrap_or_default()
            })
            .clone()
    }
}

fn trim_option(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

#[async_trait]
impl UploadProvider for HttpFormProvider {
    fn id(&self) -> &'static str {
        HTTP_FORM_ID
    }

    fn label(&self) -> String {
        HTTP_FORM_LABEL.into()
    }

    fn endpoint_display(&self) -> String {
        endpoint_for_display(&self.endpoint)
    }

    async fn upload(
        &self,
        bytes: &[u8],
        filename: &str,
        header_value: Option<&str>,
    ) -> Result<String, AppError> {
        // 发送前再裁一次：配置可能没经过 apply_config（手改 JSON 后直接跑起来）
        validate_upload_endpoint(&self.endpoint)?;
        let body = build_form_body(&self.field, filename, bytes);
        let mut req = HttpFormProvider::client()
            .post(&self.endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body);
        match (self.header_name.as_deref(), header_value) {
            (Some(name), Some(value)) => {
                // 名字与值的形态都由 `http` crate 校验：带 CRLF 的名字进不了 HeaderMap，
                // 带控制字符的值同理——头部注入在这里没有可乘之机，不是靠我们自己正则
                req = req.header(name, value);
            }
            (Some(_), None) => {
                return Err(err(
                    CODE_STATE,
                    "该上传目标要求凭据（已配置请求头），本次没有提供",
                ));
            }
            (None, Some(_)) => {
                return Err(err(
                    CODE_STATE,
                    "填了凭据却没配请求头名，值无处可放（设置里的「凭据请求头名」）",
                ));
            }
            (None, None) => {}
        }
        let resp = req
            .send()
            .await
            .map_err(|e| err(CODE_TRANSPORT, format!("请求失败: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| err(CODE_TRANSPORT, format!("响应读取失败: {e}")))?;
        if !status.is_success() {
            // 只带回首行且截断：自建端点的错误页可能整片 HTML，塞进错误消息会淹掉真因
            let snippet: String = response_first_line(&text).chars().take(200).collect();
            return Err(err(
                CODE_TRANSPORT,
                if snippet.is_empty() {
                    format!("服务端返回 {status}")
                } else {
                    format!("服务端返回 {status}：{snippet}")
                },
            ));
        }
        let link = apply_link_template(self.link_template.as_deref(), filename, &text);
        if link.is_empty() {
            return Err(err(
                CODE_TRANSPORT,
                "服务端 2xx 但响应体给不出直链（空响应，或模板占位符没对上）",
            ));
        }
        Ok(link)
    }
}

// ---------------------------------------------------------------------------
// WebDAV 档（T-B6-12，B4 挂账清偿）：截图 → 一次 PUT → 直链。
//
// 跨模块纪律：file-core 与 screenshot-core 互不依赖（DESIGN O1），提交经
// src-tauri 宿主桥注入的闭包完成——本 crate 只交出「端点目录 + 文件名 +
// 字节 + 逐次凭据」四样纯数据（[`WebDavPutRequest`]），URL 拼接、
// Overwrite/Authorization 头的形状全在 file-core 的装配口（协议零知识）。
// **缺注入则这一档根本不存在**（[`WebDavUploadProvider::from_config`] 返回
// None 并 warn）——注册一枚必失败的目标就是假就绪。
// ---------------------------------------------------------------------------

/// 交给宿主桥的一次提交（纯数据；凭据是**这次调用**的值，与 trait 的
/// 逐传参模型同谱，不经过任何长期存活的字段）
#[derive(Debug, Clone)]
pub struct WebDavPutRequest {
    /// 配置里的端点目录（已过 [`validate_upload_endpoint`]）
    pub endpoint_base: String,
    pub filename: String,
    pub bytes: Vec<u8>,
    /// 本次调用的完整凭据值（如 `Basic …`）；None = 匿名 PUT
    pub header_value: Option<String>,
}

/// 桥闭包的返回形状：直链 URL 或点名原因（字符串是因为跨 FFI 边界只送得动话）
pub type WebDavSendFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;
/// 宿主桥注入的发送闭包类型（`ScreenshotModule::set_upload_sender` 的实参）
pub type WebDavSend = Arc<dyn Fn(WebDavPutRequest) -> WebDavSendFuture + Send + Sync>;

/// URL 的起源（scheme + authority，小写；比较用的最小形状，展示另有 [`endpoint_for_display`]）
fn origin_of(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some((
        scheme.to_ascii_lowercase(),
        rest[..end].to_ascii_lowercase(),
    ))
}

/// WebDAV 上传档（见本节头注释）。
#[derive(Clone)]
pub struct WebDavUploadProvider {
    endpoint: String,
    send: WebDavSend,
}

impl WebDavUploadProvider {
    /// 桥在场才成档：`sender = None` ⇒ 不注册（warn 点名，禁注册一枚必失败的）
    pub fn from_config(cfg: &ScreenshotConfig, sender: Option<&WebDavSend>) -> Option<Self> {
        if cfg.upload_endpoint.trim().is_empty() {
            return None;
        }
        let Some(send) = sender else {
            tracing::warn!("WebDAV 上传档未注册：宿主桥未注入（set_upload_sender 缺装配）");
            return None;
        };
        Some(Self {
            endpoint: cfg.upload_endpoint.clone(),
            send: send.clone(),
        })
    }
}

#[async_trait]
impl UploadProvider for WebDavUploadProvider {
    fn id(&self) -> &'static str {
        WEBDAV_ID
    }
    fn label(&self) -> String {
        WEBDAV_LABEL.into()
    }
    fn endpoint_display(&self) -> String {
        endpoint_for_display(&self.endpoint)
    }
    async fn upload(
        &self,
        bytes: &[u8],
        filename: &str,
        header_value: Option<&str>,
    ) -> Result<String, AppError> {
        // 发送前再裁：手改 JSON 那条路只经过这里（权威闸共读点 +1：写侧 /
        // http-form 发前 / 本发前 / registry 注册前）
        validate_upload_endpoint(&self.endpoint)?;
        let link = (self.send)(WebDavPutRequest {
            endpoint_base: self.endpoint.clone(),
            filename: filename.to_owned(),
            bytes: bytes.to_vec(),
            header_value: header_value.map(str::to_owned),
        })
        .await
        .map_err(|m| err(CODE_TRANSPORT, m))?;
        // 重定向诚实：桥给回的直链必须与配置端点同源——跨源要么是劫持要么是
        // 错配，两者都不配被当成"用户的图床链接"回显（同站不同路径放行，正对照）。
        let link = link.trim().to_owned();
        if link.is_empty() {
            return Err(err(CODE_TRANSPORT, "WebDAV 桥未给回有效直链"));
        }
        if origin_of(&link) != origin_of(&self.endpoint) {
            return Err(err(
                CODE_TRANSPORT,
                format!(
                    "返回链接与端点不同源，拒绝回显：{}",
                    endpoint_for_display(&link)
                ),
            ));
        }
        Ok(link)
    }
}

/// 一个已注册目标的对外面貌（`screenshot_upload_targets` 出口）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct UploadTargetInfo {
    pub id: String,
    pub label: String,
    pub endpoint_display: String,
    pub enabled: bool,
}

/// 目标注册表：档位在构造时定（配置变了就整表重建），启用哪一档是运行期可翻的一格。
///
/// 重建而不是就地改字段，是为了让"正在 await 的一次上传"不受配置变更影响——
/// 它持有的 `Arc<dyn UploadProvider>` 是发起那一刻的那一档，参数不会被换掉一半。
pub struct UploadRegistry {
    providers: Vec<Arc<dyn UploadProvider>>,
    enabled: RwLock<Option<String>>,
}

impl UploadRegistry {
    pub fn new(providers: Vec<Arc<dyn UploadProvider>>) -> Self {
        Self {
            providers,
            enabled: RwLock::new(None),
        }
    }

    /// 没有任何一档（端点未填）：面板据此说"未注册目标"，而不是给一个空下拉
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    pub fn targets(&self) -> Vec<UploadTargetInfo> {
        let enabled = self.enabled.read().clone();
        self.providers
            .iter()
            .map(|p| UploadTargetInfo {
                id: p.id().to_owned(),
                label: p.label(),
                endpoint_display: p.endpoint_display(),
                enabled: enabled.as_deref() == Some(p.id()),
            })
            .collect()
    }

    /// 是否真有可用目标（id 在场**且**这一档确实注册了）
    pub fn is_enabled(&self) -> bool {
        let enabled = self.enabled.read().clone();
        let Some(id) = enabled else { return false };
        self.providers.iter().any(|p| p.id() == id)
    }

    /// 启用哪一档。`None` = 关掉上传。未知 id 拒并把已注册集合原样列出——
    /// 用户手打的未注册 id（比如桥未注入时的 `"webdav"`）在报错里就该看见
    /// 已注册集合原样列出。
    pub fn set_enabled(&self, id: Option<&str>) -> Result<(), AppError> {
        let Some(id) = id else {
            *self.enabled.write() = None;
            return Ok(());
        };
        if !self.providers.iter().any(|p| p.id() == id) {
            let registered: Vec<&str> = self.providers.iter().map(|p| p.id()).collect();
            let listed = if registered.is_empty() {
                "（当前没有已注册目标：先填写端点）".to_owned()
            } else {
                format!("已注册：{}", registered.join("、"))
            };
            return Err(err(CODE_STATE, format!("未知的上传目标「{id}」，{listed}")));
        }
        *self.enabled.write() = Some(id.to_owned());
        Ok(())
    }

    /// 交出去。**未启用 → `Ok(None)` 且零 provider 调用**：这一档语义是给动作链用的
    /// （`post_actions` 里有 `"upload"` 而用户后来把开关关了，不是错误，是不做了）。
    pub async fn upload(
        &self,
        bytes: &[u8],
        filename: &str,
        header_value: Option<&str>,
    ) -> Result<Option<String>, AppError> {
        let Some(id) = self.enabled.read().clone() else {
            return Ok(None);
        };
        let provider = self
            .providers
            .iter()
            .find(|p| p.id() == id)
            .ok_or_else(|| err(CODE_STATE, format!("上传目标「{id}」已被移除")))
            .cloned()?;
        provider
            .upload(bytes, filename, header_value)
            .await
            .map(Some)
    }
}

/// 由配置装配注册表（`init` 与 `apply_config` 两处唯一的构造点；WebDAV 档还需
/// 宿主桥在场，见 [`registry_from_config_with`]）。
///
/// 端点不合规时**不注册**而不是报错：这条函数没有失败通路（配置早已在 `apply_config`
/// 裁过），走到这里还不合规只可能是手改 JSON，此时"没有目标"比"模块起不来"诚实。
pub fn registry_from_config(cfg: &ScreenshotConfig) -> UploadRegistry {
    registry_from_config_with(cfg, None)
}

/// 带桥形态的装配口（模块层用这个——桥由 `ScreenshotModule::set_upload_sender`
/// 一次性注入；两档 provider 消费同一个 `upload_endpoint`，选哪档是 `upload_target`
/// 那一格的事，**零新配置键**）。
pub fn registry_from_config_with(
    cfg: &ScreenshotConfig,
    sender: Option<&WebDavSend>,
) -> UploadRegistry {
    let mut providers: Vec<Arc<dyn UploadProvider>> = Vec::new();
    if let Some(p) = HttpFormProvider::from_config(cfg) {
        if validate_upload_endpoint(&p.endpoint).is_ok() {
            providers.push(Arc::new(p));
        } else {
            tracing::warn!(endpoint = %endpoint_for_display(&cfg.upload_endpoint), "上传端点不合规，本代不注册该目标");
        }
    }
    if let Some(p) = WebDavUploadProvider::from_config(cfg, sender) {
        if validate_upload_endpoint(&p.endpoint).is_ok() {
            providers.push(Arc::new(p));
        }
        // 不合规则连 warn 都由上一档的同一句话代表——端点两档共用，不重复点名
    }
    let reg = UploadRegistry::new(providers);
    let want = cfg.upload_enabled.then_some(cfg.upload_target.as_str());
    if let Err(e) = reg.set_enabled(want) {
        tracing::warn!(error = %e, "上传目标启用失败，按未启用继续");
    }
    reg
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 计数 provider：`uploadDisabled_*` 那枚要证的是"一次都没调"，而"没报错"证明不了它
    pub(crate) struct CountingProvider {
        pub(crate) calls: Arc<AtomicUsize>,
        pub(crate) last_body_len: Arc<RwLock<usize>>,
        pub(crate) link: &'static str,
    }

    impl CountingProvider {
        pub(crate) fn new(link: &'static str) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                last_body_len: Arc::new(RwLock::new(0)),
                link,
            }
        }
    }

    #[async_trait]
    impl UploadProvider for CountingProvider {
        fn id(&self) -> &'static str {
            "counting"
        }
        fn label(&self) -> String {
            "计数档".into()
        }
        fn endpoint_display(&self) -> String {
            "https://counting.invalid/".into()
        }
        async fn upload(
            &self,
            bytes: &[u8],
            _filename: &str,
            _header_value: Option<&str>,
        ) -> Result<String, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_body_len.write() = bytes.len();
            Ok(self.link.to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::CountingProvider;
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    #[allow(non_snake_case)] // 任务书（09 §9.2 T-B4-9）字面测试名优先于 rustc 命名惯例
    fn uploadEndpoint_plainHttpRemote_rejected() {
        // 红线三臂：https 公网放行、明文 http 本机放行、明文 http 公网拒
        assert!(validate_upload_endpoint("https://x.example.com/u").is_ok());
        assert!(validate_upload_endpoint("http://127.0.0.1:8080/u").is_ok());
        assert!(validate_upload_endpoint("http://localhost/u").is_ok());
        assert!(validate_upload_endpoint("http://[::1]:8080/u").is_ok());
        for bad in [
            "http://evil.example/u",
            "http://[::ffff:1.2.3.4]/",
            "http://127.0.0.1.evil.example/u",
        ] {
            let e = validate_upload_endpoint(bad).unwrap_err();
            assert_eq!(e.code(), CODE_ENDPOINT, "{bad} 必须被端点裁决拒");
            assert!(
                e.to_string().contains("https") || e.to_string().contains("本机"),
                "{bad} 的拒绝消息要指名规则，而不是只说\"不合法\"：{e}"
            );
        }
        // 形态面四臂：相对地址、无 host、换行、非 http 族 scheme 各自点名
        let forms = [
            ("/upload", "缺 scheme"),
            ("https://", "无主机名"),
            ("http://127.0.0.1/\nu.png", "换行"),
            ("ftp://127.0.0.1/u", "非 http 族"),
            ("https://user:pw@host.example/u", "内嵌凭据"),
        ];
        for (url, why) in forms {
            let e = validate_upload_endpoint(url).unwrap_err();
            assert_eq!(e.code(), CODE_ENDPOINT, "{why} 应被拒：{url}");
        }
        // 空串与纯空白：设置里"清空端点"是合法动作（registry_from_config 走不注册），
        // 但 validate 本身对空地址就是不给过——两处各说各的话
        assert!(validate_upload_endpoint("").is_err());
        assert!(validate_upload_endpoint("   ").is_err());
    }

    #[test]
    #[allow(non_snake_case)]
    fn uploadProvider_linkTemplate_replacesIdAndUrl() {
        // {id} ← 文件名去扩展名（本地可推，不信响应体）
        assert_eq!(
            apply_link_template(
                Some("https://img.example/i/{id}"),
                "shot_2026-09-22_010203.png",
                "abc123\n",
            ),
            "https://img.example/i/shot_2026-09-22_010203"
        );
        // {url} ← 响应首行（多行响应只取首行，尾行的调试信息不进直链）
        assert_eq!(
            apply_link_template(
                Some("{url}?raw=1"),
                "a.png",
                "https://up.example/o/a.png\nserver debug",
            ),
            "https://up.example/o/a.png?raw=1"
        );
        // 缺省模板 = 响应首行原样；两格同时在场也各自落地
        assert_eq!(
            apply_link_template(None, "a.png", "https://up.example/o/a.png"),
            "https://up.example/o/a.png"
        );
        assert_eq!(
            apply_link_template(Some("{id}@{url}"), "a.b.png", "U"),
            "a.b@U",
            "多点名字要按最后一个点切（a.b.png 的 id 是 a.b，不是 a）"
        );
        // 正对照：模板里两个占位符都不出现时，原样返回（自建端点常给固定路径）
        assert_eq!(
            apply_link_template(Some("https://h/fixed"), "a.png", "U"),
            "https://h/fixed"
        );
        // 带目录的 filename 不把路径带进 {id}
        assert_eq!(
            apply_link_template(Some("{id}"), "C:\\shots\\x.png", "U"),
            "x"
        );
        // 空白模板 = 未填：走响应首行，而不是产出一个空链
        assert_eq!(
            apply_link_template(Some("   "), "a.png", "https://h/a"),
            "https://h/a"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn uploadFormBody_base64FieldAndNoShellOfValue() {
        // 三格全编：字段名、Base64 正文、文件名。b64(b"hi") = "aGk="，
        // 尾部的 `=` 必须成 `%3D`——留在原地就是对面接一个被截断的字段值
        assert_eq!(
            build_form_body("file", "a&b=c.png", b"hi"),
            "file=aGk%3D&filename=a%26b%3Dc.png"
        );
        // 正对照：不该编的没被编（unreserved 集合原样过），Base64 字母表自身不进编码
        assert_eq!(
            build_form_body("file", "c.png", b"\x00\x01\x02"),
            "file=AAEC&filename=c.png"
        );
        // Base64 的 `+` 与 `/` 是 form-encoding 的两个经典歧义源（'+' 会被解成空格）
        assert_eq!(
            build_form_body("file", "c", b"\xfb\xff"),
            "file=%2B%2F8%3D&filename=c"
        );
        // 换行与空格：任何一格都不许留下裸控制符（表单走私的第二段字段就是这么来的）
        let evil = build_form_body("file", "x.png&admin=1\nSet-Cookie: a=b", b"hi");
        assert!(!evil.contains('\n'), "正文里不得出现裸换行：{evil}");
        assert!(!evil.contains("&admin"), "文件名不许开出第二个字段：{evil}");
        assert!(evil.contains("%26admin%3D1"));
        // 字段名同样不可信（它来自用户手填的配置）
        assert_eq!(
            build_form_body("a b", "c", b"x"),
            "a%20b=eA%3D%3D&filename=c"
        );
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn uploadRegistry_unknownTarget_rejectsListingRegistered() {
        let counter = Arc::new(CountingProvider::new("https://ok/1"));
        let calls = counter.calls.clone();
        let reg = UploadRegistry::new(vec![counter]);
        // 未知 id：拒，并把已注册集合原样点名（正对照=先注册再拒，否则消息里的"只有 http-form"没来源）
        let e = reg.set_enabled(Some("webdav")).unwrap_err();
        assert_eq!(e.code(), CODE_STATE);
        let msg = e.to_string();
        assert!(msg.contains("webdav"), "错误要点名被拒的那个值：{msg}");
        assert!(msg.contains("counting"), "错误要列出已注册集合：{msg}");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "拒掉目标不该产生上传");
        // 启用得逞后 upload() 走这一档
        reg.set_enabled(Some("counting")).unwrap();
        assert!(reg.is_enabled());
        let link = reg.upload(b"bytes", "f.png", None).await.unwrap();
        assert_eq!(link.as_deref(), Some("https://ok/1"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // 关掉 = None，且关掉之后 upload 不再触及 provider
        reg.set_enabled(None).unwrap();
        assert!(!reg.is_enabled());
        assert_eq!(reg.upload(b"bytes", "f.png", None).await.unwrap(), None);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "未启用时零 provider 调用");
        // 空注册表：目标列表与"未注册"话术各有落点（set_enabled 的错误消息要说明没有档）
        let none = UploadRegistry::empty();
        assert!(none.targets().is_empty());
        let e2 = none.set_enabled(Some("http-form")).unwrap_err();
        assert!(e2.to_string().contains("没有已注册目标"), "{e2}");
        // targets() 的 enabled 位跟的是同一格（面板据此决定上传钮出不出）
        let info = reg.targets();
        assert_eq!(info[0].id, "counting");
        assert!(!info[0].enabled, "已关掉的目标不许在列表里显示为启用");
        assert_eq!(info[0].endpoint_display, "https://counting.invalid/");
    }

    #[test]
    #[allow(non_snake_case)]
    fn uploadHeader_valueNeverSerializedIntoConfig() {
        // 静态面：ScreenshotConfig 结构体源码里没有凭据位（读源断言，§9.1-⑪ B3 随修④ 样板）
        let src = include_str!("types.rs");
        let body = src
            .split("pub struct ScreenshotConfig")
            .nth(1)
            .expect("结构体存在")
            .split("\n}")
            .next()
            .expect("结构体闭合");
        for forbidden in ["token", "header_value", "secret", "api_key", "password"] {
            assert!(
                !body.to_ascii_lowercase().contains(forbidden),
                "ScreenshotConfig 不得出现凭据字段 {forbidden}（它会被序列化进 screenshot.json 与配置备份）"
            );
        }
        // 正对照：这一段确实是"有内容的配置定义"，而不是空结构体被切错了范围
        assert!(
            body.contains("upload_header_name"),
            "请求头名可以落盘（非密），它是本断言的对照组"
        );
        assert!(body.contains("upload_endpoint"));
        // 行为面：序列化后的键集同样没有凭据位，而 upload_* 七键齐备
        let json = serde_json::to_value(ScreenshotConfig::default()).unwrap();
        let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        for forbidden in ["token", "header_value", "secret", "api_key", "password"] {
            assert!(
                !keys
                    .iter()
                    .any(|k| k.to_ascii_lowercase().contains(forbidden)),
                "序列化键里冒出 {forbidden}"
            );
        }
        for expected in [
            "upload_enabled",
            "upload_target",
            "upload_endpoint",
            "upload_field",
            "upload_header_name",
            "upload_link_template",
            "upload_copy_link",
        ] {
            assert!(keys.contains(&&expected.to_owned()), "缺键 {expected}");
        }
        // 默认关：七键在场不等于这条通路默认会发请求
        assert_eq!(json["upload_enabled"], serde_json::json!(false));
        assert_eq!(json["upload_field"], serde_json::json!("file"));
    }

    #[test]
    #[allow(non_snake_case)]
    fn uploadRegistry_fromConfig_emptyOrInsecureEndpoint_registersNothing() {
        // 端点留空 → 一档都没有（面板拿空表说"未注册目标"）
        let cfg = ScreenshotConfig::default();
        let reg = registry_from_config(&cfg);
        assert!(reg.targets().is_empty());
        assert!(!reg.is_enabled());
        // 填了合规端点 + 启用 → http-form 在列且 enabled
        let cfg = ScreenshotConfig {
            upload_enabled: true,
            upload_target: HTTP_FORM_ID.into(),
            upload_endpoint: "http://127.0.0.1:8080/up".into(),
            ..Default::default()
        };
        let reg = registry_from_config(&cfg);
        let info = reg.targets();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].id, HTTP_FORM_ID);
        assert!(info[0].enabled);
        // 手改 JSON 塞进来的公网明文：不注册（而不是让模块起不来），且启用位落空
        let cfg = ScreenshotConfig {
            upload_enabled: true,
            upload_target: HTTP_FORM_ID.into(),
            upload_endpoint: "http://evil.example/up".into(),
            ..Default::default()
        };
        let reg = registry_from_config(&cfg);
        assert!(reg.targets().is_empty(), "不合规端点不许成为目标");
        assert!(!reg.is_enabled());
        // query 里的 token 不进展示面（红线：目标列表要渲染进面板）
        let cfg = ScreenshotConfig {
            upload_endpoint: "https://h/up?key=super-secret".into(),
            ..Default::default()
        };
        let reg = registry_from_config(&cfg);
        let info = reg.targets();
        assert_eq!(info[0].endpoint_display, "https://h/up");
        assert!(!info[0].endpoint_display.contains("super-secret"));
    }
}

// ---------------------------------------------------------------------------
// T-B6-12 接线判据（宿主桥形状在测试里由闭包替身给出——零网络；真 PUT 腿的
// 装配与提交形状归 file-core 的 assemble 测与人工实启冒烟）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod webdav_tests {
    use super::test_support::CountingProvider;
    use super::*;
    use std::sync::Mutex as StdMutex;

    type Captured = std::sync::Arc<StdMutex<Vec<(String, Option<String>, Vec<u8>)>>>;

    /// 记录请求并回给定直链的桥替身（`Err` 注入位同样给足：reject 臂要有真因）
    fn bridge(link: &'static str, captured: &Captured) -> WebDavSend {
        let cap = captured.clone();
        Arc::new(move |req: WebDavPutRequest| {
            cap.lock().unwrap().push((
                req.filename.clone(),
                req.header_value.clone(),
                req.bytes.clone(),
            ));
            if link.is_empty() {
                return Box::pin(async move { Err("桥替身注入：对端收尾被拒".to_owned()) })
                    as WebDavSendFuture;
            }
            let owned = link.to_owned();
            Box::pin(async move { Ok(owned) }) as WebDavSendFuture
        })
    }

    fn cfg_with(target: &str) -> ScreenshotConfig {
        ScreenshotConfig {
            upload_enabled: true,
            upload_target: target.into(),
            upload_endpoint: "https://dav.example.org/dav".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    #[allow(non_snake_case)] // 任务书（09 §6.2 T-B6-12）字面测试名优先于 rustc 命名惯例
    async fn webdavUploadProvider_registryListsItAndAcceptsSelection() {
        // 承重：接上桥后 targets() 含 id=="webdav" 且 set_enabled 从此 Ok——
        // B4 那枚"只有 http-form"的拒答语义在此翻正（改判见 09 行内）
        let cap: Captured = Default::default();
        let reg = registry_from_config_with(
            &cfg_with("webdav"),
            Some(&bridge("https://dav.example.org/dav/shot.png", &cap)),
        );
        let ids: Vec<String> = reg.targets().iter().map(|t| t.id.clone()).collect();
        assert!(
            ids.contains(&"webdav".to_owned()),
            "webdav 档须在册: {ids:?}"
        );
        reg.set_enabled(Some("webdav"))
            .expect("在册即须选得上（防新档在册但选不上）");
        assert!(reg.is_enabled());
        let link = reg
            .upload(b"PNGDATA", "shot.png", Some("Basic AAA"))
            .await
            .unwrap()
            .expect("启用档必须出链");
        assert_eq!(link, "https://dav.example.org/dav/shot.png");
        let got = cap.lock().unwrap().pop().unwrap();
        assert_eq!(got.0, "shot.png");
        assert_eq!(got.2, b"PNGDATA".to_vec());
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn webdavUpload_missingBridge_registersNothing() {
        // 缺注入 → 表里没有 webdav（不注册一枚必失败的目标），选它仍被点名拒
        let reg = registry_from_config(&cfg_with("webdav"));
        let ids: Vec<String> = reg.targets().iter().map(|t| t.id.clone()).collect();
        assert!(
            !ids.iter().any(|i| i == WEBDAV_ID),
            "无桥不得有 webdav 档: {ids:?}"
        );
        let e = reg.set_enabled(Some(WEBDAV_ID)).unwrap_err();
        assert_eq!(e.code(), CODE_STATE);
        assert!(
            e.to_string().contains("http-form"),
            "已注册集合原样列出: {e}"
        );
        // 且启用位落空 ⇒ 一次 provider 调用都不该发生（空转链条不成立）
        assert!(!reg.is_enabled());
        assert_eq!(reg.upload(b"x", "f.png", None).await.unwrap(), None);
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn webdavUpload_secretTravelsPerCall_notInConfig() {
        // 夹具口令 SUPER_SECRET → ScreenshotConfig 序列化零命中；出站请求夹具含之
        //（"只进不出"的截图侧镜像：值走参数通道，配置面永远没有它）
        let cap: Captured = Default::default();
        let cfg = cfg_with("webdav");
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("SUPER_SECRET"), "配置序列化面漏凭据: {json}");
        let reg = registry_from_config_with(
            &cfg,
            Some(&bridge("https://dav.example.org/dav/f.png", &cap)),
        );
        reg.upload(b"IMG", "f.png", Some("Basic SUPER_SECRET"))
            .await
            .unwrap()
            .expect("同站直链放行");
        let header = cap.lock().unwrap()[0].1.clone();
        assert_eq!(
            header.as_deref(),
            Some("Basic SUPER_SECRET"),
            "值须到桥（逐次通道）"
        );
        // 再传一次不给值：桥收到 None（不缓存、不复用上一次的凭据）
        reg.upload(b"IMG", "g.png", None).await.unwrap().unwrap();
        assert_eq!(cap.lock().unwrap()[1].1, None);
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn webdavUpload_linkTemplate_rejectsCrossOriginRedirect() {
        // 跨源拒（劫持/错配都不配被回显），同站重定向正对照放行
        let cap: Captured = Default::default();
        let evil = registry_from_config_with(
            &cfg_with("webdav"),
            Some(&bridge("https://evil.example/steal.png", &cap)),
        );
        evil.set_enabled(Some(WEBDAV_ID)).unwrap();
        let e = evil
            .upload(b"I", "a.png", None)
            .await
            .expect_err("跨源直链必须拒");
        assert_eq!(e.code(), CODE_TRANSPORT);
        assert!(e.to_string().contains("不同源"), "{e}");
        let ok = registry_from_config_with(
            &cfg_with("webdav"),
            Some(&bridge("https://dav.example.org/dav/sub/a.png", &cap)),
        );
        ok.set_enabled(Some(WEBDAV_ID)).unwrap();
        let link = ok.upload(b"I", "a.png", None).await.unwrap().unwrap();
        assert!(link.starts_with("https://dav.example.org/"));
        // 桥报错臂：Err(点名原因) 原样进 CODE_TRANSPORT，不塌成"上传失败"
        let bad = registry_from_config_with(&cfg_with("webdav"), Some(&bridge("", &cap)));
        bad.set_enabled(Some(WEBDAV_ID)).unwrap();
        let e = bad.upload(b"I", "a.png", None).await.err().unwrap();
        assert!(e.to_string().contains("收尾被拒"), "{e}");
    }

    #[tokio::test]
    #[allow(non_snake_case)]
    async fn webdavUpload_twoTargets_oneImage_publishesToBoth() {
        // 一次配置注册出两档（同一端点键，选哪档是一格的事）；同一字节流
        // 分别经两档各交一次——含正对照防"恒单目标"（http-form 真表单腿的
        // 出站形状属 B4 既有判据，此处不重钉，替身档在场只为证并存可选）
        let counting = Arc::new(CountingProvider::new("https://ok/1"));
        let calls = counting.calls.clone();
        let cap: Captured = Default::default();
        let webdav = WebDavUploadProvider::from_config(
            &cfg_with("http-form"),
            Some(&bridge("https://dav.example.org/dav/x.png", &cap)),
        )
        .expect("cfg 端点非空且有桥");
        let reg = UploadRegistry::new(vec![counting, Arc::new(webdav)]);
        reg.set_enabled(Some("counting")).unwrap();
        reg.upload(b"SAME-BYTES", "x.png", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        reg.set_enabled(Some(WEBDAV_ID)).unwrap();
        reg.upload(b"SAME-BYTES", "x.png", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cap.lock().unwrap()[0].2, b"SAME-BYTES".to_vec());
        // 正对照：targets() 两枚并列（恒单目标在此现形）
        assert_eq!(reg.targets().len(), 2);
    }
}
