//! Capabilities: NIP-89 announcements, NIP-90 requests/results, and the
//! separate capability reputation.
mod common;
use common::*;
use sametra_node::event::*;
use sametra_node::pb;
use std::time::Duration;

const T: Duration = Duration::from_secs(10);

fn tool(id: &str) -> pb::CapabilitySpec {
    pb::CapabilitySpec {
        id: id.into(),
        name: "Plasma assay".into(),
        about: "Quantifies marker X in plasma samples, 48 h turnaround".into(),
        kind: "tool".into(),
        input_schema: r#"{"type":"object","required":["sample"],"properties":{"sample":{"type":"string"}}}"#.into(),
        topics: vec!["Proteomics".into(), "mass spectrometry".into()],
        communities: vec!["onco".into()],
        delete: false,
    }
}
fn skill(id: &str) -> pb::CapabilitySpec {
    pb::CapabilitySpec { id: id.into(), name: "Histopathology".into(), about: "I read tumour slides".into(), kind: "skill".into(), topics: vec!["pathology".into()], ..Default::default() }
}
fn find(q: &str) -> pb::CapabilitiesRequest {
    pb::CapabilitiesRequest { query: q.into(), ..Default::default() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_search_and_withdraw() {
    let n = spawn(|_| {}).await;
    let bad = |f: &dyn Fn(&mut pb::CapabilitySpec)| {
        let mut s = tool("assay");
        f(&mut s);
        s
    };
    assert!(n.err("set_capability", &bad(&|s| s.id = "no spaces".into())).await.1.contains("'id'"));
    assert!(n.err("set_capability", &bad(&|s| s.name = " ".into())).await.1.contains("name"));
    assert!(n.err("set_capability", &bad(&|s| s.kind = "spell".into())).await.1.contains("tool or skill"));
    assert!(n.err("set_capability", &bad(&|s| s.input_schema = "{oops".into())).await.1.contains("not JSON"));
    assert!(n.err("set_capability", &bad(&|s| s.input_schema = "[1]".into())).await.1.contains("object"));
    assert!(n.err("set_capability", &bad(&|s| s.communities = vec!["Bad Slug".into()])).await.1.contains("slug"));
    n.ok::<_, pb::Ack>("set_capability", &tool("assay")).await;
    n.ok::<_, pb::Ack>("set_capability", &skill("pathology")).await;

    let all: pb::CapabilitiesResponse = n.ok("capabilities", &find("")).await;
    assert_eq!(all.capabilities.len(), 2);
    let r: pb::CapabilitiesResponse = n.ok("capabilities", &find("mass spectrometry plasma")).await;
    assert_eq!(r.capabilities.len(), 1);
    let c = &r.capabilities[0];
    let spec = c.spec.clone().unwrap();
    assert_eq!((spec.id.as_str(), spec.kind.as_str()), ("assay", "tool"));
    assert_eq!(spec.topics, ["proteomics", "mass spectrometry"], "topics are lowercased");
    assert!(spec.input_schema.contains("required") && c.text_match > 0.0);
    assert_eq!((c.provider.as_str(), c.requests, c.rep), (n.node.keys.pubkey.hex().as_str(), 0, 0.0));
    let by_kind: pb::CapabilitiesResponse = n.ok("capabilities", &pb::CapabilitiesRequest { kind: "skill".into(), ..Default::default() }).await;
    assert_eq!(by_kind.capabilities[0].spec.as_ref().unwrap().id, "pathology");
    let by_comm: pb::CapabilitiesResponse = n.ok("capabilities", &pb::CapabilitiesRequest { community: "onco".into(), ..Default::default() }).await;
    assert_eq!(by_comm.capabilities.len(), 1);
    let by_provider: pb::CapabilitiesResponse = n.ok("capabilities", &pb::CapabilitiesRequest { provider: n.node.keys.npub(), ..Default::default() }).await;
    assert_eq!(by_provider.capabilities.len(), 2);
    let none: pb::CapabilitiesResponse = n.ok("capabilities", &find("zebrafish")).await;
    assert!(none.capabilities.is_empty());

    // replacing and withdrawing
    n.ok::<_, pb::Ack>("set_capability", &pb::CapabilitySpec { name: "Plasma assay v2".into(), ..tool("assay") }).await;
    let r: pb::CapabilitiesResponse = n.ok("capabilities", &find("plasma")).await;
    assert_eq!(r.capabilities[0].spec.as_ref().unwrap().name, "Plasma assay v2");
    n.ok::<_, pb::Ack>("set_capability", &pb::CapabilitySpec { id: "assay".into(), delete: true, ..Default::default() }).await;
    let r: pb::CapabilitiesResponse = n.ok("capabilities", &find("")).await;
    assert_eq!(r.capabilities.len(), 1, "withdrawn capabilities disappear");
    // you cannot request your own capability
    let own = pb::JobRequest { provider: n.node.keys.npub(), capability: "pathology".into(), input: "x".into(), ..Default::default() };
    assert!(n.err("request", &own).await.1.contains("your own"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_accept_deliver_and_rate() {
    let (lab, asker, mallory) = (spawn(|_| {}).await, spawn(|_| {}).await, spawn(|_| {}).await);
    asker.connect(&lab).await;
    mallory.connect(&lab).await;
    lab.ok::<_, pb::Ack>("set_profile", &pb::Profile { name: "Hospital lab agent".into(), agent: true, ..Default::default() }).await;
    lab.ok::<_, pb::Ack>("set_capability", &tool("assay")).await;
    lab.ok::<_, pb::Ack>("set_capability", &skill("pathology")).await;

    // the asker discovers the lab by need
    let mut cap = None;
    for _ in 0..100 {
        let r: pb::CapabilitiesResponse = asker.ok("capabilities", &find("proteomics")).await;
        if let Some(c) = r.capabilities.into_iter().next() {
            cap = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let cap = cap.expect("announcement reached the asker");
    assert!(cap.provider_is_agent && cap.provider_name == "Hospital lab agent");

    // input validation against the tool's schema
    let req = |input: &str| pb::JobRequest { provider: cap.provider.clone(), capability: "assay".into(), input: input.into(), community: "onco".into(), ..Default::default() };
    assert!(asker.err("request", &req(r#"{"volume":2}"#)).await.1.contains("'sample'"));
    assert!(asker.err("request", &req("plain text")).await.1.contains("must be JSON"));
    assert!(asker.err("request", &pb::JobRequest { capability: "nope".into(), ..req("{}") }).await.1.contains("no capability 'nope'"));
    let post: pb::Ack = asker.ok("post", &pb::PostRequest { needs: vec!["plasma assay".into()], ..post("onco", "Need marker X measured", "two samples") }).await;
    let job: pb::Ack = asker.ok("request", &pb::JobRequest { post: post.id.clone(), ..req(r#"{"sample":"S-17"}"#) }).await;

    // the lab sees it and accepts
    let inbox = |role: &str| pb::InboxRequest { role: role.into(), ..Default::default() };
    let mut pending = None;
    for _ in 0..100 {
        let r: pb::JobsResponse = lab.ok("inbox", &inbox("provider")).await;
        if let Some(j) = r.jobs.into_iter().find(|j| j.id == job.id) {
            pending = Some(j);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let j = pending.expect("request reached the provider");
    assert_eq!((j.status.as_str(), j.capability.as_str(), j.input.as_str(), j.post.as_str(), j.community.as_str()), ("pending", "assay", r#"{"sample":"S-17"}"#, post.id.as_str(), "onco"));
    assert!(!j.mine && j.requester == asker.node.keys.pubkey.hex());
    let mine: pb::JobsResponse = asker.ok("inbox", &inbox("requester")).await;
    assert!(mine.jobs[0].mine && mine.jobs[0].status == "pending");
    assert!(asker.ok::<_, pb::JobsResponse>("inbox", &inbox("provider")).await.jobs.is_empty());

    lab.ok::<_, pb::Ack>("respond", &pb::RespondRequest { request: job.id.clone(), action: "accept".into(), content: String::new() }).await;
    assert!(eventually_status(&asker, &job.id, "accepted").await);

    // errors: not mine to answer, bad action, empty result
    assert!(asker.err("respond", &pb::RespondRequest { request: job.id.clone(), action: "accept".into(), ..Default::default() }).await.1.contains("not a request addressed to you"));
    assert!(lab.err("respond", &pb::RespondRequest { request: job.id.clone(), action: "shrug".into(), ..Default::default() }).await.1.contains("unknown action"));
    assert!(lab.err("respond", &pb::RespondRequest { request: job.id.clone(), action: "result".into(), ..Default::default() }).await.1.contains("content"));

    lab.ok::<_, pb::Ack>("respond", &pb::RespondRequest { request: job.id.clone(), action: "result".into(), content: "marker X: 42 ng/ml".into() }).await;
    assert!(eventually_status(&asker, &job.id, "done").await);
    let done: pb::JobsResponse = asker.ok("inbox", &pb::InboxRequest { status: "done".into(), ..Default::default() }).await;
    let d = &done.jobs[0];
    assert_eq!(d.result, "marker X: 42 ng/ml");
    assert!(!d.rated);

    // only the asker, who received the result, may rate the capability
    let rate = |event: &str| pb::RateRequest { score: 8, event: event.into(), aspect: "capability".into(), reason: "accurate and fast".into(), ..Default::default() };
    assert!(mallory.err("rate", &rate(&d.result_id)).await.1.contains("delivered a result to you"));
    assert!(asker.err("rate", &rate(&post.id)).await.1.contains("capability result"));
    let a: pb::Ack = asker.ok("rate", &rate(&d.result_id)).await;
    assert!(a.rep > 0.0);
    let after: pb::JobsResponse = asker.ok("inbox", &pb::InboxRequest::default()).await;
    assert!(after.jobs[0].rated);

    // a forged capability rating (bypassing the API) is ignored by every node
    let forged = mallory.node.publish(
        K_RATING,
        vec![
            vec!["d".into(), format!("cap:{}:assay", cap.provider)],
            vec!["p".into(), cap.provider.clone()],
            vec!["e".into(), d.result_id.clone()],
            vec!["score".into(), "-10".into()],
            vec!["aspect".into(), "capability".into()],
            vec!["capability".into(), "assay".into()],
        ],
        String::new(),
    );
    assert!(forged.is_ok());
    let forged_id = forged.unwrap().id.hex();
    assert!(eventually(T, || lab.has(&forged_id)).await);
    for n in [&lab, &asker] {
        let mut ok = false;
        for _ in 0..100 {
            let r: pb::CapabilitiesResponse = n.ok("capabilities", &pb::CapabilitiesRequest { provider: cap.provider.clone(), kind: "tool".into(), ..Default::default() }).await;
            let c = &r.capabilities[0];
            if c.ratings == 1 && c.rep > 0.0 && c.completed == 1 && c.requests == 1 {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(ok, "capability reputation agrees on both nodes and ignores the forgery");
    }
    // capability reputation is not community reputation
    let lb: pb::ReputationResponse = lab.ok("reputation", &pb::ReputationRequest { community: "onco".into(), pubkey: cap.provider.clone() }).await;
    assert_eq!(lb.entries[0].rep, 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn providers_may_decline() {
    let (lab, asker) = (spawn(|_| {}).await, spawn(|_| {}).await);
    asker.connect(&lab).await;
    lab.ok::<_, pb::Ack>("set_capability", &skill("pathology")).await;
    let pk = lab.node.keys.pubkey.hex();
    assert!(eventually(T, || asker.node.store.lock().unwrap().replaceable(&lab.node.keys.pubkey, K_CAPABILITY, "pathology").is_some()).await);
    let req = |input: &str| pb::JobRequest { provider: pk.clone(), capability: "pathology".into(), input: input.into(), ..Default::default() };
    assert!(asker.err("request", &req("  ")).await.1.contains("describe what you need"));
    let job: pb::Ack = asker.ok("request", &req("Could you review slide 4?")).await;
    assert!(eventually(T, || lab.has(&job.id)).await);
    lab.ok::<_, pb::Ack>("respond", &pb::RespondRequest { request: job.id.clone(), action: "decline".into(), content: "no capacity this week".into() }).await;
    assert!(eventually_status(&asker, &job.id, "declined").await);
    let r: pb::JobsResponse = asker.ok("inbox", &pb::InboxRequest::default()).await;
    assert_eq!(r.jobs[0].status_text, "no capacity this week");
    let caps: pb::CapabilitiesResponse = asker.ok("capabilities", &pb::CapabilitiesRequest { provider: pk.clone(), ..Default::default() }).await;
    assert_eq!((caps.capabilities[0].requests, caps.capabilities[0].declined, caps.capabilities[0].completed), (1, 1, 0));
    assert!(lab.err("inbox", &pb::InboxRequest { role: "boss".into(), ..Default::default() }).await.1.contains("role"));
}

async fn eventually_status(n: &TestNode, job: &str, want: &str) -> bool {
    for _ in 0..200 {
        let r: pb::JobsResponse = n.ok("inbox", &pb::InboxRequest::default()).await;
        if r.jobs.iter().any(|j| j.id == job && j.status == want) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}
