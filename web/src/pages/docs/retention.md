---
layout: ../../layouts/Docs.astro
title: Retention & pinning
description: Why posts fade when their author leaves, and how pinning keeps the work that matters.
---

Sametra has no permanent server, so "how long does a post live?" has a precise answer: as long as some node keeps it. A node keeps an event while **any** of these is true:

1. its own identity wrote it,
2. it is **pinned** on this node,
3. it is small metadata (a profile, a community definition, a rating), which every node needs,
4. its author has been seen on the network recently: any event or presence heartbeat within `--author-ttl-hours` (one week by default), **extended for popular content** (below).

Everything else is evicted. On top of that, each node caps its cache at `--max-events` (200 000 by default); when it overflows, the **least popular** unprotected events go first, older before newer, down to 90 % of the cap.

## Popular content lasts longer

Each node keeps a local, decaying usage counter for every event: being viewed (+1), fetched by a peer (+1) and returned by a search (+0.2). It halves every week without use. Citations by other posts count too, double. Together:

```
keep score = ln(1 + popularity + 2 · citations)
grace      = author TTL × min(10, 1 + keep score)
```

So a post nobody reads fades one week after its author leaves; a post that keeps being cited, read and found can stay up to ten weeks, and is the last to go when the cache is full. Popularity is local: each node only counts what it saw itself, so nobody can inflate it from outside. It is saved with the node's state.

Content that is used keeps getting refreshed, and content that is no longer used fades eventually. Only a pin keeps something for good.

## What that means in practice

- **While you are around, your posts are everywhere.** Your node announces your presence with a heartbeat every two minutes, so other nodes keep your work.
- **When you leave for good**, a week later nodes start dropping your unpinned posts. The network forgets, the way IPFS forgets unpinned content.
- **Good work survives its author.** Anyone can pin a post or a whole thread. Their node then keeps it forever and serves it to anyone who asks; other nodes find it through [remote lookups](/docs/architecture#reaching-the-rest-of-the-network).
- **Your own posts never leave your node.** Delete them if you want them gone everywhere.

## Pinning

In the app, press **pin** on any post: the node pins it together with all of its replies, fetching replies it does not have yet from its peers. **unpin** releases it. Through the API, `pin` and `unpin` take an `id` and `thread: true|false`.

Pins are stored in `state.pb` and survive restarts. A pin is a decision of *your* node only; nobody else can see or change it.

## Deleting

`delete` publishes a NIP-09 deletion event signed by you. Every node that receives it removes the target, drops any pin on it, and refuses to accept the original again if it comes back through gossip. Only the author can delete; a deletion signed by anyone else is ignored.

> Deletion is a request, not a guarantee. A node that saw your post and is running software that ignores deletions can still keep a copy. Treat anything you post as public forever.

## Evicted means evicted

Evictions are recorded in the log as tombstones, so an evicted event stays gone after a restart instead of being replayed from the log. A node that needs it again can always fetch it from a peer that still has it.
