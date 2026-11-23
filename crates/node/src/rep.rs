//! Per-community reputation.
//!
//! Every participant starts at 0 on a scale of -10..10. A rating is a
//! replaceable event (kind 31985) from a rater to a target participant,
//! optionally about one specific event. Ratings are weighted by the rater's
//! own reputation in the same community, so a "2" from a well-reputed member
//! can outweigh a "10" from a distrusted one:
//!
//!   weight(r) = ((rep(r) + 10) / 10)^2            rep 10 -> 4, 0 -> 1, -10 -> 0
//!   rep(t)    = 10 · tanh( Σ_r weight(r)·score(r,t) / SCALE )
//!
//! iterated to a fixed point (a small EigenTrust-like computation). When r's
//! weight is used to rate t, t's own vote for r is excluded, so two accounts
//! praising each other cannot bootstrap each other. A rater contributes one
//! vote per target (the mean of all their ratings of it); self-ratings are
//! ignored.
//!
//! Ratings have an aspect (`aspect` tag):
//!  * trust (default): is it accurate? Feeds the target's reputation and the
//!    event's trust score.
//!  * relevance: how useful/on-topic is this message here? Scores only the
//!    message, never the author's reputation.
//!  * capability: did the provider deliver what their capability claims? Kept
//!    out of community reputation entirely (see `capability_scores`).
//!
//! Event scores of both kinds use the same rater weighting.

use crate::event::*;
use crate::store::Ev;
use std::collections::HashMap;

const SCALE: f64 = 80.0;
const ITERS: usize = 30;

#[derive(Default, Debug)]
pub struct RepTable {
    pub user: HashMap<H32, f64>,
    pub raters: HashMap<H32, usize>,
    pub event: HashMap<H32, (f64, usize)>,
    pub relevance: HashMap<H32, (f64, usize)>,
}

impl RepTable {
    pub fn user(&self, pk: &H32) -> f64 {
        self.user.get(pk).copied().unwrap_or(0.0)
    }
    pub fn event(&self, id: &H32) -> f64 {
        self.event.get(id).map(|x| x.0).unwrap_or(0.0)
    }
    pub fn relevance(&self, id: &H32) -> f64 {
        self.relevance.get(id).map(|x| x.0).unwrap_or(0.0)
    }
}

