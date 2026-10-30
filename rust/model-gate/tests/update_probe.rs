// 探针：用与 update.rs 相同的客户端配置访问 GitHub 公开 Release 入口，
// 验证系统根证书 + webpki 双根在「hosts 劫持 + 本地 MITM」环境下能否握手成功。
//
// 这里刻意走 `github.com/releases/latest`（302 到 tag 页），而不是
// `api.github.com`：检查更新不应消耗匿名 REST API 配额。
//
// 用法：cargo test --test update_probe -- --ignored --nocapture
// 或：  cargo run --example update_probe
//
// 放在 tests/ 下作为集成测试，避免污染产品代码。

use std::time::Duration;

#[tokio::test]
#[ignore = "需要真实网络；由人工在有网环境执行"]
async fn probe_release_asset_head() {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(format!("Model-Gate/{}", env!("CARGO_PKG_VERSION")))
        .tls_built_in_native_certs(true)
        .tls_built_in_webpki_certs(true)
        .build()
        .expect("client");

    let url = "https://github.com/klaus2918/model-bridege/releases/download/v2.27.0/Model-Gate-v2.27.0-rust-setup.exe";
    match client
        .head(url)
        .timeout(Duration::from_secs(15))
        .send()
        .await
    {
        Ok(response) => {
            println!("STATUS = {}", response.status());
            println!("FINAL  = {}", response.url());
            println!("LENGTH = {:?}", response.content_length());
            println!("HEADERS = {:?}", response.headers());
            assert!(response.status().is_success());
        }
        Err(err) => {
            println!("ERROR  = {err}");
            let mut source = std::error::Error::source(&err);
            while let Some(inner) = source {
                println!("  caused by: {inner}");
                source = inner.source();
            }
            panic!("资产 HEAD 失败：{err}");
        }
    }
}

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
        .get("https://github.com/klaus2918/model-bridege/releases/latest")
        .header("Accept", "text/html")
        .timeout(Duration::from_secs(15))
        .send()
        .await;

    match resp {
        Ok(response) => {
            let status = response.status();
            let final_url = response.url().to_string();
            println!("STATUS = {status}");
            println!("FINAL  = {final_url}");
            assert!(status.is_success());
            assert!(
                final_url.contains("/releases/tag/"),
                "releases/latest 未跳转到 tag 页：{final_url}"
            );
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
