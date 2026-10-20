// 探针：用与 update.rs 相同的客户端配置访问 GitHub API，验证
// 系统根证书 + webpki 双根在「hosts 劫持 + 本地 MITM」环境下能否握手成功。
//
// 用法：cargo test --test update_probe -- --ignored --nocapture
// 或：  cargo run --example update_probe
//
// 放在 tests/ 下作为集成测试，避免污染产品代码。

use std::time::Duration;

#[tokio::test]
#[ignore = "需要真实网络；由人工在有网环境执行"]
async fn probe_github_releases_latest() {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(format!("Model-Gate/{}", env!("CARGO_PKG_VERSION")))
        .tls_built_in_native_certs(true)
        .tls_built_in_webpki_certs(true)
        .build()
        .expect("client");

    let resp = client
        .get("https://api.github.com/repos/klaus2918/model-bridege/releases/latest")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .timeout(Duration::from_secs(15))
        .send()
        .await;

    match resp {
        Ok(response) => {
            let status = response.status();
            println!("STATUS = {status}");
            let body = response.text().await.unwrap_or_default();
            println!("BODY   = {}", &body[..body.len().min(500)]);
            assert!(status.is_success());
        }
        Err(err) => {
            println!("ERROR  = {err}");
            let mut source = std::error::Error::source(&err);
            while let Some(inner) = source {
                println!("  caused by: {inner}");
                source = inner.source();
            }
            panic!("GitHub 访问失败：{err}");
        }
    }
}
