//! Reputation, on a scale of -1..1, contextual and message first.
//!
//! Ratings rate messages, not people. A message's score is the weighted sum of
//! the ratings it received (each faded by its age):
//!
//!   T(m)        = Σ_r  weight(r near m) · score · decay
//!   score(m)    = tanh(T(m) / 4)
//!
//! A person has no single reputation. Their standing *near a message* comes from
//! their own rated messages, each counted by how close it is to that message:
//!
//!   rep(p near x) = tanh( Σ_{m by p} closeness(x, m) · T(m) / 8 )
//!
//! so someone strong in one area and weak in another is judged by the area at
//! hand, and fields need no names: closeness decides (see [`Closeness`]; the node
//! supplies it from embeddings, or from the post tree). A rater counts for as much
//! as their own standing near what they rate:
//!
//!   weight(r near m) = (rep(r near m) + 1)²       1 -> 4, 0 -> 1, -1 -> 0
//!
//! computed in two passes: first every vote at weight 1, then weighted by the
//! raters' standing from the first pass. When a rater's standing is taken from
//! the first pass, votes the rated author gave the rater are left out, so two
//! accounts praising each other gain nothing from it. Ratings fade with a
//! half-life of `HALF_LIFE`, so standing reflects recent work, not old history.
//! Cost: one pass over the ratings, plus per vote a look at the rater's own rated
//! messages (capped at `MAX_PER_AUTHOR`).
//!
//! Aspects (`aspect` tag): trust (default: accurate?), relevance (useful here?
//! scores only the message), human (is this a human? network-wide, see
//! [`humanity`]), capability (delivered what was claimed? see [`capability_scores`]).

use crate::event::*;
use crate::store::Ev;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A message's weighted sum that maps to tanh(1) ≈ 0.76: four average votes.
const MSG_SCALE: f64 = 4.0;
/// A person near a message: eight average votes on close messages.
const PERSON_SCALE: f64 = 8.0;
/// Humanity: eight average vouches.
const SCALE: f64 = 8.0;
/// Capability reputation saturates faster: a few satisfied requesters count.
const CAP_SCALE: f64 = 4.0;
/// Ratings lose half their weight in this many seconds (about six months).
pub const HALF_LIFE: f64 = 182.0 * 86400.0;
/// A person's most recent rated messages that count.
pub const MAX_PER_AUTHOR: usize = 512;
const ITERS: usize = 30;

/// How close two messages are, 0..1 (1: the same subject). The node supplies it.
pub trait Closeness: Send + Sync {
    fn closeness(&self, a: &H32, b: &H32) -> f64;
}

/// Everything is close: reputation without context (overall).
pub struct Everywhere;
impl Closeness for Everywhere {
    fn closeness(&self, _: &H32, _: &H32) -> f64 {
        1.0
    }
}

pub fn weight(rep: f64) -> f64 {
    let x = rep + 1.0;
    x * x
}

pub fn decay(at: u64, now: u64) -> f64 {
    0.5f64.powf(now.saturating_sub(at) as f64 / HALF_LIFE)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aspect {
    Trust,
    Relevance,
    Human,
    Capability(String),
}

pub struct Rating {
    pub rater: H32,
    pub target: H32,
    pub event: Option<H32>,
    pub score: f64,
    pub aspect: Aspect,
    pub at: u64,
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
        "human" => Aspect::Human,
        "capability" => Aspect::Capability(e.tag("capability")?.to_string()),
        _ => return None,
    };
    let event = e.tag("e").and_then(H32::from_hex);
    match aspect {
        // trust and relevance rate a message; capability ratings cite a delivered result
        Aspect::Trust | Aspect::Relevance | Aspect::Capability(_) if event.is_none() => return None,
        // humanity is about the person, never a message
        Aspect::Human if event.is_some() => return None,
        _ => {}
    }
    Some(Rating { rater: e.pubkey, target, event, score: score.clamp(-1.0, 1.0), aspect, at: e.created_at })
}

/// One vote on a message: (rater, score · decay).
type Vote = (H32, f64);

