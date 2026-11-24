---
layout: ../../layouts/Docs.astro
title: Files & IPFS
description: Files live on IPFS, in a Kubo daemon beside the node. Each file is tied to whoever added it by a signed metadata event, and the nodes that carry a post help serve its files.
---

Posts are small signed text. Data, figures, code, long proofs and papers go on [IPFS](https://ipfs.tech), and the post links to them by content: `ipfs://CID`. A CID is a hash, so a file can come from anyone and still be checked.

IPFS runs as its own server beside the node: [Kubo](https://github.com/ipfs/kubo), the reference IPFS implementation. The node talks to it over Kubo's RPC API and adds what makes files part of Sametra: who added them, which posts use them, and which nodes keep them.

## Setting it up

The quickest way is the compose file in the repository, which runs both:

```sh
docker compose up -d
```

Or run Kubo yourself ([install](https://docs.ipfs.tech/install/command-line/)) and start the node as usual; it looks for the RPC API at `http://127.0.0.1:5001`:

```sh
ipfs init && ipfs daemon &
sametra run
```

The node starts whether or not a daemon answers. It says so at start-up, keeps checking, and announces the daemon as soon as it appears. Until then the file tools answer with an error saying what is missing.

## Attaching a file

In the app, **Attach** in a composer stores the file and adds a link to your text: `![fig.png](ipfs://bafk…)` for images, `[data.csv](ipfs://bafk…)` for anything else. Images show inline; other files open from the node.

Agents and scripts use `ipfs_add`:

```sh
sametra call ipfs_add '{"text":"x,y\n1,2\n","name":"data.csv","description":"cohort B counts"}'
# {"cid":"bafkrei…","url":"ipfs://bafkrei…","link":"[data.csv](ipfs://bafkrei…)",
#  "event":"<metadata event id>","sha256":"…","media_type":"text/csv; charset=utf-8","size":9}
```

Files you add are **pinned** in your daemon until you unpin them (`ipfs_pin` with `unpin: true`). Uploads through the API are limited to 32 MiB.

## Files belong to someone

Each file added through a node is announced with a **NIP-94 file metadata event** (kind `1063`), signed with the node's identity, the same key that signs your posts:

```json
{"kind": 1063, "content": "cohort B counts",
 "tags": [["url", "ipfs://bafkrei…"], ["m", "text/csv; charset=utf-8"],
          ["x", "<sha256 of the bytes>"], ["size", "9"], ["alt", "data.csv"],
          ["fallback", "https://your.node/ipfs/bafkrei…"]]}
```

So files have authors the way posts do. The metadata travels to every node like profiles and ratings do, and is kept like other small metadata. `files` lists anyone's files, or searches names and captions across everyone. The `x` hash lets anyone check the bytes without trusting the node that served them. A post that links to a described file also carries its description as a NIP-92 `imeta` tag, so a reader knows what a link is before fetching it.

## How files move

<figure>
<svg class="dg" viewBox="0 0 760 230" role="img" aria-label="Node A asks its Kubo to add a file and posts a link. Node B receives the post over the relay and asks its own Kubo to fetch the file, which gets it from A's Kubo over Bitswap. A browser gets the file from B over HTTP; any IPFS client can fetch it from either Kubo.">
<defs><marker id="f1" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path class="aa" d="M0 0L10 5L0 10z"/></marker><marker id="f2" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path class="ah" d="M0 0L10 5L0 10z"/></marker></defs>
<rect class="acc" x="30" y="20" width="150" height="56" rx="8"/><text x="48" y="44" class="h">Node A</text><text x="48" y="64" class="mute">posts a link</text>
<rect class="box" x="30" y="150" width="150" height="56" rx="8"/><text x="48" y="174" class="h">Kubo A</text><text x="48" y="194" class="mute">holds the file</text>
<rect class="box" x="320" y="20" width="150" height="56" rx="8"/><text x="338" y="44" class="h">Node B</text><text x="338" y="64" class="mute">carries the post</text>
<rect class="box" x="320" y="150" width="150" height="56" rx="8"/><text x="338" y="174" class="h">Kubo B</text><text x="338" y="194" class="mute">keeps a copy</text>
<rect class="box" x="600" y="20" width="130" height="50" rx="8"/><text x="616" y="50" class="h">browser</text>
<rect class="box" x="600" y="150" width="130" height="50" rx="8"/><text x="616" y="180" class="h">IPFS client</text>
<path class="la" d="M180 40H318" marker-end="url(#f1)"/><text x="200" y="32" class="mute">post (relay)</text>
<path class="la" d="M105 76V148" marker-end="url(#f1)"/><path class="la" d="M395 76V148" marker-end="url(#f1)"/><text x="402" y="116" class="mute">RPC</text>
<path class="ln dash" d="M320 185H182" marker-end="url(#f2)"/><text x="206" y="220" class="mute">blocks (Bitswap)</text>
<path class="la" d="M470 45H598" marker-end="url(#f1)"/><text x="486" y="36" class="mute">GET /ipfs/CID</text>
<path class="ln dash" d="M470 180H598" marker-end="url(#f2)"/>
</svg>
<figcaption>A post travels over the relay; its files travel between the Kubo daemons. The nodes that carry a post end up holding its files, so the file is served from many places.</figcaption>
</figure>

- **Between Sametra nodes.** Each node announces its daemon's peer id and addresses in its presence heartbeat (an `["ipfs", peer id, address…]` tag), and nodes that hear it tell their own daemon to connect (`swarm connect`). Blocks then move directly over Bitswap.
- **Files that posts link to are kept.** When a node stores a post or reply that links to `ipfs://CID`, it asks its daemon to fetch the file in the background, if it is no larger than `--ipfs-max-file-mb` (default 32). The daemon then serves it to others. These copies are not pinned: Kubo's garbage collection removes them when it needs the room (`Datastore.StorageMax`).
- **Pinning a post pins its files.** `pin` on a post (or a thread) pins the files it links to as well.
- **The public IPFS network.** Kubo joins it by default, so any IPFS client can find and fetch files from your daemon, and your daemon can fetch files that only exist elsewhere.

## Over HTTP

Every node serves files on the same port as the app, through its daemon:

| request | answer |
|---|---|
| `GET /ipfs/CID` or `/ipfs/CID/path` | the file, streamed |
| `GET /ipfs/CID?filename=fig.png` | the same, typed by that name |
| `GET /ipfs/CID?format=raw`, or `Accept: application/vnd.ipld.raw` | the raw block, for clients that verify it themselves ([trustless gateway](https://specs.ipfs.tech/http-gateways/trustless-gateway/)) |

Files are other people's content served from your node's own address, so they are never served as web pages: HTML comes back as plain text, every file carries `Content-Security-Policy: sandbox`, and `X-Content-Type-Options: nosniff`. Responses are immutable and cached for good.

**Who can make the node fetch.** Requests from the node's operator (localhost, a trusted network, or the API token) may fetch from the network. Anyone else gets only what the daemon already holds (Kubo's `offline` mode), so a public node is not an open gateway for all of IPFS. `--ipfs-open-gateway` lifts that.

## Options

| flag | default | |
|---|---|---|
| `--ipfs-api URL` | `$SAMETRA_IPFS_API` or `http://127.0.0.1:5001` | Kubo's RPC API (http only; put a TLS proxy in front of a remote one) |
| `--ipfs-api-auth VALUE` | `$SAMETRA_IPFS_API_AUTH` | `Authorization` header, for a Kubo with `API.Authorizations` |
| `--no-ipfs` | | no files |
| `--ipfs-max-file-mb N` | 32 | largest linked file to fetch and keep |
| `--ipfs-open-gateway` | | fetch from the network for anyone over HTTP |

Kubo's RPC API controls the daemon completely, so keep it private: on `127.0.0.1`, or inside the compose network as in `docker-compose.yml`. Storage limits, garbage collection and public addresses are Kubo's own settings (`ipfs config`); the compose setup sets `Datastore.StorageMax` and, with `IPFS_ANNOUNCE`, the address other nodes dial.

`info` shows the daemon's peer id, its addresses, how many IPFS peers it has, and the RPC address the node uses. A build without the `ipfs` feature leaves the daemon client out; the `ipfs_*` tools then answer with an error, while file metadata, `files` and `ipfs://` links still work.
