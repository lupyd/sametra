---
layout: ../../layouts/Docs.astro
title: Retention & pinning
description: Posts live as long as some node keeps them. Each node decides what to forget; authors host their own work for as long as they like.
---

Sametra has no permanent server, so "how long does a post live?" has a precise answer: as long as some node keeps it. Nothing expires because its author went quiet. Content fades only when the nodes holding it forget it, and **each node decides for itself what to forget**.

## What a node never forgets

1. events its own identity wrote: **you host your own work for as long as you run a node**,
2. events **pinned** on it,
3. small metadata every node needs: profiles, ratings, capabilities, file metadata.

## What it forgets, and in which order

Everything else stays until the node needs room. Each node caps its cache at `--max-events` (200 000 by default). When the cache overflows, the node drops its lowest-ranked events down to 90 % of the cap. How it ranks is its own choice, `--keep`:

| strategy | keeps first |
|---|---|
| `balanced` (default) | standing (the post's own score, plus its author's [standing](/docs/reputation) near it) and use, minus age: `2·standing + ln(1 + popularity + 2·citations) − age in months` |
| `reputation` | standing; use only breaks ties. Well-rated posts, and work by people who stand well on its subject, stay longest |
| `popularity` | what is used and cited, minus age |
| `recent` | the newest |

A node can also forget others' unpinned posts past an age it chooses, `--forget-after-days N`. Without it, age only matters through the ranking.

**Popularity** is a local, decaying count of how often an event was viewed (+1), fetched by a peer (+1) or returned by a search (+0.2); it halves every week without use. Citations by other posts count double. Popularity is local: each node only counts what it saw itself, so nobody can inflate it from outside. It is saved with the node's state.

Because every node chooses, the network keeps what its members value: well-regarded and much-used work is held by many nodes and survives cache pressure everywhere, while unread, low-standing posts thin out. Strategies are a node's own business, so new ones can be added without anyone else changing.

## What that means in practice

- **Your posts live on your node for good**, and on every node that has room for them or values them.
- **Leaving does not erase you.** Other nodes keep your work as long as their strategy ranks it high enough. If you stop running a node, your posts last as long as others keep them.
- **Good work survives.** Anyone can pin a post or a whole thread; their node then keeps it and serves it to anyone who asks. Other nodes find it through [remote lookups](/docs/architecture#reaching-the-rest-of-the-network).
- **Your own posts never leave your node.** Delete them if you want them gone everywhere.

## Pinning

In the app, press **pin** on any post: the node pins it together with everything under it, fetching replies it does not have yet from its peers. **unpin** releases it. Through the API, `pin` and `unpin` take an `id` and `thread: true|false`.

Pins are stored in `state.pb` and survive restarts. A pin is a decision of *your* node only; nobody else can see or change it.

## Deleting

`delete` publishes a NIP-09 deletion event signed by you. Every node that receives it removes the target, drops any pin on it, and refuses to accept the original again if it comes back through gossip. Only the author can delete; a deletion signed by anyone else is ignored.

> Deletion is a request, not a guarantee. A node that saw your post and is running software that ignores deletions can still keep a copy. Treat anything you post as public forever.

## Evicted means evicted

Evictions are recorded in the log as tombstones, so an evicted event stays gone after a restart instead of being replayed from the log. A node that needs it again can fetch it from a peer that still has it.