/// Message scores and contextual standing of people.
pub struct Model {
    /// message -> (weighted trust sum T, raters)
    pub(crate) event: HashMap<H32, (f64, usize)>,
    pub(crate) relevance: HashMap<H32, (f64, usize)>,
    /// author -> their rated messages (message, T), most recent last
    pub(crate) by_author: HashMap<H32, Vec<(H32, f64)>>,
    /// author -> distinct people who rated their messages
    raters: HashMap<H32, usize>,
    close: Arc<dyn Closeness>,
}

impl Default for Model {
    fn default() -> Model {
        Model { event: HashMap::new(), relevance: HashMap::new(), by_author: HashMap::new(), raters: HashMap::new(), close: Arc::new(Everywhere) }
    }
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Model").field("messages", &self.event.len()).field("people", &self.by_author.len()).finish()
    }
}

impl Model {
    /// The message's trust score.
    pub fn event(&self, id: &H32) -> f64 {
        self.event.get(id).map(|x| (x.0 / MSG_SCALE).tanh()).unwrap_or(0.0)
    }
    pub fn event_raters(&self, id: &H32) -> usize {
        self.event.get(id).map(|x| x.1).unwrap_or(0)
    }
    pub fn relevance(&self, id: &H32) -> f64 {
        self.relevance.get(id).map(|x| (x.0 / MSG_SCALE).tanh()).unwrap_or(0.0)
    }
    pub fn relevance_raters(&self, id: &H32) -> usize {
        self.relevance.get(id).map(|x| x.1).unwrap_or(0)
    }
    /// A person's standing near message `at`.
    pub fn near(&self, pk: &H32, at: &H32) -> f64 {
        let Some(ms) = self.by_author.get(pk) else { return 0.0 };
        let sum: f64 = ms.iter().map(|(m, t)| self.close.closeness(at, m) * t).sum();
        (sum / PERSON_SCALE).tanh()
    }
    /// A person's standing over all their rated messages alike.
    pub fn overall(&self, pk: &H32) -> f64 {
        let sum: f64 = self.by_author.get(pk).map(|ms| ms.iter().map(|x| x.1).sum()).unwrap_or(0.0);
        (sum / PERSON_SCALE).tanh()
    }
    /// How many people rated this person's messages.
    pub fn raters(&self, pk: &H32) -> usize {
        self.raters.get(pk).copied().unwrap_or(0)
    }
    /// Their best-rated messages, best first.
    pub fn best(&self, pk: &H32, n: usize) -> Vec<H32> {
        let mut ms = self.by_author.get(pk).cloned().unwrap_or_default();
        ms.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        ms.into_iter().filter(|x| x.1 > 0.0).take(n).map(|x| x.0).collect()
    }
    /// Everyone with rated messages.
    pub fn people(&self) -> impl Iterator<Item = &H32> {
        self.by_author.keys()
    }
}

