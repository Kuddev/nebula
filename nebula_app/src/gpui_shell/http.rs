//! gpui 图片管线的 HTTP 客户端。
//!
//! gpui 默认装的是 `NullHttpClient`（一切请求直接报错），markdown 文档里的
//! 网络图（shields 徽章、外链截图）会全部加载失败。这里用项目里已有的
//! ureq（rustls）实现 `http_client::HttpClient`，请求跑在 gpui 的后台
//! executor 线程上，不阻塞 UI。
//!
//! 范围合同：只服务只读资源加载（GET 语义的图片/附件），跟随重定向；
//! 不实现代理与流式 body——gpui 图片缓存一次性 `read_to_end`，聚合 body
//! 足够。
//!
//! 错误类型用 gpui `http_client` 再导出的 `Result`（trait 签名要求），
//! 本 crate 不直接依赖 anyhow。

use std::sync::Arc;
use std::time::Duration;

use futures::AsyncReadExt;
use gpui::BackgroundExecutor;
use gpui::http_client::{AsyncBody, HttpClient, Result as HttpResult, Url, anyhow};

/// 单张图片的响应体上限。gpui 会把 body 全量读进内存再解码，超大响应
/// （错误配置的链接指向视频等）不该拖垮查看器。
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

pub struct UreqClient {
    executor: BackgroundExecutor,
    agent: ureq::Agent,
}

impl UreqClient {
    pub fn new(executor: BackgroundExecutor) -> Self {
        let agent = ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(30)))
            // 状态码不当错误：gpui 侧按 `status().is_success()` 自行分流，
            // 让它拿到真实状态码而不是 ureq 的 Status 错误。
            .http_status_as_error(false)
            .build()
            .new_agent();
        Self { executor, agent }
    }
}

type FetchedBody = (u16, Vec<(String, Vec<u8>)>, Vec<u8>);

fn fetch_blocking(
    agent: ureq::Agent,
    parts: gpui::http_client::http::request::Parts,
    request_bytes: Vec<u8>,
) -> HttpResult<FetchedBody> {
    let mut builder = ureq::http::Request::builder()
        .method(parts.method.as_str())
        .uri(parts.uri.to_string());
    for (name, value) in parts.headers.iter() {
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let request = builder
        .body(request_bytes)
        .map_err(|error| anyhow!("构造请求失败: {error}"))?;
    let mut response = agent.run(request).map_err(|error| anyhow!("{error}"))?;
    let status = response.status().as_u16();
    let mut header_pairs: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, value) in response.headers().iter() {
        header_pairs.push((name.as_str().to_owned(), value.as_bytes().to_vec()));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_vec()
        .map_err(|error| anyhow!("读取响应失败: {error}"))?;
    Ok((status, header_pairs, bytes))
}

impl HttpClient for UreqClient {
    fn type_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn user_agent(&self) -> Option<&gpui::http_client::http::HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        request: gpui::http_client::http::Request<AsyncBody>,
    ) -> futures::future::BoxFuture<'static, HttpResult<gpui::http_client::Response<AsyncBody>>>
    {
        let agent = self.agent.clone();
        let executor = self.executor.clone();
        Box::pin(async move {
            let (parts, mut body) = request.into_parts();
            // AsyncBody 是异步流，在 async 上下文里先聚合成字节再交给
            // 阻塞的 ureq；图片加载的请求体实际为空。
            let mut request_bytes = Vec::new();
            body.read_to_end(&mut request_bytes).await?;

            let (status, header_pairs, bytes) =
                executor.spawn(async move { fetch_blocking(agent, parts, request_bytes) }).await?;

            let mut builder = gpui::http_client::http::Response::builder().status(status);
            for (name, value) in header_pairs {
                builder = builder.header(name, value);
            }
            builder
                .body(AsyncBody::from(bytes))
                .map_err(|error| anyhow!("构造响应失败: {error}"))
        })
    }
}

/// 注册为 gpui 的全局 HTTP 客户端；`gpui_shell::init` 调用一次。
pub fn register(cx: &mut gpui::App) {
    cx.set_http_client(Arc::new(UreqClient::new(cx.background_executor().clone())));
}
