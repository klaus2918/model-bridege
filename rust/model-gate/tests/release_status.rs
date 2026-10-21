// 发布状态探针：通过 GitHub API 查 Release 与 Actions 运行状态。
// 用 Rust 客户端（系统根 + webpki 双根）绕过本地加速器的 MITM 证书。
//
// 用法：cargo test --test release_status -- --ignored --nocapture

use std::time::Duration;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent(format!("Model-Gate/{}", env!("CARGO_PKG_VERSION")))
        .tls_built_in_native_certs(true)
        .tls_built_in_webpki_certs(true)
        .build()
        .expect("client")
}

const API: &str = "https://api.github.com/repos/klaus2918/model-bridege";

#[tokio::test]
#[ignore = "needs real network; run manually"]
async fn report_release_and_runs() {
    let c = client();

    println!("=== latest release ===");
    let resp = c
        .get(format!("{API}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .expect("release request");
    println!("status = {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("release json");
    println!("tag_name    = {}", body["tag_name"].as_str().unwrap_or("?"));
    println!("name        = {}", body["name"].as_str().unwrap_or("?"));
    println!("published   = {}", body["published_at"].as_str().unwrap_or("?"));
    println!("html_url    = {}", body["html_url"].as_str().unwrap_or("?"));
    println!("assets:");
    if let Some(assets) = body["assets"].as_array() {
        for a in assets {
            println!(
                "  - {} ({} bytes) -> {}",
                a["name"].as_str().unwrap_or("?"),
                a["size"].as_u64().unwrap_or(0),
                a["browser_download_url"].as_str().unwrap_or("?")
            );
        }
    } else {
        println!("  (none)");
    }

    println!();
    println!("=== workflow runs ===");
    let resp = c
        .get(format!("{API}/actions/runs?per_page=5"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .expect("runs request");
    println!("status = {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("runs json");
    if let Some(runs) = body["workflow_runs"].as_array() {
        for r in runs {
            println!(
                "  {} | {} | {} | {} | ref={}",
                r["id"].as_u64().unwrap_or(0),
                r["name"].as_str().unwrap_or("?"),
                r["head_branch"].as_str().unwrap_or("?"),
                r["status"].as_str().unwrap_or("?"),
                r["conclusion"].as_str().unwrap_or("-")
            );
        }
    }
}