/// Message scores and standing from `ratings`. `author_of` gives a message's
/// author (ratings of messages this node does not hold, or naming the wrong
/// author, are left out); `close` measures how close two messages are.
pub fn compute(ratings: &[Ev], author_of: impl Fn(&H32) -> Option<H32>, close: Arc<dyn Closeness>, now: u64) -> Model {
    // one vote per (rater, message, aspect): the mean of their ratings, faded by age
    let mut trust: HashMap<H32, HashMap<H32, (f64, usize)>> = HashMap::new();
    let mut rel: HashMap<H32, HashMap<H32, (f64, usize)>> = HashMap::new();
    let mut author: HashMap<H32, H32> = HashMap::new();
    // when each message was last rated: the cap keeps the most recent
    let mut last: HashMap<H32, u64> = HashMap::new();
    for r in ratings.iter().filter_map(|e| parse_rating(e)) {
        let into = match r.aspect {
            Aspect::Trust => &mut trust,
            Aspect::Relevance => &mut rel,
            _ => continue,
        };
        let m = r.event.unwrap();
        let Some(a) = author_of(&m).filter(|a| *a == r.target) else { continue };
        author.insert(m, a);
        let l = last.entry(m).or_default();
        *l = (*l).max(r.at);
        let v = into.entry(m).or_default().entry(r.rater).or_insert((0.0, 0));
        v.0 += r.score * decay(r.at, now);
        v.1 += 1;
    }
    let mean = |m: HashMap<H32, HashMap<H32, (f64, usize)>>| -> HashMap<H32, Vec<Vote>> {
        m.into_iter()
            .map(|(id, votes)| {
                let mut v: Vec<Vote> = votes.into_iter().map(|(r, (s, n))| (r, s / n as f64)).collect();
                v.sort_by_key(|x| x.0);
                (id, v)
            })
            .collect()
    };
    let (trust, rel) = (mean(trust), mean(rel));

    // pass 1: every vote at weight 1
    let mut by_author0: HashMap<H32, Vec<(H32, &Vec<Vote>)>> = HashMap::new();
    for (m, votes) in &trust {
        by_author0.entry(author[m]).or_default().push((*m, votes));
    }
    for ms in by_author0.values_mut() {
        ms.sort_by_key(|x| (last[&x.0], x.0));
        let n = ms.len();
        ms.drain(..n.saturating_sub(MAX_PER_AUTHOR));
    }
    // a rater's standing near `m`, leaving out what `exclude` gave them
    let standing0 = |rater: &H32, m: &H32, exclude: &H32| -> f64 {
        let Some(ms) = by_author0.get(rater) else { return 0.0 };
        let sum: f64 = ms
            .iter()
            .map(|(m2, votes)| {
                let t: f64 = votes.iter().filter(|(r, _)| r != exclude).map(|x| x.1).sum();
                close.closeness(m, m2) * t
            })
            .sum();
        (sum / PERSON_SCALE).tanh()
    };
    // pass 2: weighted by the raters' standing near what they rate
    let weighted = |votes: &HashMap<H32, Vec<Vote>>| -> HashMap<H32, (f64, usize)> {
        votes
            .iter()
            .map(|(m, vs)| {
                let a = author[m];
                let t: f64 = vs.iter().map(|(r, s)| weight(standing0(r, m, &a)) * s).sum();
                (*m, (t, vs.len()))
            })
            .collect()
    };
    let event = weighted(&trust);
    let relevance = weighted(&rel);

    let mut by_author: HashMap<H32, Vec<(H32, f64)>> = HashMap::new();
    let mut raters: HashMap<H32, HashSet<H32>> = HashMap::new();
    for (m, (t, _)) in &event {
        by_author.entry(author[m]).or_default().push((*m, *t));
        raters.entry(author[m]).or_default().extend(trust[m].iter().map(|v| v.0));
    }
    for ms in by_author.values_mut() {
        ms.sort_by_key(|x| (last[&x.0], x.0));
        let n = ms.len();
        ms.drain(..n.saturating_sub(MAX_PER_AUTHOR));
    }
    Model { event, relevance, by_author, raters: raters.into_iter().map(|(k, v)| (k, v.len())).collect(), close }
}

// ---- humanity ----------------------------------------------------------------

/// Network-wide per-person scores (humanity).
#[derive(Default, Debug)]
pub struct RepTable {
    pub user: HashMap<H32, f64>,
    pub raters: HashMap<H32, usize>,
}

impl RepTable {
    pub fn user(&self, pk: &H32) -> f64 {
        self.user.get(pk).copied().unwrap_or(0.0)
    }
}

fn squash(sum: f64) -> f64 {
    (sum / SCALE).tanh()
}

