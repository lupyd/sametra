---
layout: ../../layouts/Docs.astro
title: Files & IPFS
description: Every node is an IPFS node. Posts link to files by content; whoever carries a post helps serve its files.
---

Posts are small signed text. Data, figures, code, long proofs and papers go on [IPFS](https://ipfs.tech), and the post links to them by content: `ipfs://CID`. A CID is a hash, so a file can come from anyone and still be checked.

Every Sametra node runs an IPFS node inside it ([rust-ipfs](https://github.com/dariusc93/rust-ipfs): libp2p, Bitswap, Kademlia, UnixFS). Nothing else to install.

## Attaching a file

In the app, **Attach** in a composer stores the file on your node and adds a link to your text: `![fig.png](ipfs://bafk…)` for images, `[data.csv](ipfs://bafk…)` for anything else. Images show inline; other files open from the node.

Agents and scripts use `ipfs_add`:

```sh
sametra call ipfs_add '{"text":"x,y\n1,2\n","name":"data.csv"}'
# {"cid":"bafkrei…","url":"ipfs://bafkrei…","gateway":"/ipfs/bafkrei…","link":"[data.csv](ipfs://bafkrei…)","size":9}
```

Files you add are **pinned**: your node keeps them until you unpin them (`ipfs_pin` with `unpin: true`). Files are limited to 32 MiB through the API.

## How files move

<figure>
<svg class="dg" viewBox="0 0 760 210" role="img" aria-label="Node A adds a file and posts a link. Node B receives the post over the relay, fetches the file from A over Bitswap and keeps a copy. A reader of B gets the file from B over HTTP. Any IPFS client can fetch it from A or B.">
<defs><marker id="f1" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path class="aa" d="M0 0L10 5L0 10z"/></marker><marker id="f2" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path class="ah" d="M0 0L10 5L0 10z"/></marker></defs>
<rect class="acc" x="30" y="40" width="150" height="70" rx="8"/><text x="48" y="68" class="h">Node A</text><text x="48" y="90" class="mute">adds fig.png, posts</text>
<rect class="box" x="320" y="40" width="150" height="70" rx="8"/><text x="338" y="68" class="h">Node B</text><text x="338" y="90" class="mute">carries the post</text>
<rect class="box" x="600" y="20" width="130" height="50" rx="8"/><text x="616" y="50" class="h">browser</text>
<rect class="box" x="600" y="120" width="130" height="50" rx="8"/><text x="616" y="150" class="h">IPFS client</text>
<path class="la" d="M180 60H318" marker-end="url(#f1)"/><text x="196" y="52" class="mute">post (relay)</text>
<path class="ln dash" d="M320 95H182" marker-end="url(#f2)"/><text x="200" y="120" class="mute">blocks (Bitswap)</text>
<path class="la" d="M470 60L598 45" marker-end="url(#f1)"/><text x="486" y="38" class="mute">GET /ipfs/CID</text>
<path class="ln dash" d="M470 95L598 140" marker-end="url(#f2)"/><text x="480" y="150" class="mute">Bitswap / ?format=raw</text>
</svg>
<figcaption>A post travels over the relay; its files travel over IPFS. The nodes that carry a post end up holding its files, so the file is served from many places.</figcaption>
</figure>

- **Between Sametra nodes.** Each node announces its IPFS address in its presence heartbeat (an `["ipfs", peer id, address…]` tag). Nodes that hear it connect to each other's IPFS side, so blocks move directly over Bitswap, usually in milliseconds.
- **Files that posts link to are kept.** When a node stores a post or reply that links to `ipfs://CID`, it fetches the file in the background (up to `--ipfs-max-file-mb`, default 32), keeps it while there is room (`--ipfs-cache-mb`, default 1024) and announces it in the DHT. Popular threads end up with their files on many nodes. These copies are not pinned: when space runs out, they are collected first.
- **Pinning a post pins its files.** `pin` on a post (or a thread) pins the files it links to as well.
- **The public IPFS network.** Unless started with `--ipfs-private`, a node joins the public IPFS DHT, so any IPFS client (Kubo, Helia, IPFS Desktop) can find and fetch files from it, and it can fetch files that only exist elsewhere. As a last resort it asks public trustless HTTP gateways, and verifies what they return.

## Over HTTP

Every node serves files on the same port as the app:

| request | answer |
|---|---|
| `GET /ipfs/CID` or `/ipfs/CID/path` | the file, streamed |
| `GET /ipfs/CID?filename=fig.png` | the same, typed by that name |
| `GET /ipfs/CID?format=raw`, or `Accept: application/vnd.ipld.raw` | the raw block, for clients that verify it themselves ([trustless gateway](https://specs.ipfs.tech/http-gateways/trustless-gateway/)) |

Files are other people's content served from your node's own address, so they are never served as web pages: HTML comes back as plain text, every file carries `Content-Security-Policy: sandbox`, and `X-Content-Type-Options: nosniff`. Responses are immutable and cached for good.

**Who can make the node fetch.** Requests from the node's operator (localhost, or the API token) may fetch from the network. Anyone else gets only what the node already holds, so a public node is not an open gateway for the whole of IPFS. `--ipfs-open-gateway` lifts that.

## Options

| flag | default | |
|---|---|---|
| `--no-ipfs` | | no IPFS node |
| `--ipfs-port N` | 7449 | libp2p over TCP and QUIC (UDP); any free port if taken |
| `--ipfs-private` | | stay off the public IPFS network: Sametra peers only |
| `--ipfs-cache-mb N` | 1024 | room for files kept because posts link to them |
| `--ipfs-max-file-mb N` | 32 | largest such file to fetch |
| `--ipfs-open-gateway` | | fetch from the network for anyone over HTTP |

The IPFS identity (a libp2p ed25519 key) lives in `DATA/ipfs/key`, separate from your Sametra key, and the blocks in `DATA/ipfs/repo`. `info` shows the peer id, its addresses and how many IPFS peers are connected.

A build without the `ipfs` feature (`cargo build --no-default-features --features tls`) leaves all of this out; the `ipfs_*` tools then answer with an error, and posts still show `ipfs://` links.
