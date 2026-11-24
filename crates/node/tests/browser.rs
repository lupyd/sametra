//! End to end in a real browser: the embedded SPA renders live node data.
//! Needs headless Chromium (CHROME=/path or chromium/google-chrome on PATH) and a
//! built UI (web/dist); skips with a message otherwise.
mod common;
use common::*;
use sametra_node::pb;
use std::process::Command;

fn chrome() -> Option<String> {
    let cands = std::env::var("CHROME").into_iter().chain(["chromium", "chromium-browser", "google-chrome-stable", "google-chrome"].map(String::from));
    cands.into_iter().find(|c| Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))
}

/// Load a URL headless, let the app fetch and render, return the serialized DOM.
fn dom(chrome: &str, url: &str, profile: &std::path::Path) -> String {
    let o = Command::new(chrome)
        .args(["--headless=new", "--no-sandbox", "--disable-gpu", "--no-first-run", "--disable-extensions", "--virtual-time-budget=8000", "--dump-dom"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(url)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spa_renders_node_data() {
    let Some(chrome) = chrome() else {
        eprintln!("SKIP: no headless chromium found (set CHROME=...)");
        return;
    };
    let n = spawn(|_| {}).await;
    if n.get("/").await.text().contains("Web UI not built") {
        eprintln!("SKIP: web/dist not built");
        return;
    }
    n.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Dr Browser".into(), about: "tests UIs".into(), ..Default::default() }).await;
    n.ok::<_, pb::Ack>("create_community", &pb::CreateCommunityRequest { slug: "oncology".into(), name: "Oncology".into(), about: "tumour biology".into() }).await;
    let root: pb::Ack = n.ok("post", &post("oncology", "Is marker X predictive?", "Looking for cohort data.")).await;
    let r1: pb::Ack = n.ok("post", &reply(&root.id, "We saw it in 40 patients.")).await;
    n.ok::<_, pb::Ack>("post", &reply(&r1.id, "Which assay did you use?")).await;
    let profile = tempdir("chrome");
    let base = format!("http://{}", n.host());
    let (c, b, p) = (chrome.clone(), base.clone(), profile.0.clone());
    let run = move |path: &str| {
        let (c, u, p) = (c.clone(), format!("{b}{path}"), p.clone());
        tokio::task::spawn_blocking(move || dom(&c, &u, &p))
    };

    let feed = run("/c/oncology").await.unwrap();
    assert!(feed.contains("Is marker X predictive?"), "feed shows the thread:\n{feed}");
    assert!(feed.contains("Oncology") && feed.contains("tumour biology"));
    assert!(feed.contains("2 replies"));
    assert!(feed.contains("Dr Browser"));

    let thread = run(&format!("/t/{}", root.id)).await.unwrap();
    for s in ["Looking for cohort data.", "We saw it in 40 patients.", "Which assay did you use?", "class=\"replies\""] {
        assert!(thread.contains(s), "thread view missing {s:?}");
    }

    let home = run("/").await.unwrap();
    assert!(home.contains("Is marker X predictive?"), "home redirects to the joined community");

    let me = run("/me").await.unwrap();
    assert!(me.contains("Dr Browser") && me.contains("tests UIs") && me.contains(&n.node.keys.npub()));
    assert!(me.contains("This node"));

    let chain = run(&format!("/chain/{}", r1.id)).await.unwrap();
    assert!(chain.contains("class=\"weavers\"") && chain.contains("Which assay did you use?") && chain.contains("Is marker X predictive?"), "chain view:\n{chain}");

    let search = run("/search?q=assay").await.unwrap();
    assert!(search.contains("Which assay did you use?"));

    let rep = run("/c/oncology/reputation").await.unwrap();
    assert!(rep.contains("Nobody here has been rated yet."));

    assert!(feed.contains("href=\"/docs\""), "the app links to the docs");
    let docs = run("/docs/architecture").await.unwrap();
    assert!(docs.contains("A node at a glance") && docs.contains("<svg class=\"dg\""), "docs render with diagrams");
    assert!(docs.contains("aria-current=\"page\">Architecture"));

    // capabilities: offered on the profile, found by need, requested into the inbox
    n.ok::<_, pb::Ack>("set_capability", &pb::CapabilitySpec { id: "assay".into(), name: "Plasma marker assay".into(), kind: "skill".into(), topics: vec!["proteomics".into()], ..Default::default() }).await;
    let need: pb::Ack = n.ok("post", &pb::PostRequest { needs: vec!["proteomics".into()], ..post("oncology", "Who can quantify marker X?", "plasma samples") }).await;
    let asker = sametra_node::event::Keys::generate();
    let job = asker.sign(
        sametra_node::event::K_JOB,
        vec![
            vec!["p".into(), n.node.keys.pubkey.hex()],
            vec!["param".into(), "capability".into(), "assay".into()],
            vec!["i".into(), "40 samples from cohort B".into(), "text".into()],
        ],
        String::new(),
    );
    n.node.ingest(job, 9, false).unwrap();
    let me = run("/me").await.unwrap();
    assert!(me.contains("Plasma marker assay") && me.contains("Offer a capability"));
    let help = run("/help?q=proteomics").await.unwrap();
    assert!(help.contains("Plasma marker assay"));
    let thread = run(&format!("/t/{}", need.id)).await.unwrap();
    assert!(thread.contains("Who can help") && thread.contains("proteomics"));
    let inbox = run("/inbox").await.unwrap();
    assert!(inbox.contains("40 samples from cohort B") && inbox.contains("Accept"));
    let search = run("/search?q=marker&scope=network").await.unwrap();
    assert!(search.contains("Who can quantify marker X?") && search.contains("best effort"));
    let comms = run("/communities?q=tumour").await.unwrap();
    assert!(comms.contains("tumour biology"));

    let missing = run("/nope/at/all").await.unwrap();
    assert!(missing.contains("There is nothing at this address"));
}
