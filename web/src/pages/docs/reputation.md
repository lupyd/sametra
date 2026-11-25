---
layout: ../../layouts/Docs.astro
title: Reputation
description: Standing is contextual. A person's reputation near a post comes from their own rated posts on close subjects, and a rating counts for as much as its rater stands there.
---

## The rules

- **Ratings rate messages, not people.** You rate a post from −1 to 1 (any decimal, such as 0.6 or −0.3), with a reason. Negative means false or misleading; positive means useful, correct, well sourced.
- **Nobody has one reputation.** A person's **standing near a post** comes from their own rated posts, each counted by how close it is to that post. Someone who is strong on one subject and weak on another is judged by the subject at hand, and no subject needs a name.
- Everyone starts at **0** and stays within **−1 … 1**.
- **Who rates matters.** A rating is weighted by the rater's own standing near the post they rate.
- **Ratings fade.** A rating loses half its weight every 182 days (about six months), so standing reflects recent work.
- One rating per rater per message and aspect: rating it again replaces your earlier score. You cannot rate yourself.

## Two kinds of rating on a message

When you rate a message, you say which question you are answering:

| aspect | question | moves |
|---|---|---|
| **accuracy** (`trust`, the default) | Is it correct? Negative means false or misleading. | the post's trust **score**, and so its **author's standing** near that subject |
| **relevance** | Is it useful and on-topic here? | only the message's **relevance** |

So a correct but off-topic post can be rated down for relevance without hurting its author, and a highly relevant post with a mistake can be both. Both kinds are weighted by the rater's standing in the same way. Relevance and citations lift a message in feeds and [search](/docs/search).