/// The weighted fixed point for person-level vouching: one averaged vote per
/// (rater, target), weighted by the rater's own score, without the target's vote
/// for the rater.
fn fixed_point(votes: &[(H32, H32, f64)]) -> HashMap<H32, f64> {
    let mut rep: HashMap<H32, f64> = HashMap::new();
    let mut sum: HashMap<H32, f64> = HashMap::new();
    let mut contrib: HashMap<(H32, H32), f64> = HashMap::new();
    for _ in 0..ITERS {
        let mut next_sum: HashMap<H32, f64> = HashMap::new();
        let mut next_contrib = HashMap::with_capacity(votes.len());
        for (rater, target, s) in votes {
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
        if delta < 1e-5 {
            break;
        }
    }
    rep
}

/// Humanity: is this participant a human? Network-wide, from `human` ratings:
/// participants others verified as human vouch with more weight, those judged
/// to be bots with less.
pub fn humanity(ratings: &[Ev]) -> RepTable {
    let mut v: HashMap<(H32, H32), (f64, usize)> = HashMap::new();
    for r in ratings.iter().filter_map(|e| parse_rating(e)).filter(|r| r.aspect == Aspect::Human) {
        let e = v.entry((r.rater, r.target)).or_insert((0.0, 0));
        e.0 += r.score;
        e.1 += 1;
    }
    let votes: Vec<(H32, H32, f64)> = v.into_iter().map(|((a, b), (s, n))| (a, b, s / n as f64)).collect();
    let mut t = RepTable { user: fixed_point(&votes), ..Default::default() };
    for (_, target, _) in &votes {
        *t.raters.entry(*target).or_insert(0) += 1;
    }
    t
}

/// Capability reputation: per (provider, capability id), from capability ratings
/// that `valid` accepts (the caller checks each cites a result the provider
/// delivered to the rater), each weighted by `weight_of(rating)`.
pub fn capability_scores(ratings: &[Ev], valid: impl Fn(&Rating) -> bool, weight_of: impl Fn(&Rating) -> f64) -> HashMap<(H32, String), (f64, usize)> {
    let mut votes: HashMap<(H32, String, H32), (f64, usize, f64)> = HashMap::new();
    for r in ratings.iter().filter_map(|e| parse_rating(e)).filter(|r| matches!(r.aspect, Aspect::Capability(_))) {
        if !valid(&r) {
            continue;
        }
        let Aspect::Capability(c) = &r.aspect else { continue };
        let w = weight_of(&r);
        let v = votes.entry((r.target, c.clone(), r.rater)).or_insert((0.0, 0, 0.0));
        v.0 += r.score;
        v.1 += 1;
        v.2 = v.2.max(w);
    }
    let mut out: HashMap<(H32, String), (f64, usize)> = HashMap::new();
    for ((target, cap, _), (sum, n, w)) in votes {
        let o = out.entry((target, cap)).or_insert((0.0, 0));
        o.0 += w * sum / n as f64;
        o.1 += 1;
    }
    out.into_iter().map(|(k, (s, n))| (k, ((s / CAP_SCALE).tanh(), n))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    /// Messages in "areas": closeness 1 within an area, `far` across.
    struct Areas(HashMap<H32, u8>, f64);
    impl Closeness for Areas {
        fn closeness(&self, a: &H32, b: &H32) -> f64 {
            if self.0.get(a) == self.0.get(b) { 1.0 } else { self.1 }
        }
    }

    struct World {
        authors: HashMap<H32, H32>,
        areas: HashMap<H32, u8>,
        ratings: Vec<Ev>,
        n: u8,
    }
    impl World {
        fn new() -> World {
            World { authors: HashMap::new(), areas: HashMap::new(), ratings: vec![], n: 0 }
        }
        fn msg(&mut self, author: &Keys, area: u8) -> H32 {
            self.n += 1;
            let id = H32([self.n; 32]);
            self.authors.insert(id, author.pubkey);
            self.areas.insert(id, area);
            id
        }
        fn rate(&mut self, k: &Keys, m: &H32, score: f64) {
            self.rate_aged(k, m, score, NOW, "trust");
        }
        fn rate_aged(&mut self, k: &Keys, m: &H32, score: f64, at: u64, aspect: &str) {
            let target = self.authors[m];
            let tags = vec![
                vec!["d".into(), format!("{}:{aspect}", m.hex())],
                vec!["p".into(), target.hex()],
                vec!["e".into(), m.hex()],
                vec!["score".into(), score.to_string()],
                vec!["aspect".into(), aspect.into()],
            ];
            self.ratings.push(Arc::new(k.sign_at(K_RATING, tags, String::new(), at)));
        }
        fn model(&self, far: f64) -> Model {
            compute(&self.ratings, |m| self.authors.get(m).copied(), Arc::new(Areas(self.areas.clone(), far)), NOW)
        }
    }

    #[test]
    fn standing_depends_on_the_area() {
        // a physicist who is wrong about maths: strong near physics, weak near maths
        let mut w = World::new();
        let (phys, rater) = (Keys::generate(), Keys::generate());
        let p: Vec<H32> = (0..4).map(|_| w.msg(&phys, 1)).collect();
        let m: Vec<H32> = (0..4).map(|_| w.msg(&phys, 2)).collect();
        for x in &p {
            w.rate(&rater, x, 1.0);
        }
        for x in &m {
            w.rate(&rater, x, -1.0);
        }
        let model = w.model(0.0);
        assert!(model.near(&phys.pubkey, &p[0]) > 0.3, "{}", model.near(&phys.pubkey, &p[0]));
        assert!(model.near(&phys.pubkey, &m[0]) < -0.3);
        assert!(model.overall(&phys.pubkey).abs() < 1e-9, "overall they cancel out; in context they do not");
        assert_eq!(model.raters(&phys.pubkey), 1);
        assert_eq!(model.best(&phys.pubkey, 2).len(), 2);
        assert!(model.best(&phys.pubkey, 10).iter().all(|b| p.contains(b)), "only well-rated messages are their best");
    }

    #[test]
    fn raters_count_for_their_standing_near_what_they_rate() {
        let mut w = World::new();
        let elders: Vec<Keys> = (0..6).map(|_| Keys::generate()).collect();
        let (expert, crank, author) = (Keys::generate(), Keys::generate(), Keys::generate());
        // the expert is respected in area 1; the crank is not
        let e1 = w.msg(&expert, 1);
        let c1 = w.msg(&crank, 1);
        for el in &elders {
            w.rate(el, &e1, 1.0);
            w.rate(el, &c1, -1.0);
        }
        let (good, bad) = (w.msg(&author, 1), w.msg(&author, 1));
        w.rate(&expert, &good, 0.3);
        w.rate(&crank, &bad, 1.0);
        let model = w.model(0.0);
        assert!(model.event(&good) > model.event(&bad), "{} vs {}", model.event(&good), model.event(&bad));
        // in another area the expert's word is an ordinary vote
        let other = w.msg(&author, 2);
        w.rate(&expert, &other, 0.3);
        let model = w.model(0.0);
        assert!(model.event(&other) < model.event(&good));
    }

    #[test]
    fn newcomers_rise_on_one_good_message() {
        let mut w = World::new();
        let (new, a, b) = (Keys::generate(), Keys::generate(), Keys::generate());
        let idea = w.msg(&new, 1);
        w.rate(&a, &idea, 1.0);
        w.rate(&b, &idea, 1.0);
        let model = w.model(0.1);
        assert!(model.event(&idea) > 0.4, "a good message stands on its own: {}", model.event(&idea));
        assert!(model.near(&new.pubkey, &idea) > 0.0);
    }

    #[test]
    fn mutual_admiration_does_not_snowball() {
        let mut w = World::new();
        let (a, b) = (Keys::generate(), Keys::generate());
        let (ma, mb) = (w.msg(&a, 1), w.msg(&b, 1));
        w.rate(&a, &mb, 1.0);
        let single = w.model(1.0).event(&mb);
        w.rate(&b, &ma, 1.0);
        let both = w.model(1.0).event(&mb);
        assert!((both - single).abs() < 1e-9, "{both} vs {single}");
    }

    #[test]
    fn the_cap_keeps_the_most_recently_rated() {
        let mut w = World::new();
        let (a, r) = (Keys::generate(), Keys::generate());
        let n = MAX_PER_AUTHOR + 10;
        let ids: Vec<H32> = (0..n)
            .map(|i| {
                let mut b = [0xAB; 32];
                b[..8].copy_from_slice(&(i as u64).to_be_bytes());
                let id = H32(b);
                w.authors.insert(id, a.pubkey);
                w.areas.insert(id, 1);
                id
            })
            .collect();
        // rated oldest first; ids sort the other way round, so order by id would keep the wrong ones
        for (i, id) in ids.iter().rev().enumerate() {
            w.rate_aged(&r, id, 1.0, NOW - (n - i) as u64, "trust");
        }
        let m = w.model(1.0);
        let kept: HashSet<H32> = m.by_author[&a.pubkey].iter().map(|x| x.0).collect();
        assert_eq!(kept.len(), MAX_PER_AUTHOR);
        assert!(kept.contains(&ids[0]) && !kept.contains(&ids[n - 1]), "the last rated stays, the first rated goes");
    }

    #[test]
    fn old_ratings_fade() {
        let mut w = World::new();
        let (a, r) = (Keys::generate(), Keys::generate());
        let (fresh, old) = (w.msg(&a, 1), w.msg(&a, 1));
        w.rate_aged(&r, &fresh, 1.0, NOW, "trust");
        w.rate_aged(&r, &old, 1.0, NOW - HALF_LIFE as u64, "trust");
        let m = w.model(1.0);
        let (f, o) = (m.event.get(&fresh).unwrap().0, m.event.get(&old).unwrap().0);
        assert!((o / f - 0.5).abs() < 1e-6, "a half-life halves it: {o} vs {f}");
    }

    #[test]
    fn relevance_scores_messages_not_people() {
        let mut w = World::new();
        let (a, r) = (Keys::generate(), Keys::generate());
        let m = w.msg(&a, 1);
        w.rate_aged(&r, &m, 0.8, NOW, "relevance");
        let model = w.model(1.0);
        assert!(model.relevance(&m) > 0.0);
        assert_eq!(model.relevance_raters(&m), 1);
        assert_eq!(model.event(&m), 0.0);
        assert_eq!(model.overall(&a.pubkey), 0.0, "relevance never moves standing");
    }

    #[test]
    fn misattributed_and_unknown_messages_are_ignored() {
        let mut w = World::new();
        let (a, b, r) = (Keys::generate(), Keys::generate(), Keys::generate());
        let m = w.msg(&a, 1);
        // a rating that names the wrong author
        let tags = vec![vec!["p".into(), b.pubkey.hex()], vec!["e".into(), m.hex()], vec!["score".into(), "1".into()]];
        w.ratings.push(Arc::new(r.sign_at(K_RATING, tags, String::new(), NOW)));
        // one of a message this node does not hold
        let tags = vec![vec!["p".into(), a.pubkey.hex()], vec!["e".into(), H32([99; 32]).hex()], vec!["score".into(), "1".into()]];
        w.ratings.push(Arc::new(r.sign_at(K_RATING, tags, String::new(), NOW)));
        let model = w.model(1.0);
        assert_eq!((model.overall(&a.pubkey), model.overall(&b.pubkey)), (0.0, 0.0));
    }

    #[test]
    fn one_vote_per_rater_and_message() {
        let mut w = World::new();
        let (a, r) = (Keys::generate(), Keys::generate());
        let m = w.msg(&a, 1);
        w.rate(&r, &m, 1.0);
        w.rate(&r, &m, 0.0);
        let model = w.model(1.0);
        assert_eq!(model.event_raters(&m), 1);
        assert!((model.event.get(&m).unwrap().0 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn bounded_and_deterministic() {
        let mut w = World::new();
        let ks: Vec<Keys> = (0..6).map(|_| Keys::generate()).collect();
        let ms: Vec<H32> = ks.iter().map(|k| w.msg(k, 1)).collect();
        for (i, k) in ks.iter().enumerate() {
            for (j, m) in ms.iter().enumerate() {
                if i != j {
                    w.rate(k, m, ((i as i32 * 3 - j as i32 * 2) % 11) as f64 / 10.0);
                }
            }
        }
        let m1 = w.model(0.5);
        w.ratings.reverse();
        let m2 = w.model(0.5);
        for (k, m) in ks.iter().zip(&ms) {
            assert!((m1.near(&k.pubkey, m) - m2.near(&k.pubkey, m)).abs() < 1e-12);
            assert!((-1.0..=1.0).contains(&m1.near(&k.pubkey, m)) && (-1.0..=1.0).contains(&m1.event(m)));
        }
        let empty = compute(&[], |_| None, Arc::new(Everywhere), NOW);
        assert_eq!((empty.event(&H32([1; 32])), empty.near(&H32([1; 32]), &H32([2; 32]))), (0.0, 0.0));
    }

    #[test]
    fn weight_curve() {
        assert_eq!(weight(1.0), 4.0);
        assert_eq!(weight(0.0), 1.0);
        assert_eq!(weight(-1.0), 0.0);
    }

    fn raw(k: &Keys, tags: &[(&str, String)]) -> Ev {
        Arc::new(k.sign(K_RATING, tags.iter().map(|(a, b)| vec![a.to_string(), b.clone()]).collect(), String::new()))
    }

    #[test]
    fn ratings_are_validated() {
        let (a, b) = (Keys::generate(), Keys::generate());
        let bp = b.pubkey.hex();
        let ev = "ab".repeat(32);
        let ok = |t: &[(&str, String)]| parse_rating(&raw(&a, t)).is_some();
        assert!(ok(&[("p", bp.clone()), ("e", ev.clone()), ("score", "0.3".into())]));
        assert!(!ok(&[("p", bp.clone()), ("score", "0.3".into())]), "trust rates a message");
        assert!(!ok(&[("p", a.pubkey.hex()), ("e", ev.clone()), ("score", "0.3".into())]), "self-rating");
        assert!(!ok(&[("p", bp.clone()), ("e", ev.clone())]), "no score");
        assert!(!ok(&[("p", bp.clone()), ("e", ev.clone()), ("score", "NaN".into())]));
        assert!(!ok(&[("p", bp.clone()), ("e", ev.clone()), ("score", "1".into()), ("aspect", "vibes".into())]));
        assert!(!ok(&[("p", bp.clone()), ("e", ev.clone()), ("score", "1".into()), ("aspect", "capability".into())]), "capability needs an id");
        assert!(!ok(&[("p", bp.clone()), ("e", ev.clone()), ("score", "1".into()), ("aspect", "human".into())]), "humanity rates a person");
        assert!(ok(&[("p", bp.clone()), ("score", "1".into()), ("aspect", "human".into())]));
        let r = parse_rating(&raw(&a, &[("p", bp), ("e", ev), ("score", "9".into())])).unwrap();
        assert_eq!(r.score, 1.0, "clamped");
    }

    fn vouch(k: &Keys, t: &Keys, s: f64) -> Ev {
        raw(k, &[("p", t.pubkey.hex()), ("score", s.to_string()), ("aspect", "human".into())])
    }

    #[test]
    fn humanity_is_vouched() {
        let verified: Vec<Keys> = (0..6).map(|_| Keys::generate()).collect();
        let (anchor, person, bot, bot_friend) = (Keys::generate(), Keys::generate(), Keys::generate(), Keys::generate());
        let mut rs: Vec<Ev> = verified.iter().map(|k| vouch(k, &anchor, 1.0)).collect();
        rs.extend(verified.iter().take(4).map(|k| vouch(k, &bot, -1.0)));
        rs.push(vouch(&anchor, &person, 0.6));
        rs.push(vouch(&bot, &bot_friend, 1.0));
        let t = humanity(&rs);
        assert!(t.user(&anchor.pubkey) > 0.5);
        assert!(t.user(&bot.pubkey) < -0.3);
        assert!(t.user(&person.pubkey) > t.user(&bot_friend.pubkey), "a bot's vouch is worth little");
        assert_eq!(t.raters[&anchor.pubkey], 6);
    }

    #[test]
    fn capability_scores_only_count_valid_weighted_votes() {
        let (provider, alice, bob, mallory) = (Keys::generate(), Keys::generate(), Keys::generate(), Keys::generate());
        let res = H32([5; 32]).hex();
        let cap = |k: &Keys, s: f64, c: &str| raw(k, &[("p", provider.pubkey.hex()), ("e", res.clone()), ("score", s.to_string()), ("aspect", "capability".into()), ("capability", c.into())]);
        let rs = vec![cap(&alice, 0.9, "ms"), cap(&alice, 0.7, "ms"), cap(&bob, 0.6, "ms"), cap(&mallory, -1.0, "ms"), cap(&bob, 0.4, "other")];
        let t = capability_scores(&rs, |r| r.rater != mallory.pubkey, |_| 1.0);
        let (s, n) = t[&(provider.pubkey, "ms".to_string())];
        assert_eq!(n, 2);
        assert!((s - (1.4f64 / CAP_SCALE).tanh()).abs() < 1e-9, "{s}");
        assert_eq!(t[&(provider.pubkey, "other".to_string())].1, 1);
        let heavy = capability_scores(&rs, |r| r.rater != mallory.pubkey, |r| if r.rater == bob.pubkey { 4.0 } else { 1.0 });
        assert!(heavy[&(provider.pubkey, "ms".to_string())].0 > s);
    }
}
