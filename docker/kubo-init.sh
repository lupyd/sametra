#!/bin/sh
# Runs once when the Kubo container initialises its repo (/container-init.d).
# The RPC API listens on the compose network so the node can reach it as
# http://kubo:5001; it is never published on the host, because whoever reaches
# it controls the daemon.
set -e
ipfs config Addresses.API /ip4/0.0.0.0/tcp/5001
# Sametra serves files itself (/ipfs/CID on its own port); Kubo's gateway stays local
ipfs config Addresses.Gateway /ip4/127.0.0.1/tcp/8080
# room for files posts link to; Kubo collects unpinned blocks beyond it
ipfs config Datastore.StorageMax "${SAMETRA_IPFS_STORAGE:-20GB}"
# announce a public address if one is given, so other nodes can reach this one
if [ -n "${IPFS_ANNOUNCE:-}" ]; then
  ipfs config --json Addresses.Announce "[\"/ip4/${IPFS_ANNOUNCE}/tcp/4001\",\"/ip4/${IPFS_ANNOUNCE}/udp/4001/quic-v1\"]"
fi
