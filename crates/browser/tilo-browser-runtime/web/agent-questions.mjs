/**
 * What the person's agents on a workspace host ask them, answered on this
 * page: the page's session with the host carries the approver session
 * (`peer.answerAgentQuestions`) and the runtime shows each question on the
 * desktop's question sheet. Every page of the person's answers, as every
 * device of theirs does natively: one holding their root signs with it
 * (`identity`, when it is the person the page is in as), and one paired to
 * them signs with its own device key, so they are asked wherever they are.
 *
 * Returns whether this page answers.
 */
export function answerAgentQuestions({peer, identity}) {
  if (!peer?.answerAgentQuestions) return false;
  const root = identity?.userPublicKey && peer.userPublicKey === identity.userPublicKey ? identity : undefined;
  try {
    peer.answerAgentQuestions(root);
    return true;
  } catch (error) {
    console.warn('agent questions: this page cannot answer them', error);
    return false;
  }
}