Two other kinds of rating are about something else. **Humanity** ratings vouch that a person is a human; they are network-wide, not contextual, and they decide whether a profile shows as verified, unverified or disputed. **Capability** ratings say whether a provider delivered what their [capability](/docs/capabilities#capability-reputation) claims. Neither touches standing.

## The formulas

A post's weighted trust sum adds up the ratings it received, each weighted by its rater and faded by its age:

```
T(m)            = Σ weight(rater near m) · score · decay(age)
score(m)        = tanh( T(m) / 4 )
standing(p, x)  = tanh( Σ over p's rated posts m of closeness(x, m) · T(m) / 8 )
weight(r near m) = (standing(r, m) + 1)²
decay(age)      = ½ ^ (age / 182 days)
```

| rater's standing near the post | −1 | −0.5 | 0 | 0.5 | 1 |
|---|---|---|---|---|---|
| weight of their rating | 0 | 0.25 | 1 | 2.25 | 4 |

**Closeness** says how near two posts are, from 0 to 1. It comes from the post tree, or from meaning when the node has an embeddings service; see [below](#closeness-tree-or-meaning).

Weights depend on standing, and standing depends on weighted ratings, so the node computes in **two passes**: first every rating at weight 1, giving everyone a first standing; then each rating weighted by its rater's first-pass standing near the post. Every node with the same ratings, posts and closeness computes the same numbers; ratings of posts a node does not hold are left out until it has them.

At most 512 of a person's rated posts count. **Overall** standing (shown on profiles) is the same sum with every post at closeness 1. A profile also lists a person's best-rated posts, which show where they stand highest.

`tanh` keeps everything bounded and makes it saturate: the first few good ratings move a post a lot, and getting from 0.9 to 1 takes broad agreement. A post's score saturates twice as fast as a person's standing (divisor 4 instead of 8).

## Closeness: tree or meaning

There are no fields or categories to pick from. Closeness decides what counts as "the same subject", so new and niche fields need no names.

**From the post tree** (every node, always): posts in the same tree (under the same top-level topic) have closeness 1, posts in different trees 0.1. Good work in one tree counts fully there and a tenth as much everywhere else.

**From meaning** (nodes started with `--embed-api` and `--embed-model`): the node embeds the title and text of every post it keeps, through any OpenAI-compatible `/v1/embeddings` service, such as a local Ollama with `nomic-embed-text`. Closeness is then the cosine similarity of two posts, rescaled against the node's baseline (the mean similarity of random pairs it holds, so any model works without tuning):

```
closeness(a, b) = clamp( (cos(a, b) − baseline) / (1 − baseline), 0, 1 )
                  at least 0.5 in the same tree, at least 0.02 otherwise
```

So expertise in protein folding counts in a new thread about misfolding in another tree, and counts for almost nothing in a thread on prime numbers. Posts not yet embedded fall back to the tree.

- **Each node computes its own vectors** from the text it holds. Vectors never travel on the network, so nobody can claim their post sits somewhere it does not.
- **Lightweight**: one signed byte per dimension (768 bytes a post for a 768-dimension model), in memory and in `DATA/vectors.pb`. Posts are embedded in batches of 32 in the background. If the service stops answering, the node keeps going on tree closeness and retries.
- **Nodes may differ.** Two nodes with different models, or one with and one without embeddings, can compute slightly different standing for the same person. Standing is each node's own reading of the ratings, like its retention strategy. A model change re-embeds everything.

Your profile's node panel shows which it uses, how many posts are embedded, and whether the service answers.

## Worked examples

Ana writes one post under a topic. Each row is one situation, with fresh ratings. "Near" is Ana's standing near any post in the same tree; "elsewhere" is her standing near a post in another tree (closeness 0.1).

| situation | T | post score | Ana near | Ana elsewhere |
|---|---|---|---|---|
| a newcomer (standing 0) gives it 1 | 1 × 1 = 1 | **+0.245** | **+0.124** | +0.012 |
| someone who stands at 1 near it gives it 0.2 | 4 × 0.2 = 0.8 | **+0.197** | **+0.100** | +0.010 |
| someone at −0.5 near it gives it 1 | 0.25 × 1 = 0.25 | **+0.062** | **+0.031** | +0.003 |
| someone rated down to −0.8 near it gives it −1 | 0.04 × −1 = −0.04 | **−0.010** | **−0.005** | −0.0005 |
| 20 newcomers each give it 1 | 20 | **+1.000** | **+0.987** | +0.245 |

So a small rating from someone who knows the subject can mean more than a full one from someone who does not, reports from people with poor standing there barely register, and a strong record in one tree carries only a little into another. A rating six months old counts half; a year old, a quarter.

## Resisting abuse

- **Mutual admiration.** When the node weighs A's rating of B's post, it computes A's first-pass standing *without* the ratings B gave A. Two accounts praising each other gain exactly what one-way praise would give: nothing compounds.
- **Poor standing loses influence.** Weight falls with the square of standing, reaching zero at −1, and it is the rater's standing near the post they rate: an expert elsewhere is a newcomer here.
- **Self-ratings** are ignored, ratings that name the wrong author for a post are ignored, scores outside −1 … 1 are clamped, and non-numbers are ignored.
- **Old praise fades**, so a reputation earned years ago does not carry forever.
- **Sybil rings** (many fresh keys praising each other) are the open problem of any open network. Fresh keys start at weight 1, so a ring lifts its members modestly but stays far below people who earned their standing on the subject. Stronger defences (trust seeded from known members, attestations from institutions) are a possible next layer.

## Post scores and low trust

Each rated post gets its own score, `score(m)` above. The app shows it as **accuracy** once a post has ratings, next to the author's **standing** near that post.

Content is flagged **low trust** when its author's standing near it, or the post's own score, is below −0.3. Low-trust content is shaded and sorted down, never hidden: everything stays public, and readers can judge for themselves.

On a topic's page, **Standing here** lists the people who stand highest near it among those who wrote it or under it. The `reputation` tool returns the same list, or one person's standing near any post ([API](/docs/api)).

## Ranking

The *top* feed orders posts by

```
10 · (trust score + relevance + ½ · author's standing near the post) + ln(1 + posts under it) + ½ · ln(1 + citations) − age in hours / 24
```

Trust, relevance, discussion and citations lift a post; each day of age costs one point. The *new* feed is simply newest first. Posts under a post are ordered the same way.
