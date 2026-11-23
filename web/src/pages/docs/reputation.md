---
layout: ../../layouts/Docs.astro
title: Reputation
description: Per-community trust from −10 to 10, where a rating counts for as much as its rater is trusted.
---

## The rules

- Reputation is **per community**. Being trusted in `math` says nothing about `oncology`.
- Everyone starts at **0** and stays within **−10 … 10**.
- Anyone can rate a person, or one specific post, from −10 to 10, with a reason. Negative means false or misleading; positive means useful, correct, well sourced.
- **Who rates matters.** A rating is weighted by the rater's own reputation in that community.
- One vote per rater per target: rating several posts by the same person counts as one averaged vote on that person. Re-rating replaces your earlier score. You cannot rate yourself.

## The formula

```
weight(rater) = ((rep(rater) + 10) / 10)²
rep(target)   = 10 · tanh( Σ weight(rater) · score / 80 )
```

| rater's reputation | −10 | −5 | 0 | 5 | 10 |
|---|---|---|---|---|---|
| weight of their ratings | 0 | 0.25 | 1 | 2.25 | 4 |

Because reputations depend on each other, the node iterates the formula until it settles (at most 30 rounds), in the spirit of EigenTrust. Every node with the same ratings computes the same numbers.

`tanh` keeps scores bounded and makes them saturate: the first few good ratings move you a lot, and getting from 9 to 10 takes broad agreement.

## Worked examples

| situation | sum | result |
|---|---|---|
| a newcomer (rep 0) gives you 10 | 1 × 10 = 10 | **+1.24** |
| a highly trusted member (rep 10) gives you 2 | 4 × 2 = 8 | **+1.00** |
| a distrusted member (rep −5) gives you 10 | 0.25 × 10 = 2.5 | **+0.31** |
| a troll the community rated down to −8 gives you −10 | 0.04 × −10 = −0.4 | **−0.05** |
| 20 newcomers each give you 10 | 200 | **+9.87** |

So, as the plan for Sametra put it, "a 2 from a high-reputed participant may mean more than a 10 from a low-reputed one", and reports from members the community distrusts barely register.

## Resisting abuse

- **Mutual admiration.** When the node weighs A's vote for B, it computes A's standing *without* B's vote for A. Two accounts praising each other gain exactly what one-way praise would give: nothing compounds.
- **Distrusted raters lose influence.** Weight falls with the square of reputation, reaching zero at −10.
- **Self-ratings** are ignored, and so is anything outside −10 … 10 (clamped) or not a number.
- **Sybil rings** (many fresh keys praising each other) are the open problem of any open network. Fresh keys start at weight 1, so a ring lifts its members modestly but stays far below members the community actually trusts. Stronger defences (proof of work on ratings, trust seeded from known members, attestations from institutions) are a possible next layer.

## Post scores and low trust

Each rated post gets its own score, computed the same way from the ratings that name it. The app shows `score` once a post has ratings.

Content is flagged **low trust** when its author's reputation in the community, or the post's own score, is below −3. Low-trust content is shaded and sorted down, never hidden: everything stays public, and readers can judge for themselves.

## Ranking

The *top* feed orders threads by

```
post score + ½ · author reputation + ln(1 + replies) − age in hours / 24
```

Trust and discussion lift a thread; each day of age costs one point. The *new* feed is simply newest first. Replies within a thread are ordered the same way.
