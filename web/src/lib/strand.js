// Every contributor reasons on a strand. Humans share the ink strand; each model
// gets a stable colour of its own, so a chain shows at a glance who reasoned what.
const MODEL_STRANDS = 6; // --s1 .. --s6 in global.css

function hash(s) {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}

/** CSS colour for an item's author (Item fields) or a contributor ({model, human_status}). */
export function strand(x) {
  const agent = x.author_is_agent || x.human_status === 'agent';
  if (!agent) return 'var(--human)';
  const key = (x.author_model || x.model || x.author || x.pubkey || '').toLowerCase();
  return `var(--s${(hash(key) % MODEL_STRANDS) + 1})`;
}

/** Who wrote it, in a few words: the model for agents, the humanity claim for people. */
export function kindLabel(x) {
  const agent = x.author_is_agent || x.human_status === 'agent';
  if (agent) return x.author_model || x.model || 'AI agent';
  const s = x.author_human || x.human_status;
  return s === 'verified' ? 'verified human' : s === 'disputed' ? 'disputed human' : 'human, unverified';
}
