// Pipeline step probe: which step is the v2.21.0 release run on?
// ASCII-only output.

use std::time::Duration;

#[tokio::test]
#[ignore = "needs real network; run manually"]
async fn report_run_steps() {
    let c = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent(format!("Model-Gate/{}", env!("CARGO_PKG_VERSION")))
        .tls_built_in_native_certs(true)
        .tls_built_in_webpki_certs(true)
        .build()
        .expect("client");

    const API: &str = "https://api.github.com/repos/klaus2918/model-bridege";

    // find the run for v2.21.0
    let body: serde_json::Value = c
        .get(format!("{API}/actions/runs?per_page=5"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .expect("runs")
        .json()
        .await
        .expect("runs json");

    let mut run_id = 0u64;
    if let Some(runs) = body["workflow_runs"].as_array() {
        for r in runs {
            let branch = r["head_branch"].as_str().unwrap_or("");
            println!(
                "run {} | {} | {} | {}",
                r["id"].as_u64().unwrap_or(0),
                branch,
                r["status"].as_str().unwrap_or("?"),
                r["conclusion"].as_str().unwrap_or("-")
            );
            if branch == "v2.21.0" && run_id == 0 {
                run_id = r["id"].as_u64().unwrap_or(0);
            }
        }
    }
    if run_id == 0 {
        println!("no v2.21.0 run found");
        return;
    }

    println!();
    println!("=== jobs of run {} ===", run_id);
    let jobs: serde_json::Value = c
        .get(format!("{API}/actions/runs/{run_id}/jobs"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .expect("jobs")
        .json()
        .await
        .expect("jobs json");
    if let Some(list) = jobs["jobs"].as_array() {
        for j in list {
            println!(
                "job {} | status={} | conclusion={}",
                j["name"].as_str().unwrap_or("?"),
                j["status"].as_str().unwrap_or("?"),
                j["conclusion"].as_str().unwrap_or("-")
            );
            if let Some(steps) = j["steps"].as_array() {
                for s in steps {
                    println!(
                        "    step {} | {} | {}",
                        s["number"].as_u64().unwrap_or(0),
                        s["name"].as_str().unwrap_or("?"),
                        s["conclusion"]
                            .as_str()
                            .unwrap_or(s["status"].as_str().unwrap_or("-"))
                    );
                }
            }
        }
    }
}
