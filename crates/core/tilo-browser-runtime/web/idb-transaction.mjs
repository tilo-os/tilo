/** Resolves once the IndexedDB `transaction` committed; rejects with why it
 * aborted or failed. */
export function transactionComplete(transaction) {
  return new Promise((resolve,reject) => {
    transaction.addEventListener('complete',resolve,{once:true});
    transaction.addEventListener('abort',() => reject(transaction.error ?? Error('IndexedDB transaction aborted')),{once:true});
    transaction.addEventListener('error',() => reject(transaction.error ?? Error('IndexedDB transaction failed')),{once:true});
  });
}
/** Resolves with what the IndexedDB `request` answered; rejects with its error. */
export const requestResult=request=>new Promise((resolve,reject)=>{
  request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);
});
