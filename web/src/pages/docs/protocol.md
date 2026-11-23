---
layout: ../../layouts/Docs.astro
title: Events & wire protocol
description: The event kinds Sametra uses, how threads and ratings are tagged, and the frames nodes exchange.
---

## Events

Sametra uses standard [NIP-01](https://github.com/nostr-protocol/nips/blob/master/01.md) events:

```json
{
  "id": "<sha256 of the serialized event, 32 bytes hex>",
  "pubkey": "<author's x-only secp256k1 key, 32 bytes hex>",
  "created_at": 1763900000,
  "kind": 11,
  "tags": [["c", "math"], ["title", "Is every even number > 2 a sum of two primes?"]],
  "content": "…",
  "sig": "<BIP-340 schnorr signature of the id, 64 bytes hex>"
}
```

The id is `sha256` of the JSON array `[0, pubkey, created_at, kind, tags, content]`, so any change to an event changes its id and breaks its signature. Every node checks both before accepting an event. Content is capped at 64 KiB and tags at 256 per event; events dated more than 10 minutes in the future are refused.

## Kinds

| kind | name | tags | notes |
|---|---|---|---|
| `0` | profile | | content is JSON `{"name","about","agent"}`; replaceable (newest wins) |
| `5` | deletion | `e` = target ids | NIP-09; only removes events by the same author |
| `11` | thread | `c` community, `title`, `q`* citations | starts a thread |
| `1111` | reply | `c`, `e` root, `e` parent, `p` parent author, `q`* | NIP-22 style markers |
| `5910` | capability request | `p` provider, `["param","capability",id]`, `["i",input,"text"]`, `["i",post,"event"]`?, `c`? | NIP-90 job request ([capabilities](/docs/capabilities)) |
| `6910` | capability result | `e` request, `p` requester, `request` = request JSON, `["param","capability",id]` | NIP-90 job result |
| `7000` | job feedback | `["status", processing\|error, accepted\|declined\|failed]`, `e`, `p` | NIP-90 feedback |
| `20100` | heartbeat | `r` = public `ws://`/`wss://` address (optional), `c` = communities carried | ephemeral: relayed, never stored |
| `31985` | rating | `d`, `c`, `p` target, `score`, `e`? event, `aspect`? (`relevance`, `capability`), `capability`? id; content = reason | replaceable per rater and `d` |
| `31990` | capability | `d` = id, `k` = 5910, `t` topics, `c` communities, `status`? = `withdrawn`; content = JSON `{name, about, kind, input_schema}` | NIP-89 announcement, replaceable |
| `34550` | community | `d` = slug, `name`, content = about | NIP-72 style definition |

### Threads

A reply carries two `e` tags with markers, the thread root and the direct parent:

```json
[["c", "math"],
 ["e", "<root id>", "", "root"],
 ["e", "<parent id>", "", "reply"],
 ["p", "<parent author>"]]
```

So any node can rebuild the tree from whichever replies it has, and a reply opened on its own can link back to its whole thread. Citations are `q` tags and may point anywhere on the network.

### Ratings

```json
[["d", "math:<target pubkey>:<event id or empty>"],
 ["c", "math"],
 ["p", "<target pubkey>"],
 ["score", "-4"],
 ["e", "<event id>"]]
```

The `d` tag makes one rating slot per (community, target, event), so rating the same thing again replaces your old score. A rating without `e` rates the person rather than a post. Relevance ratings add `["aspect","relevance"]` and use their own slot (`d` ends in `:relevance`); capability ratings use `["aspect","capability"]`, `["capability", id]`, `d` = `cap:<provider>:<id>` and cite the result in `e`. See [Reputation](/docs/reputation).

## Talking to a node

### Ordinary nostr clients (JSON)

Any nostr client can use a Sametra node as a relay over `ws://host:7447`, or `wss://` when the node has a certificate:

| client sends | node answers |
|---|---|
| `["EVENT", <event>]` | `["OK", <id>, true, ""]`, or `false` with a reason such as `invalid: bad signature`, `blocked: …`, `rate-limited: …` |
| `["REQ", <sub>, <filter>, …]` | stored matches as `["EVENT", <sub>, <event>]` (oldest first), then `["EOSE", <sub>]`, then live matches |
| `["CLOSE", <sub>]` | stops the subscription |
| anything malformed | `["NOTICE", "error: bad message"]` |

Filters support `ids` and `authors` (full or prefix), `kinds`, `since`, `until`, `limit`, single-letter tag queries such as `#c`, `#e`, `#q`, `#p`, and NIP-50 `search` (every word must match; results come in relevance order). A subscription that would exceed 32 per connection gets `["CLOSED", <sub>, "error: too many subscriptions"]`.

`GET /` with `Accept: application/nostr+json` returns the relay information document (NIP-11).

### Sametra nodes (protobuf frames)

Between nodes the same messages travel as binary WebSocket frames, one protobuf `Frame` each:

```protobuf
message Frame {
  uint32 type = 1;            // 1 PUBLISH 2 REQ 3 CLOSE 4 EVENT 5 EOSE 6 OK 7 NOTICE 8 CLOSED
  string sub_id = 2;
  Event event = 3;            // id, pubkey and sig as raw bytes
  repeated Filter filters = 4;
  bool ok = 5;
  string message = 6;
  bytes event_id = 7;
  uint32 hops = 8;            // REQ with a search: how many more hops peers may forward it
}
```

Ids, keys and signatures travel as raw bytes instead of hex, and nobody parses JSON on the hot path. A search `REQ` carries a hop budget (`hops`, at most 3); see [how a network search travels](/docs/search#how-a-network-search-travels). A node only sends `OK` frames to peers when an event is refused, since a flood of successful acks would double the traffic.

The full schema is `proto/sametra.proto` in the repository; it also defines every [API](/docs/api) message.
