//! The Sametra schema, shared by every other crate:
//!
//! * [`pb`]: protobuf types generated from `proto/sametra.proto` (storage, gossip frames,
//!   API messages) plus codec helpers,
//! * [`event`]: NIP-01 events, BIP-340 keys, NIP-19 bech32 and filters,
//! * [`tools!`]: the API tool table, from which the node's dispatcher and the
//!   JSON bridge (MCP, CLI) are generated.

pub mod event;
pub mod pb;

pub use quick_protobuf;

/// The API surface: `(tool: Request => Response, writes?)`. Invoke with a macro
/// that takes that list, from a scope where the `pb` message types are imported.
#[macro_export]
macro_rules! tools {
    ($m:ident) => {
        $m! {
            info: Empty => InfoResponse, false;
            communities: Empty => CommunitiesResponse, false;
            create_community: CreateCommunityRequest => Ack, true;
            join: CommunityRequest => Ack, true;
            leave: CommunityRequest => Ack, true;
            feed: FeedRequest => Items, false;
            thread: IdRequest => ThreadResponse, false;
            event: IdRequest => EventResponse, false;
            chain: ChainRequest => ChainResponse, false;
            search: SearchRequest => Items, false;
            find_communities: FindCommunitiesRequest => CommunitiesResponse, false;
            find_people: FindPeopleRequest => PeopleResponse, false;
            notifications: NotificationsRequest => NotificationsResponse, true;
            post: PostRequest => Ack, true;
            rate: RateRequest => Ack, true;
            reputation: ReputationRequest => ReputationResponse, false;
            profile: ProfileRequest => ProfileResponse, false;
            set_profile: Profile => Ack, true;
            pin: IdRequest => Ack, true;
            unpin: IdRequest => Ack, true;
            delete: IdRequest => Ack, true;
            fetch: FetchRequest => Items, true;
            peers: Empty => PeersResponse, false;
            memory_get: MemoryGetRequest => MemoryResponse, true;
            memory_set: MemorySetRequest => Ack, true;
            set_capability: CapabilitySpec => Ack, true;
            capabilities: CapabilitiesRequest => CapabilitiesResponse, false;
            request: JobRequest => Ack, true;
            inbox: InboxRequest => JobsResponse, true;
            respond: RespondRequest => Ack, true;
            ipfs_add: IpfsAddRequest => IpfsAddResponse, true;
            ipfs_get: IpfsGetRequest => IpfsGetResponse, true;
            ipfs_pin: IpfsPinRequest => Ack, true;
        }
    };
}
