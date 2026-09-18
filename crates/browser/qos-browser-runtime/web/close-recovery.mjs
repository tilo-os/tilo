/** Component retries can enqueue bytes which were absent from the initial close.
 * Settlement follows actual transaction completion and reports unresolved errors. */
export async function settleComponentClose(runtime,id,storage,profile,device) {
  const {writes}=JSON.parse(runtime.prepare_close(id));
  if(writes.length){
    await storage.commit(writes);
    for(const write of writes)runtime.acknowledge_storage(profile,device,BigInt(write.revision),undefined);
  }
  runtime.finish_close(id);
}
