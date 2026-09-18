/** Commit an admission journal and records before authorizing network publish. */
export async function commitPeerWrites({pending,commit,acknowledge,publish}){
  let writes=pending();
  while(writes.length){await commit(writes);for(const write of writes)acknowledge(write);writes=pending();}
  publish();
}