pub fn weight(rep: f64) -> f64 {
    let x = (rep + 10.0) / 10.0;
    x * x
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aspect {
    Trust,
    Relevance,
    Capability(String),
}

pub struct Rating {
    pub rater: H32,
    pub target: H32,
    pub event: Option<H32>,
    pub score: f64,
    pub aspect: Aspect,
}

pub fn parse_rating(e: &Event) -> Option<Rating> {
    if e.kind != K_RATING {
        return None;
    }
    let score: f64 = e.tag("score")?.parse().ok()?;
    let target = H32::from_hex(e.tag("p")?)?;
    if target == e.pubkey || !score.is_finite() {
        return None;
    }
    let aspect = match e.tag("aspect").unwrap_or("trust") {
        "trust" => Aspect::Trust,
        "relevance" => Aspect::Relevance,
        "capability" => Aspect::Capability(e.tag("capability")?.to_string()),
        _ => return None,
    };
    let event = e.tag("e").and_then(H32::from_hex);
    if aspect != Aspect::Trust && event.is_none() {
        return None; // relevance rates a message; capability ratings cite a delivered result
    }
    Some(Rating { rater: e.pubkey, target, event, score: score.clamp(-10.0, 10.0), aspect })
}

pub fn compute(ratings: &[Ev]) -> RepTable {
    let all: Vec<Rating> = ratings.iter().filter_map(|e| parse_rating(e)).collect();
    let (rs, rel): (Vec<Rating>, Vec<Rating>) = all.into_iter().filter(|r| !matches!(r.aspect, Aspect::Capability(_))).partition(|r| r.aspect == Aspect::Trust);
    // one vote per (rater, target)
    let mut votes: HashMap<(H32, H32), (f64, usize)> = HashMap::new();
    for r in &rs {
        let v = votes.entry((r.rater, r.target)).or_insert((0.0, 0));
        v.0 += r.score;
        v.1 += 1;
    }
    let votes: Vec<(H32, H32, f64)> = votes.into_iter().map(|((a, b), (s, n))| (a, b, s / n as f64)).collect();

    let squash = |sum: f64| 10.0 * (sum / SCALE).tanh();
    let mut rep: HashMap<H32, f64> = HashMap::new();
    let mut sum: HashMap<H32, f64> = HashMap::new(); // raw weighted sums received
    let mut contrib: HashMap<(H32, H32), f64> = HashMap::new(); // w·s each vote added last round
    for _ in 0..ITERS {
        let mut next_sum: HashMap<H32, f64> = HashMap::new();
        let mut next_contrib = HashMap::with_capacity(votes.len());
        for (rater, target, s) in &votes {
            // rater's standing without the target's own vote for the rater
            let back = contrib.get(&(*target, *rater)).copied().unwrap_or(0.0);
            let c = weight(squash(sum.get(rater).copied().unwrap_or(0.0) - back)) * s;
            *next_sum.entry(*target).or_insert(0.0) += c;
            next_contrib.insert((*rater, *target), c);
        }
        let next: HashMap<H32, f64> = next_sum.iter().map(|(k, v)| (*k, squash(*v))).collect();
        let delta = next.iter().map(|(k, v)| (v - rep.get(k).copied().unwrap_or(0.0)).abs()).fold(0.0, f64::max);
        rep = next;
        sum = next_sum;
        contrib = next_contrib;
        if delta < 1e-4 {
            break;
        }
    }

    let mut t = RepTable::default();
    for (_, target, _) in &votes {
        *t.raters.entry(*target).or_insert(0) += 1;
    }
    let mut ev: HashMap<H32, (f64, usize)> = HashMap::new();
    for r in rs.iter().filter(|r| r.event.is_some()) {
        let a = ev.entry(r.event.unwrap()).or_insert((0.0, 0));
        a.0 += weight(rep.get(&r.rater).copied().unwrap_or(0.0)) * r.score;
        a.1 += 1;
    }
    t.event = ev.into_iter().map(|(k, (n, c))| (k, (squash(n), c))).collect();
    let mut rv: HashMap<H32, (f64, usize)> = HashMap::new();
    for r in &rel {
        let a = rv.entry(r.event.unwrap()).or_insert((0.0, 0));
        a.0 += weight(rep.get(&r.rater).copied().unwrap_or(0.0)) * r.score;
        a.1 += 1;
    }
    t.relevance = rv.into_iter().map(|(k, (n, c))| (k, (squash(n), c))).collect();
    t.user = rep;
    t
}

/// Capability reputation: per (provider, capability id), from capability ratings
/// that `valid` accepts (the caller checks each cites a result the provider
/// delivered to the rater), each weighted by `weight_of(rater)`.
/// Same squashing as community reputation but on a smaller scale, so a handful
/// of satisfied requesters is already meaningful.
pub fn capability_scores(ratings: &[Ev], valid: impl Fn(&Rating) -> bool, weight_of: impl Fn(&H32) -> f64) -> HashMap<(H32, String), (f64, usize)> {
    let mut votes: HashMap<(H32, String, H32), (f64, usize)> = HashMap::new();
    for r in ratings.iter().filter_map(|e| parse_rating(e)).filter(|r| matches!(r.aspect, Aspect::Capability(_))) {
        if !valid(&r) {
            continue;
        }
        let Aspect::Capability(c) = &r.aspect else { continue };
        let v = votes.entry((r.target, c.clone(), r.rater)).or_insert((0.0, 0));
        v.0 += r.score;
        v.1 += 1;
    }
    let mut out: HashMap<(H32, String), (f64, usize)> = HashMap::new();
    for ((target, cap, rater), (sum, n)) in votes {
        let o = out.entry((target, cap)).or_insert((0.0, 0));
        o.0 += weight_of(&rater) * sum / n as f64;
        o.1 += 1;
    }
    out.into_iter().map(|(k, (s, n))| (k, (10.0 * (s / 40.0).tanh(), n))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn rate(k: &Keys, target: &H32, score: i32) -> Ev {
        Arc::new(k.sign(
            K_RATING,
            vec![
                vec!["d".into(), format!("c:{}", target.hex())],
                vec!["c".into(), "c".into()],
                vec!["p".into(), target.hex()],
                vec!["score".into(), score.to_string()],
            ],
            String::new(),
        ))
    }

    #[test]
    fn reputable_raters_count_more() {
        let elders: Vec<Keys> = (0..6).map(|_| Keys::generate()).collect();
        let (expert, shady, alice, bob) = (Keys::generate(), Keys::generate(), Keys::generate(), Keys::generate());
        let mut rs = vec![];
        for e in &elders {
            rs.push(rate(e, &expert.pubkey, 10)); // community trusts expert
        }
        for e in &elders[..4] {
            rs.push(rate(e, &shady.pubkey, -10)); // and distrusts shady
        }
        rs.push(rate(&expert, &alice.pubkey, 2)); // modest praise from a trusted member
        rs.push(rate(&shady, &bob.pubkey, 10)); // max praise from a distrusted one
        let t = compute(&rs);
        assert!(t.user(&expert.pubkey) > 5.0);
        assert!(t.user(&alice.pubkey) > t.user(&bob.pubkey), "{} vs {}", t.user(&alice.pubkey), t.user(&bob.pubkey));
        assert_eq!(t.user(&elders[0].pubkey), 0.0);
        assert!(t.user.values().all(|v| (-10.0..=10.0).contains(v)));
    }

    #[test]
    fn distrusted_raters_lose_influence() {
        let honest: Vec<Keys> = (0..6).map(|_| Keys::generate()).collect();
        let (troll, victim) = (Keys::generate(), Keys::generate());
        let mut rs = vec![];
        for h in &honest {
            rs.push(rate(h, &troll.pubkey, -10));
        }
        rs.push(rate(&troll, &victim.pubkey, -10));
        let t = compute(&rs);
        assert!(t.user(&troll.pubkey) < -5.0);
        assert!(t.user(&victim.pubkey) > -0.5, "troll's report should barely matter: {}", t.user(&victim.pubkey));
    }

    #[test]
    fn mutual_admiration_does_not_snowball() {
        let (a, b) = (Keys::generate(), Keys::generate());
        let t = compute(&[rate(&a, &b.pubkey, 10), rate(&b, &a.pubkey, 10)]);
        let single = compute(&[rate(&a, &b.pubkey, 10)]);
        assert!((t.user(&b.pubkey) - single.user(&b.pubkey)).abs() < 1e-6, "{}", t.user(&b.pubkey));
    }

    fn raw(k: &Keys, tags: &[(&str, String)]) -> Ev {
        Arc::new(k.sign(K_RATING, tags.iter().map(|(a, b)| vec![a.to_string(), b.clone()]).collect(), String::new()))
    }
    fn rate_event(k: &Keys, target: &H32, ev: &H32, score: i32) -> Ev {
        raw(k, &[("d", format!("c:{}:{}", target.hex(), ev.hex())), ("p", target.hex()), ("e", ev.hex()), ("score", score.to_string())])
    }

    #[test]
    fn weight_curve() {
        assert_eq!(weight(10.0), 4.0);
        assert_eq!(weight(0.0), 1.0);
        assert_eq!(weight(-10.0), 0.0);
        assert!(weight(5.0) > weight(4.0));
    }

    #[test]
    fn parse_rating_validation() {
        let (a, b) = (Keys::generate(), Keys::generate());
        let bp = b.pubkey.hex();
        assert!(parse_rating(&raw(&a, &[("p", bp.clone()), ("score", "3".into())])).is_some());
        assert!(parse_rating(&raw(&a, &[("p", a.pubkey.hex()), ("score", "3".into())])).is_none(), "self-rating");
        assert!(parse_rating(&raw(&a, &[("p", bp.clone())])).is_none(), "no score");
        assert!(parse_rating(&raw(&a, &[("p", bp.clone()), ("score", "NaN".into())])).is_none());
        assert!(parse_rating(&raw(&a, &[("p", bp.clone()), ("score", "inf".into())])).is_none());
        assert!(parse_rating(&raw(&a, &[("p", "zz".into()), ("score", "1".into())])).is_none());
        assert!(parse_rating(&Arc::new(a.sign(K_POST, vec![vec!["p".into(), bp.clone()], vec!["score".into(), "1".into()]], String::new()))).is_none());
        let r = parse_rating(&raw(&a, &[("p", bp.clone()), ("score", "99".into())])).unwrap();
        assert_eq!(r.score, 10.0, "clamped");
        assert_eq!(parse_rating(&raw(&a, &[("p", bp), ("score", "-2.5".into())])).unwrap().score, -2.5);
    }

    #[test]
    fn empty_and_unrated() {
        let t = compute(&[]);
        assert!(t.user.is_empty() && t.event.is_empty());
        assert_eq!(t.user(&H32([1; 32])), 0.0);
        assert_eq!(t.event(&H32([1; 32])), 0.0);
    }

    #[test]
    fn one_vote_per_rater_and_target() {
        let (a, b) = (Keys::generate(), Keys::generate());
        let (e1, e2) = (H32([1; 32]), H32([2; 32]));
        // rating two of b's posts counts as one averaged vote on b
        let twice = compute(&[rate_event(&a, &b.pubkey, &e1, 10), rate_event(&a, &b.pubkey, &e2, 0)]);
        let once = compute(&[rate(&a, &b.pubkey, 5)]);
        assert!((twice.user(&b.pubkey) - once.user(&b.pubkey)).abs() < 1e-9);
        assert_eq!(twice.raters[&b.pubkey], 1);
        // but each post keeps its own score
        assert!(twice.event(&e1) > 0.0);
        assert_eq!(twice.event(&e2), 0.0);
        assert_eq!(twice.event[&e1].1, 1);
    }

    #[test]
    fn more_raters_more_rep_but_bounded() {
        let target = Keys::generate();
        let mut prev = 0.0;
        for n in [1, 5, 20, 200] {
            let rs: Vec<Ev> = (0..n).map(|_| rate(&Keys::generate(), &target.pubkey, 10)).collect();
            let t = compute(&rs);
            let r = t.user(&target.pubkey);
            assert!(r > prev && r <= 10.0, "{n} raters -> {r}");
            assert_eq!(t.raters[&target.pubkey], n);
            prev = r;
        }
        assert!(prev > 9.9);
        let rs: Vec<Ev> = (0..200).map(|_| rate(&Keys::generate(), &target.pubkey, -10)).collect();
        assert!(compute(&rs).user(&target.pubkey) >= -10.0);
    }

    #[test]
    fn event_score_weighted_by_rater_rep() {
        let elders: Vec<Keys> = (0..6).map(|_| Keys::generate()).collect();
        let (trusted, distrusted, author) = (Keys::generate(), Keys::generate(), Keys::generate());
        let mut rs = vec![];
        for e in &elders {
            rs.push(rate(e, &trusted.pubkey, 10));
            rs.push(rate(e, &distrusted.pubkey, -10));
        }
        let (good, bad) = (H32([7; 32]), H32([8; 32]));
        rs.push(rate_event(&trusted, &author.pubkey, &good, 3));
        rs.push(rate_event(&distrusted, &author.pubkey, &bad, 10));
        let t = compute(&rs);
        assert!(t.event(&good) > t.event(&bad), "{} vs {}", t.event(&good), t.event(&bad));
    }

    fn aspect(k: &Keys, target: &H32, ev: &H32, score: i32, a: &str, cap: Option<&str>) -> Ev {
        let mut t = vec![("d", format!("{a}:{}", ev.hex())), ("p", target.hex()), ("e", ev.hex()), ("score", score.to_string()), ("aspect", a.to_string())];
        if let Some(c) = cap {
            t.push(("capability", c.to_string()));
        }
        raw(k, &t)
    }

    #[test]
    fn relevance_scores_messages_not_people() {
        let (a, b) = (Keys::generate(), Keys::generate());
        let post = H32([9; 32]);
        let t = compute(&[aspect(&a, &b.pubkey, &post, 8, "relevance", None)]);
        assert!(t.relevance(&post) > 0.0);
        assert_eq!(t.relevance[&post].1, 1);
        assert_eq!(t.user(&b.pubkey), 0.0, "relevance never moves the author's reputation");
        assert_eq!(t.event(&post), 0.0, "nor the post's trust score");
        // weighted like trust: a distrusted rater's relevance vote counts less
        let elders: Vec<Keys> = (0..6).map(|_| Keys::generate()).collect();
        let (troll, good) = (Keys::generate(), Keys::generate());
        let (p1, p2) = (H32([1; 32]), H32([2; 32]));
        let mut rs: Vec<Ev> = elders.iter().map(|e| rate(e, &troll.pubkey, -10)).collect();
        rs.extend(elders.iter().map(|e| rate(e, &good.pubkey, 10)));
        rs.push(aspect(&troll, &b.pubkey, &p1, 10, "relevance", None));
        rs.push(aspect(&good, &b.pubkey, &p2, 3, "relevance", None));
        let t = compute(&rs);
        assert!(t.relevance(&p2) > t.relevance(&p1));
    }

    #[test]
    fn aspects_are_validated() {
        let (a, b) = (Keys::generate(), Keys::generate());
        let bp = b.pubkey.hex();
        assert!(parse_rating(&raw(&a, &[("p", bp.clone()), ("score", "1".into()), ("aspect", "vibes".into())])).is_none(), "unknown aspect");
        assert!(parse_rating(&raw(&a, &[("p", bp.clone()), ("score", "1".into()), ("aspect", "relevance".into())])).is_none(), "relevance needs a message");
        assert!(parse_rating(&raw(&a, &[("p", bp.clone()), ("score", "1".into()), ("aspect", "capability".into()), ("e", "ab".repeat(32))])).is_none(), "capability needs an id");
        assert_eq!(parse_rating(&raw(&a, &[("p", bp), ("score", "1".into())])).unwrap().aspect, Aspect::Trust);
    }

    #[test]
    fn capability_scores_only_count_valid_weighted_votes() {
        let (provider, alice, bob, mallory) = (Keys::generate(), Keys::generate(), Keys::generate(), Keys::generate());
        let res = H32([5; 32]);
        let rs = vec![
            aspect(&alice, &provider.pubkey, &res, 9, "capability", Some("ms")),
            aspect(&alice, &provider.pubkey, &res, 7, "capability", Some("ms")), // same rater: averaged
            aspect(&bob, &provider.pubkey, &res, 6, "capability", Some("ms")),
            aspect(&mallory, &provider.pubkey, &res, -10, "capability", Some("ms")), // never got a result
            aspect(&bob, &provider.pubkey, &res, 4, "capability", Some("other")),
            rate(&alice, &provider.pubkey, 10), // trust ratings are not capability ratings
        ];
        let t = capability_scores(&rs, |r| r.rater != mallory.pubkey, |_| 1.0);
        let (s, n) = t[&(provider.pubkey, "ms".to_string())];
        assert_eq!(n, 2);
        assert!((s - 10.0 * (14.0f64 / 40.0).tanh()).abs() < 1e-9, "{s}");
        assert_eq!(t[&(provider.pubkey, "other".to_string())].1, 1);
        let heavy = capability_scores(&rs, |r| r.rater != mallory.pubkey, |k| if *k == bob.pubkey { 4.0 } else { 1.0 });
        assert!(heavy[&(provider.pubkey, "ms".to_string())].0 > s);
        // capability ratings stay out of community reputation
        assert!(compute(&rs[..5]).user.is_empty());
    }

    #[test]
    fn deterministic_regardless_of_order() {
        let ks: Vec<Keys> = (0..5).map(|_| Keys::generate()).collect();
        let mut rs = vec![];
        for (i, a) in ks.iter().enumerate() {
            for (j, b) in ks.iter().enumerate() {
                if i != j {
                    rs.push(rate(a, &b.pubkey, (i as i32 * 3 - j as i32 * 2) % 11));
                }
            }
        }
        let t1 = compute(&rs);
        rs.reverse();
        let t2 = compute(&rs);
        for k in &ks {
            assert!((t1.user(&k.pubkey) - t2.user(&k.pubkey)).abs() < 1e-3);
        }
    }
}
