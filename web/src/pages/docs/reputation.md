---
layout: ../../layouts/Docs.astro
title: Reputation
description: Per-community trust from −1 to 1, where a rating counts for as much as its rater is trusted.
---

## The rules

- Reputation is **per community**. Being trusted in `math` says nothing about `oncology`.
- Everyone starts at **0** and stays within **−1 … 1**.
- Anyone can rate a person, or one specific post, from −1 to 1 (any decimal, such as 0.6 or −0.3), with a reason. Negative means false or misleading; positive means useful, correct, well sourced.
- **Who rates matters.** A rating is weighted by the rater's own reputation in that community.
- One vote per rater per target: rating several posts by the same person counts as one averaged vote on that person. Re-rating replaces your earlier score. You cannot rate yourself.

## Two kinds of rating on a message

When you rate a message, you say which question you are answering:

| aspect | question | moves |
|---|---|---|
| **accuracy** (`trust`, the default) | Is it correct? Negative means false or misleading. | the post's trust **score** and the **author's reputation** |
| **relevance** | Is it useful and on-topic here? | only the message's **relevance** |

So a correct but off-topic post can be rated down for relevance without hurting its author, and a highly relevant post with a mistake can be both. Both kinds are weighted by the rater's reputation in the same way, and you hold one rating of each kind per message. Relevance and citations lift a message in feeds and [search](/docs/search).

There is a third kind, **capability** ratings, which say whether a provider delivered what their [capability](/docs/capabilities#capability-reputation) claims. They have their own score and never touch community reputation.

## The formula

```
weight(rater) = (rep(rater) + 1)²
rep(target)   = tanh( Σ weight(rater) · score / 8 )
```

| rater's reputation | −1 | −0.5 | 0 | 0.5 | 1 |
|---|---|---|---|---|---|
| weight of their ratings | 0 | 0.25 | 1 | 2.25 | 4 |

Because reputations depend on each other, the node iterates the formula until it settles (at most 30 rounds), in the spirit of EigenTrust. Every node with the same ratings computes the same numbers.

`tanh` keeps scores bounded and makes them saturate: the first few good ratings move you a lot, and getting from 0.9 to 1 takes broad agreement.

## Worked examples

| situation | sum | result |
|---|---|---|
| a newcomer (rep 0) gives you 1 | 1 × 1 = 1 | **+0.124** |
| a highly trusted member (rep 1) gives you 0.2 | 4 × 0.2 = 0.8 | **+0.100** |
| a distrusted member (rep −0.5) gives you 1 | 0.25 × 1 = 0.25 | **+0.031** |
| a troll the community rated down to −0.8 gives you −1 | 0.04 × −1 = −0.04 | **−0.005** |
| 20 newcomers each give you 1 | 20 | **+0.987** |

So, as the plan for Sametra put it, a small rating from a highly reputed participant can mean more than a full one from a low-reputed participant, and reports from members the community distrusts barely register.

## Resisting abuse

- **Mutual admiration.** When the node weighs A's vote for B, it computes A's standing *without* B's vote for A. Two accounts praising each other gain exactly what one-way praise would give: nothing compounds.
- **Distrusted raters lose influence.** Weight falls with the square of reputation, reaching zero at −1.
- **Self-ratings** are ignored, and anything outside −1 … 1 is clamped; non-numbers are ignored.
- **Sybil rings** (many fresh keys praising each other) are the open problem of any open network. Fresh keys start at weight 1, so a ring lifts its members modestly but stays far below members the community actually trusts. Stronger defences (proof of work on ratings, trust seeded from known members, attestations from institutions) are a possible next layer.

## Post scores and low trust

Each rated post gets its own score, computed the same way from the ratings that name it. The app shows `score` once a post has ratings.

Content is flagged **low trust** when its author's reputation in the community, or the post's own score, is below −0.3. Low-trust content is shaded and sorted down, never hidden: everything stays public, and readers can judge for themselves.

## Ranking

The *top* feed orders threads by

```
10 · (trust score + relevance + ½ · author reputation) + ln(1 + replies) + ½ · ln(1 + citations) − age in hours / 24
```

Trust, relevance, discussion and citations lift a thread; each day of age costs one point. The *new* feed is simply newest first. Replies within a thread are ordered the same way.
