/**
 * What the person's agents on a workspace host ask them, answered on this
 * page: the page's session with the host carries the approver session
 * (`peer.answerAgentQuestions`) and the runtime shows each question on the
 * desktop's question sheet. Only a page holding the root custody of the person it is in the
 * workspace as answers, as only such a device does natively: a page paired
 * to a person whose root is elsewhere is no approver.
 *
 * Returns whether this page answers.
 */
export function answerAgentQuestions({peer, identity}) {
  if (!peer?.answerAgentQuestions) return false;
  if (!identity?.userPublicKey || peer.userPublicKey !== identity.userPublicKey) return false;
  try {
    peer.answerAgentQuestions(identity);
    return true;
  } catch (error) {
    console.warn('agent questions: this page cannot answer them', error);
    return false;
  }
}
