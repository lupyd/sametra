// Protobuf RPC to the local node: POST /api/<tool>, body = request message.
import { PbfReader, PbfWriter } from 'pbf';
import * as P from './proto.js';

// tool -> [request message, response message]  (mirrors `tools!` in crates/proto/src/lib.rs)
export const TOOLS = {
  info: ['Empty', 'InfoResponse'],
  communities: ['Empty', 'CommunitiesResponse'],
  create_community: ['CreateCommunityRequest', 'Ack'],
  join: ['CommunityRequest', 'Ack'],
  leave: ['CommunityRequest', 'Ack'],
  feed: ['FeedRequest', 'Items'],
  thread: ['IdRequest', 'ThreadResponse'],
  event: ['IdRequest', 'EventResponse'],
  chain: ['ChainRequest', 'ChainResponse'],
  search: ['SearchRequest', 'Items'],
  find_communities: ['FindCommunitiesRequest', 'CommunitiesResponse'],
  find_people: ['FindPeopleRequest', 'PeopleResponse'],
  notifications: ['NotificationsRequest', 'NotificationsResponse'],
  post: ['PostRequest', 'Ack'],
  rate: ['RateRequest', 'Ack'],
  reputation: ['ReputationRequest', 'ReputationResponse'],
  profile: ['ProfileRequest', 'ProfileResponse'],
  set_profile: ['Profile', 'Ack'],
  pin: ['IdRequest', 'Ack'],
  unpin: ['IdRequest', 'Ack'],
  delete: ['IdRequest', 'Ack'],
  fetch: ['FetchRequest', 'Items'],
  peers: ['Empty', 'PeersResponse'],
  memory_get: ['MemoryGetRequest', 'MemoryResponse'],
  memory_set: ['MemorySetRequest', 'Ack'],
  set_capability: ['CapabilitySpec', 'Ack'],
  capabilities: ['CapabilitiesRequest', 'CapabilitiesResponse'],
  request: ['JobRequest', 'Ack'],
  inbox: ['InboxRequest', 'JobsResponse'],
  respond: ['RespondRequest', 'Ack'],
};

export async function call(tool, req = {}) {
  const [rq, rs] = TOOLS[tool];
  const w = new PbfWriter();
  P['write' + rq](req, w);
  const res = await fetch('/api/' + tool, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-protobuf' },
    body: w.finish(),
  });
  const buf = new Uint8Array(await res.arrayBuffer());
  if (!res.ok) {
    let msg = 'HTTP ' + res.status;
    try { msg = P.readError(new PbfReader(buf)).message || msg; } catch {}
    throw new Error(msg);
  }
  return P['read' + rs](new PbfReader(buf));
}
